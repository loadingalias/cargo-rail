//! Fixed local cache scenarios. Hyperfine times Cargo directly; this process owns preparation and validation.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};

use super::{evidence, workload};
use crate::source::ContentDigest;
use crate::{RailError, RailResult};

const BUILD: &[&str] = &[
    "build",
    "--workspace",
    "--all-features",
    "--locked",
    "--offline",
    "--message-format=json",
];
const EDIT: &str = "crates/fixture-service-a/src/lib.rs";
const TOOLS: [&str; 3] = ["native", "cargo-rail", "sccache"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Scenario {
    Cold,
    Rebuild,
    Freshness,
    SourceEdit,
}

#[derive(Debug, Args)]
pub(super) struct Options {
    /// New private result directory; defaults to a retained temporary directory.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Samples per tool and scenario. One sample checks orchestration, not variability.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100))]
    runs: u32,
    /// Fixed local scenarios to execute.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "cold,rebuild,freshness,source-edit"
    )]
    scenario: Vec<Scenario>,
    /// Disable incremental compilation equally for all tools; ordinary dev builds are the default.
    #[arg(long)]
    non_incremental: bool,
    /// Exercise one sample per case without making a performance comparison.
    #[arg(long, conflicts_with = "runs")]
    smoke: bool,
    /// Cargo-Rail executable with its authenticated cache components beside it.
    #[arg(long)]
    rail: Option<PathBuf>,
}

#[derive(Serialize)]
struct Sample {
    order: usize,
    round: u32,
    tool: &'static str,
    scenario: Scenario,
    status: &'static str,
    seconds: Option<f64>,
    cargo_fresh_units: Option<u64>,
    cargo_rebuilt_units: Option<u64>,
    cargo_reported_files: Option<usize>,
    compiler_cache_hits: Option<u64>,
    compiler_cache_misses: Option<u64>,
    failed_compiler_attempts: Option<u64>,
    cache_bypass_reasons: Option<BTreeMap<String, u64>>,
    verified_restored_files: Option<usize>,
    verified_restored_bytes: Option<u64>,
    unretained_restore_outputs: Option<usize>,
    restored_by_role: Option<BTreeMap<String, u64>>,
    evidence: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    kind: &'static str,
    mode: &'static str,
    purpose: &'static str,
    profile: &'static str,
    status: &'static str,
    context: Option<Value>,
    samples: Vec<Sample>,
}

struct Run {
    root: PathBuf,
    cargo: PathBuf,
    rail: PathBuf,
    sccache: PathBuf,
    hyperfine: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    identities: BTreeMap<PathBuf, String>,
    inputs: BTreeMap<PathBuf, String>,
    socket_directory: tempfile::TempDir,
}

pub(super) fn run(mut options: Options) -> RailResult<()> {
    if !cfg!(unix) {
        return Err(RailError::message(
            "local comparison currently requires Unix socket isolation; workload preparation is portable",
        ));
    }
    if options.smoke {
        options.runs = 1;
    }
    let root = match &options.output {
        Some(path) => {
            let path = workload::new_path(path)?;
            workload::private_directory(&path)?;
            path
        }
        None => crate::utils::canonicalize_existing(
            &tempfile::Builder::new().prefix("cargo-rail-bench-").tempdir()?.keep(),
        )?,
    };
    eprintln!("Cache benchmark evidence: {}", root.display());
    let mut report = Report {
        schema_version: 1,
        kind: "cargo-rail-cache-benchmark",
        mode: "local",
        purpose: if options.smoke {
            "orchestration-smoke"
        } else {
            "measurement"
        },
        profile: if options.non_incremental {
            "dev-non-incremental"
        } else {
            "dev"
        },
        status: "preparing",
        context: None,
        samples: Vec::new(),
    };
    for round in 0..options.runs {
        for scenario in &options.scenario {
            for offset in 0..TOOLS.len() {
                let order = report.samples.len();
                report.samples.push(Sample {
                    order,
                    round,
                    tool: TOOLS[(round as usize + offset) % TOOLS.len()],
                    scenario: *scenario,
                    status: "pending",
                    seconds: None,
                    cargo_fresh_units: None,
                    cargo_rebuilt_units: None,
                    cargo_reported_files: None,
                    compiler_cache_hits: None,
                    compiler_cache_misses: None,
                    failed_compiler_attempts: None,
                    cache_bypass_reasons: None,
                    verified_restored_files: None,
                    verified_restored_bytes: None,
                    unretained_restore_outputs: None,
                    restored_by_role: None,
                    evidence: format!("sample-{order:04}"),
                });
            }
        }
    }
    write_json(&root.join("summary.json"), &report)?;
    let outcome = execute(&root, &options, &mut report);
    report.status = if outcome.is_ok() { "complete" } else { "failed" };
    write_json(&root.join("summary.json"), &report)?;
    if let Err(error) = outcome {
        fs::write(root.join("failure.txt"), error.to_string())?;
        return Err(error.context(format!(
            "cache comparison incomplete; evidence retained at {}",
            root.display()
        )));
    }
    if options.smoke {
        println!(
            "Local orchestration smoke passed ({} samples). Timings are unqualified.",
            report.samples.len()
        );
    } else {
        print_report(&report);
    }
    println!("Evidence: {}", root.display());
    Ok(())
}

fn execute(root: &Path, options: &Options, report: &mut Report) -> RailResult<()> {
    reject_ancestor_config(root)?;
    let environment = [
        "PATH",
        "HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SystemRoot",
        "COMSPEC",
        "PATHEXT",
        "USERPROFILE",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
    .collect();
    let mut run = Run {
        root: root.to_path_buf(),
        cargo: executable("cargo")?,
        rail: match &options.rail {
            Some(path) => crate::utils::canonicalize_existing(path)?,
            None => {
                let sibling = std::env::current_exe()?.with_file_name("cargo-rail");
                if sibling.is_file() {
                    sibling
                } else {
                    executable("cargo-rail")?
                }
            }
        },
        sccache: executable("sccache")?,
        hyperfine: executable("hyperfine")?,
        environment,
        identities: BTreeMap::new(),
        inputs: BTreeMap::new(),
        socket_directory: tempfile::Builder::new().prefix("rail-bench-").tempdir_in("/tmp")?,
    };
    // Resolve the toolchain once, before isolated Cargo homes remove rustup's default lookup location.
    let rustc = checked_output(
        run.command("native", &executable("rustc")?)
            .args(["--print", "sysroot"]),
    )?;
    let sysroot = PathBuf::from(String::from_utf8(rustc)?.trim());
    run.cargo = sysroot.join("bin/cargo");
    let mut search_path = vec![sysroot.join("bin")];
    search_path.extend(std::env::split_paths(
        run.environment
            .get(OsStr::new("PATH"))
            .map(OsString::as_os_str)
            .unwrap_or_default(),
    ));
    run.environment.insert(
        "PATH".into(),
        std::env::join_paths(search_path).map_err(|error| RailError::message(error.to_string()))?,
    );
    run.environment
        .insert("RUSTC".into(), sysroot.join("bin/rustc").into_os_string());
    let mut versions = BTreeMap::new();
    for (name, path) in [
        ("cargo", &run.cargo),
        ("rustc", &sysroot.join("bin/rustc")),
        ("cargo-rail", &run.rail),
        ("sccache", &run.sccache),
        ("hyperfine", &run.hyperfine),
    ] {
        versions.insert(
            name,
            String::from_utf8(checked_output(run.command("native", path).arg("--version"))?)?,
        );
        run.identities.insert(path.clone(), digest_file(path)?);
    }
    let component_parent = run
        .rail
        .parent()
        .ok_or_else(|| RailError::message("Cargo-Rail executable has no parent"))?;
    for entry in fs::read_dir(component_parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("cargo-rail-"))
            && entry.file_type()?.is_file()
            && entry.path().extension().is_none_or(|extension| extension != "d")
        {
            run.identities.insert(entry.path(), digest_file(&entry.path())?);
        }
    }
    let rustc_verbose = String::from_utf8(checked_output(
        run.command("native", &sysroot.join("bin/rustc")).arg("-vV"),
    )?)?;
    report.context = Some(json!({
        "benchmark_version": env!("CARGO_PKG_VERSION"), "workload_sha256": workload::identity(),
        "tool_versions": versions, "tool_digests": run.identities.iter().map(|(path, digest)| (path.file_name().unwrap_or_default().to_string_lossy().into_owned(), digest)).collect::<BTreeMap<_,_>>(),
        "rustc": rustc_verbose, "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "available_parallelism": std::thread::available_parallelism()?.get(), "cargo_arguments": BUILD,
    }));
    write_json(
        &root.join("provenance.json"),
        &json!({
            "versions": versions, "executables": run.identities, "rustc": rustc_verbose,
            "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
            "available_parallelism": std::thread::available_parallelism()?.get(),
            "environment": run.environment.iter().map(|(key, value)| {
            Ok((key.to_str().ok_or_else(|| RailError::message("benchmark environment name is not UTF-8"))?, value.to_str().ok_or_else(|| RailError::message("benchmark environment value is not UTF-8"))?))
        }).collect::<RailResult<BTreeMap<_, _>>>()?, "cargo_arguments": BUILD,
            "instrumentation": "Cargo JSON and Cargo-Rail action/output events; sccache daemon statistics",
            "socket": run.socket()?,
        }),
    )?;
    for tool in TOOLS {
        let home = run.home(tool);
        let directory = root.join(tool);
        workload::private_directory(&directory)?;
        workload::private_directory(&home)?;
        let workspace = run.workspace(tool);
        let mut prepare = run.command(tool, &std::env::current_exe()?);
        prepare.current_dir(root).args(["prepare", "--output"]).arg(&workspace);
        logged(&mut prepare, &directory.join("prepare"))?;
        // The correctness fixture has controlled profiles. The benchmark's ordinary case uses Cargo dev defaults.
        let manifest = workspace.join("Cargo.toml");
        let original = fs::read_to_string(&manifest)?;
        let (workspace_manifest, _) = original
            .split_once("[profile.dev]")
            .ok_or_else(|| RailError::message("bundled workload profile boundary is missing"))?;
        let suffix = if options.non_incremental {
            "[profile.dev]\nincremental = false\n"
        } else {
            ""
        };
        replace_input(&manifest, format!("{workspace_manifest}{suffix}").as_bytes())?;
        write_json(
            &directory.join("inputs.json"),
            &json!({"manifest": digest_file(&manifest)?, "lockfile": digest_file(&workspace.join("Cargo.lock"))?, "source_edit": digest_file(&workspace.join(EDIT))?}),
        )?;
        if tool == "cargo-rail" {
            logged(
                run.command(tool, &run.rail)
                    .current_dir(&workspace)
                    .args(["rail", "cache", "setup", "--local-only", "--local-dir"])
                    .arg(directory.join("cache")),
                &directory.join("setup"),
            )?;
        }
        if tool == "sccache" {
            fs::write(directory.join("sccache.toml"), "")?;
        }
        for path in workload::input_paths(&workspace) {
            run.inputs.insert(path.clone(), digest_file(&path)?);
        }
    }
    report.status = "running";
    for index in 0..report.samples.len() {
        run.revalidate_tools()?;
        for (path, digest) in &run.inputs {
            if digest_file(path)? != *digest {
                return Err(RailError::message(format!(
                    "benchmark input changed: {}",
                    path.display()
                )));
            }
        }
        report.samples[index].status = "running";
        write_json(&root.join("summary.json"), report)?;
        let sample = &mut report.samples[index];
        eprintln!(
            "Sample {} / {}: {} {:?}",
            sample.order + 1,
            options.runs as usize * options.scenario.len() * TOOLS.len(),
            sample.tool,
            sample.scenario
        );
        let outcome = run.sample(sample);
        sample.status = if outcome.is_ok() { "passed" } else { "failed" };
        write_json(&root.join("summary.json"), report)?;
        outcome?;
    }
    run.revalidate_tools()?;
    Ok(())
}

impl Run {
    fn home(&self, tool: &str) -> PathBuf {
        self.root.join(tool).join("cargo-home")
    }
    fn workspace(&self, tool: &str) -> PathBuf {
        self.root.join(tool).join("workspace")
    }
    fn socket(&self) -> RailResult<PathBuf> {
        let socket = crate::utils::canonicalize_existing(self.socket_directory.path())?.join("s");
        if socket.as_os_str().len() >= 100 {
            return Err(RailError::message("private sccache socket path is too long"));
        }
        Ok(socket)
    }
    fn command(&self, tool: &str, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(&self.environment)
            .env("CARGO_HOME", self.home(tool))
            .env("CARGO_TERM_COLOR", "never");
        command
    }
    fn build_command(&self, tool: &str, program: &Path) -> RailResult<Command> {
        for path in [&self.root, &self.home(tool), &self.workspace(tool)] {
            if crate::utils::canonicalize_existing(path)? != *path {
                return Err(RailError::message(format!(
                    "benchmark directory was redirected: {}",
                    path.display()
                )));
            }
        }
        let mut command = self.command(tool, program);
        command.current_dir(self.workspace(tool));
        if tool == "sccache" {
            command
                .env("RUSTC_WRAPPER", &self.sccache)
                .env("SCCACHE_CONF", self.root.join(tool).join("sccache.toml"))
                .env("SCCACHE_CACHED_CONF", self.root.join(tool).join("sccache-cached.toml"))
                .env("SCCACHE_SERVER_UDS", self.socket()?)
                .env("SCCACHE_DIR", self.root.join(tool).join("cache"));
        }
        Ok(command)
    }
    fn revalidate_tools(&self) -> RailResult<()> {
        for (path, digest) in &self.identities {
            if digest_file(path)? != *digest {
                return Err(RailError::message(format!(
                    "benchmark executable changed during the run: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
    fn sample(&self, sample: &mut Sample) -> RailResult<()> {
        let evidence = self.root.join(&sample.evidence);
        workload::private_directory(&evidence)?;
        let tool = sample.tool;
        let workspace = self.workspace(tool);
        let edit = workspace.join(EDIT);
        let original = fs::read_to_string(&edit)?;
        if tool == "cargo-rail" {
            logged(
                self.command(tool, &self.rail)
                    .current_dir(&workspace)
                    .args(["rail", "cache", "clean", "--scope", "all"]),
                &evidence.join("clean-cache"),
            )?;
            logged(
                self.command(tool, &self.rail)
                    .current_dir(&workspace)
                    .args(["rail", "cache", "setup", "--local-only", "--local-dir"])
                    .arg(self.root.join(tool).join("cache")),
                &evidence.join("repair-cache"),
            )?;
        }
        logged(
            self.build_command(tool, &self.cargo)?.args(["clean"]),
            &evidence.join("clean-target"),
        )?;
        if tool == "sccache" {
            let cache = self.root.join(tool).join("cache");
            if cache.try_exists()? {
                if crate::utils::canonicalize_existing(&cache)? != cache {
                    return Err(RailError::message("private sccache cache was redirected"));
                }
                fs::remove_dir_all(cache)?;
            }
            logged(
                self.build_command(tool, &self.sccache)?.arg("--start-server"),
                &evidence.join("start-server"),
            )?;
        }
        let result = self.measure(sample, &evidence, &original);
        let restore = replace_input(&edit, original.as_bytes());
        let stop = if tool == "sccache" {
            logged(
                self.build_command(tool, &self.sccache)?.arg("--stop-server"),
                &evidence.join("stop-server"),
            )
        } else {
            Ok(())
        };
        result.and(restore).and(stop)
    }
    fn rail_usage(&self, evidence: &Path, name: &str) -> RailResult<(u64, u64)> {
        let stem = evidence.join(name);
        logged(
            self.command("cargo-rail", &self.rail)
                .current_dir(self.workspace("cargo-rail"))
                .args(["rail", "cache", "status", "--scope", "local", "-f", "json"]),
            &stem,
        )?;
        let status: Value = serde_json::from_slice(&fs::read(stem.with_extension("stdout"))?)?;
        let usage = &status["status"]["installation"]["usage"];
        Ok((
            usage["hits"]
                .as_u64()
                .ok_or_else(|| RailError::message("cache status omitted hit count"))?,
            usage["misses"]
                .as_u64()
                .ok_or_else(|| RailError::message("cache status omitted miss count"))?,
        ))
    }
    fn measure(&self, sample: &mut Sample, evidence: &Path, original: &str) -> RailResult<()> {
        let tool = sample.tool;
        let workspace = self.workspace(tool);
        let seed_directory = evidence.join("seed-events");
        let mut seed = None;
        if sample.scenario != Scenario::Cold {
            let mut seed_command = self.build_command(tool, &self.cargo)?;
            seed_command.args(BUILD);
            if tool == "cargo-rail" {
                configure_evidence(&mut seed_command, &seed_directory)?;
            }
            let before = if tool == "cargo-rail" {
                Some(self.rail_usage(evidence, "seed-usage-before")?)
            } else {
                None
            };
            logged(&mut seed_command, &evidence.join("seed"))?;
            validate_cargo(&evidence.join("seed.stdout"), &workspace, evidence, "seed", 119)?;
            if tool == "cargo-rail" {
                let captured = evidence::read(&seed_directory, &workspace, None)?;
                require_usage(before, self.rail_usage(evidence, "seed-usage-after")?, &captured)?;
                seed = Some(captured);
                write_json(&evidence.join("seed-cache-statistics.json"), &seed)?;
            }
            if sample.scenario == Scenario::Rebuild {
                logged(
                    self.build_command(tool, &self.cargo)?.arg("clean"),
                    &evidence.join("clean-seeded-target"),
                )?;
            }
            if sample.scenario == Scenario::SourceEdit {
                let edited = original.replace("response().id + 1", "response().id + 2");
                if edited == original || edited.len() != original.len() {
                    return Err(RailError::message(
                        "bundled source edit is not the expected same-size mutation",
                    ));
                }
                replace_input(&workspace.join(EDIT), edited.as_bytes())?;
            }
        }
        if matches!(sample.scenario, Scenario::Cold | Scenario::Rebuild) {
            match fs::read_dir(workspace.join("target")) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        return Err(RailError::message("empty target scenario retained target entries"));
                    }
                }
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
        }
        let inputs = workload::input_paths(&workspace)
            .into_iter()
            .map(|path| Ok((path.clone(), digest_file(&path)?)))
            .collect::<RailResult<BTreeMap<_, _>>>()?;
        write_json(&evidence.join("source-inputs.json"), &inputs)?;
        let event_directory = evidence.join("events");
        if tool == "sccache" {
            logged(
                self.build_command(tool, &self.sccache)?.arg("--zero-stats"),
                &evidence.join("zero-stats"),
            )?;
        }
        let command_line = std::iter::once(self.cargo.as_os_str())
            .chain(BUILD.iter().map(OsStr::new))
            .map(quote_argument)
            .collect::<RailResult<Vec<_>>>()?
            .join(" ");
        let mut timing = self.build_command(tool, &self.hyperfine)?;
        timing
            .args([
                "--shell=none",
                "--runs=1",
                "--style=none",
                "--output=inherit",
                "--export-json",
            ])
            .arg(evidence.join("hyperfine.json"))
            .arg(&command_line);
        if tool == "cargo-rail" {
            configure_evidence(&mut timing, &event_directory)?;
        }
        write_json(
            &evidence.join("command.json"),
            &json!({"command": command_line, "working_directory": workspace, "scenario": sample.scenario, "target_before": if matches!(sample.scenario, Scenario::Cold | Scenario::Rebuild) {"empty"} else {"seeded"}, "result_cache_before": if sample.scenario == Scenario::Cold {"empty"} else {"seeded"}}),
        )?;
        let before = if tool == "cargo-rail" {
            Some(self.rail_usage(evidence, "usage-before")?)
        } else {
            None
        };
        logged(&mut timing, &evidence.join("cargo"))?;
        for (path, digest) in &inputs {
            if digest_file(path)? != *digest {
                return Err(RailError::message(format!(
                    "benchmark input changed during measurement: {}",
                    path.display()
                )));
            }
        }
        sample.seconds = Some(read_timing(&evidence.join("hyperfine.json"))?);
        let (fresh, rebuilt, files) = validate_cargo(
            &evidence.join("cargo.stdout"),
            &workspace,
            evidence,
            "measured",
            if sample.scenario == Scenario::SourceEdit {
                120
            } else {
                119
            },
        )?;
        sample.cargo_fresh_units = Some(fresh);
        sample.cargo_rebuilt_units = Some(rebuilt);
        sample.cargo_reported_files = Some(files);
        if matches!(sample.scenario, Scenario::Cold | Scenario::Rebuild) && fresh != 0 {
            return Err(RailError::message(
                "empty target sample unexpectedly retained Cargo-fresh units",
            ));
        }
        if sample.scenario == Scenario::Freshness && rebuilt != 0 {
            return Err(RailError::message("freshness sample unexpectedly rebuilt Cargo units"));
        }
        if sample.scenario == Scenario::SourceEdit && rebuilt == 0 {
            return Err(RailError::message("source edit did not rebuild any Cargo units"));
        }
        match tool {
            "cargo-rail" => {
                let measurements = evidence::read(&event_directory, &workspace, seed.as_ref())?;
                require_usage(before, self.rail_usage(evidence, "usage-after")?, &measurements)?;
                write_json(&evidence.join("cache-statistics.json"), &measurements)?;
                sample.cache_bypass_reasons = Some(measurements.bypass_reasons);
                sample.compiler_cache_hits = Some(measurements.hits);
                sample.compiler_cache_misses = Some(measurements.misses);
                sample.verified_restored_files = Some(measurements.restored_files);
                sample.unretained_restore_outputs = Some(measurements.unretained_restore_outputs);
                sample.verified_restored_bytes = Some(measurements.restored_bytes);
                sample.restored_by_role = Some(measurements.restored_by_role);
                if rebuilt > 0 && measurements.events == 0 {
                    return Err(RailError::message("Cargo-Rail recorded no compiler events"));
                }
            }
            "sccache" => {
                let stats = checked_output(self.build_command(tool, &self.sccache)?.args([
                    "--show-stats",
                    "--stats-format",
                    "json",
                ]))?;
                fs::write(evidence.join("cache-statistics.json"), &stats)?;
                let value: Value = serde_json::from_slice(&stats)?;
                let requests = value["stats"]["compile_requests"]
                    .as_u64()
                    .ok_or_else(|| RailError::message("sccache omitted compiler request count"))?;
                if rebuilt > 0 && requests == 0 {
                    return Err(RailError::message("sccache recorded no compiler requests"));
                }
                sample.cache_bypass_reasons = Some(serde_json::from_value(value["stats"]["not_cached"].clone())?);
                let (hits, misses, failures) = sccache_counts(&value, &self.root.join(tool).join("cache"))?;
                sample.compiler_cache_hits = Some(hits);
                sample.compiler_cache_misses = Some(misses);
                sample.failed_compiler_attempts = Some(failures);
            }
            _ => {}
        }
        if sample.scenario == Scenario::Cold && sample.compiler_cache_hits.is_some_and(|hits| hits != 0) {
            return Err(RailError::message(
                "cold sample unexpectedly restored cached compiler actions",
            ));
        }
        Ok(())
    }
}

fn require_usage(before: Option<(u64, u64)>, after: (u64, u64), evidence: &evidence::Evidence) -> RailResult<()> {
    let before = before.ok_or_else(|| RailError::message("compiler evidence has no initial usage binding"))?;
    if after.0.checked_sub(before.0) != Some(evidence.hits) || after.1.checked_sub(before.1) != Some(evidence.misses) {
        return Err(RailError::message(
            "compiler event census disagrees with recorded cache outcomes",
        ));
    }
    Ok(())
}

fn configure_evidence(command: &mut Command, directory: &Path) -> RailResult<()> {
    workload::private_directory(directory)?;
    command
        .env(
            crate::compiler::invocation::CACHE_CONTROL_ENV,
            crate::compiler::invocation::BENCH_COVERAGE_CACHE_CONTROL,
        )
        .env(crate::compiler::native_cache::BENCH_COVERAGE_DIRECTORY_ENV, directory);
    Ok(())
}

fn checked_output(command: &mut Command) -> RailResult<Vec<u8>> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "{} failed: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

fn logged(command: &mut Command, stem: &Path) -> RailResult<()> {
    command
        .stdout(fs::File::create(stem.with_extension("stdout"))?)
        .stderr(fs::File::create(stem.with_extension("stderr"))?);
    let status = command.status()?;
    if !status.success() {
        return Err(RailError::message(format!(
            "{} exited with {status}; see {}",
            command.get_program().to_string_lossy(),
            stem.with_extension("stderr").display()
        )));
    }
    Ok(())
}

fn executable(name: &str) -> RailResult<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join(name);
        if path.is_file() {
            return Ok(std::path::absolute(path)?);
        }
    }
    Err(RailError::message(format!(
        "required benchmark tool is not on PATH: {name}"
    )))
}

fn reject_ancestor_config(root: &Path) -> RailResult<()> {
    for ancestor in root.ancestors() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "rust-toolchain",
            "rust-toolchain.toml",
        ] {
            if ancestor.join(name).try_exists()? {
                return Err(RailError::message(format!(
                    "benchmark isolation would inherit {}; choose an output directory outside that tree",
                    ancestor.join(name).display()
                )));
            }
        }
    }
    Ok(())
}

fn replace_input(path: &Path, bytes: &[u8]) -> RailResult<()> {
    if crate::utils::canonicalize_existing(path)? != path || !fs::symlink_metadata(path)?.is_file() {
        return Err(RailError::message(format!(
            "benchmark input was redirected: {}",
            path.display()
        )));
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn digest_file(path: &Path) -> RailResult<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(RailError::message(format!(
            "benchmark input is not a regular file: {}",
            path.display()
        )));
    }
    Ok(ContentDigest::sha256(&fs::read(path)?).to_string())
}

fn quote_argument(argument: &OsStr) -> RailResult<String> {
    let argument = argument
        .to_str()
        .ok_or_else(|| RailError::message("Hyperfine arguments must be UTF-8"))?;
    Ok(format!("'{}'", argument.replace('\'', "'\\''")))
}

fn write_json(path: &Path, value: &impl Serialize) -> RailResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("report has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.persist(path).map_err(|error| RailError::from(error.error))?;
    Ok(())
}

fn read_timing(path: &Path) -> RailResult<f64> {
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    let results = value["results"]
        .as_array()
        .filter(|results| results.len() == 1)
        .ok_or_else(|| RailError::message("Hyperfine did not return exactly one command"))?;
    let times = results[0]["times"]
        .as_array()
        .filter(|times| times.len() == 1)
        .ok_or_else(|| RailError::message("Hyperfine did not return exactly one sample"))?;
    if results[0]["exit_codes"] != json!([0]) {
        return Err(RailError::message("Hyperfine sample did not exit successfully"));
    }
    times[0]
        .as_f64()
        .filter(|time| time.is_finite() && *time > 0.0)
        .ok_or_else(|| RailError::message("Hyperfine sample has no positive finite elapsed time"))
}

fn validate_cargo(
    log: &Path,
    workspace: &Path,
    evidence: &Path,
    name: &str,
    expected: u64,
) -> RailResult<(u64, u64, usize)> {
    let mut fresh = 0;
    let mut rebuilt = 0;
    let mut finished = false;
    let mut files = BTreeSet::new();
    for line in std::io::BufReader::new(fs::File::open(log)?).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        match value["reason"].as_str() {
            Some("compiler-artifact") => {
                match value["fresh"].as_bool() {
                    Some(true) => fresh += 1,
                    Some(false) => rebuilt += 1,
                    None => return Err(RailError::message("Cargo artifact has no freshness state")),
                }
                for path in value["filenames"]
                    .as_array()
                    .ok_or_else(|| RailError::message("Cargo artifact has no filenames"))?
                {
                    files.insert(PathBuf::from(
                        path.as_str()
                            .ok_or_else(|| RailError::message("Cargo artifact filename is not text"))?,
                    ));
                }
            }
            Some("build-finished") if value["success"] == true && !finished => finished = true,
            Some("compiler-message" | "build-script-executed") => {}
            _ => return Err(RailError::message("unexpected or unsuccessful Cargo JSON message")),
        }
    }
    if !finished || files.is_empty() {
        return Err(RailError::message(
            "Cargo did not finish with a nonempty artifact inventory",
        ));
    }
    let target = workspace.join("target");
    let mut inventory = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(&target)
            .map_err(|_| RailError::message("Cargo reported an artifact outside the owned target directory"))?;
        let metadata = fs::symlink_metadata(file)?;
        if !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || crate::utils::canonicalize_existing(file)? != *file
        {
            return Err(RailError::message("Cargo artifact is not a contained regular file"));
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        inventory.push(json!({"path": relative, "sha256": digest_file(file)?, "bytes": metadata.len(), "mode": mode}));
    }
    write_json(&evidence.join(format!("{name}-cargo-files.json")), &inventory)?;
    let binary = target.join("debug/fixture-cli");
    let output = Command::new(&binary).env_clear().current_dir(workspace).output()?;
    fs::write(evidence.join(format!("{name}-behavior.stdout")), &output.stdout)?;
    fs::write(evidence.join(format!("{name}-behavior.stderr")), &output.stderr)?;
    if !output.status.success() || output.stdout != format!("{expected}\n").as_bytes() || !output.stderr.is_empty() {
        return Err(RailError::message(
            "built fixture behavior disagrees with the uncached workload contract",
        ));
    }
    Ok((fresh, rebuilt, files.len()))
}

fn sccache_counts(value: &Value, cache: &Path) -> RailResult<(u64, u64, u64)> {
    if value["cache_location"] != format!("Local disk: {cache:?}") {
        return Err(RailError::message(
            "sccache did not use the benchmark's private local disk cache",
        ));
    }
    let stats = &value["stats"];
    for field in [
        "cache_timeouts",
        "cache_read_errors",
        "cache_write_errors",
        "dist_errors",
    ] {
        if stats[field].as_u64() != Some(0) {
            return Err(RailError::message(format!(
                "sccache reported errors or omitted {field}"
            )));
        }
    }
    if stats["dist_compiles"]
        .as_object()
        .is_none_or(|values| !values.is_empty())
    {
        return Err(RailError::message(
            "local sccache unexpectedly used distributed compilation",
        ));
    }
    let count = |field: &str| -> RailResult<u64> {
        stats[field]["counts"]
            .as_object()
            .ok_or_else(|| RailError::message(format!("sccache omitted {field} counts")))?
            .values()
            .try_fold(0u64, |total, value| {
                value
                    .as_u64()
                    .and_then(|count| total.checked_add(count))
                    .ok_or_else(|| RailError::message("invalid sccache count"))
            })
    };
    if count("cache_errors")? != 0 {
        return Err(RailError::message("sccache reported cache errors"));
    }
    let failures = stats["compile_fails"]
        .as_u64()
        .ok_or_else(|| RailError::message("sccache omitted failed compiler attempts"))?;
    Ok((count("cache_hits")?, count("cache_misses")?, failures))
}

fn print_report(report: &Report) {
    println!(
        "Local cache comparison ({}) — seconds, median [min, max]",
        report.profile
    );
    for scenario in [
        Scenario::Cold,
        Scenario::Rebuild,
        Scenario::Freshness,
        Scenario::SourceEdit,
    ] {
        for tool in TOOLS {
            let rows = report
                .samples
                .iter()
                .filter(|row| row.tool == tool && row.scenario == scenario)
                .collect::<Vec<_>>();
            let mut times = rows.iter().filter_map(|row| row.seconds).collect::<Vec<_>>();
            if times.is_empty() {
                continue;
            }
            times.sort_by(f64::total_cmp);
            let middle = times.len() / 2;
            let median = if times.len() % 2 == 0 {
                (times[middle - 1] + times[middle]) / 2.0
            } else {
                times[middle]
            };
            let hits = rows.iter().filter_map(|row| row.compiler_cache_hits).sum::<u64>();
            println!(
                "{scenario:?} {tool}: {median:.3} [{:.3}, {:.3}], n={}, compiler hits={}",
                times[0],
                times[times.len() - 1],
                times.len(),
                if tool == "native" {
                    "n/a".to_string()
                } else {
                    hits.to_string()
                }
            );
        }
    }
    for row in &report.samples {
        if let Some(files) = row.verified_restored_files {
            println!(
                "{:?} cargo-rail round {}: {} verified retained restored files, {} bytes, {} unretained restore outputs, roles={:?}",
                row.scenario,
                row.round + 1,
                files,
                row.verified_restored_bytes.unwrap_or_default(),
                row.unretained_restore_outputs.unwrap_or_default(),
                row.restored_by_role.as_ref().unwrap_or(&BTreeMap::new())
            );
        }
    }
    println!(
        "Cargo-Rail restored files are verified against admitted seed outputs. sccache file attribution is unavailable; no artifact-coverage comparison is claimed."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_requires_one_successful_finite_sample() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("timing.json");
        fs::write(&path, r#"{"results":[{"times":[0.125],"exit_codes":[0]}]}"#)?;
        anyhow::ensure!(read_timing(&path)? == 0.125);
        for invalid in [
            r#"{"results":[{"times":[0.125,0.25],"exit_codes":[0,0]}]}"#,
            r#"{"results":[{"times":[0.125],"exit_codes":[1]}]}"#,
            r#"{"results":[{"times":[0],"exit_codes":[0]}]}"#,
            r#"{"results":[{"times":[0.125]}]}"#,
            r#"{"results":[]}"#,
        ] {
            fs::write(&path, invalid)?;
            anyhow::ensure!(read_timing(&path).is_err(), "accepted invalid timing: {invalid}");
        }
        Ok(())
    }

    #[test]
    fn sccache_statistics_require_owned_local_storage_and_complete_error_counters() -> anyhow::Result<()> {
        let cache = Path::new("/private/benchmark/cache");
        let valid = json!({"cache_location": "Local disk: \"/private/benchmark/cache\"", "stats": {
            "cache_hits": {"counts": {"Rust": 3, "C/C++": 2}}, "cache_misses": {"counts": {"Rust": 1}},
            "cache_errors": {"counts": {}}, "cache_timeouts": 0, "cache_read_errors": 0,
            "cache_write_errors": 0, "compile_fails": 0, "dist_errors": 0, "dist_compiles": {}
        }});
        anyhow::ensure!(sccache_counts(&valid, cache)? == (5, 1, 0));
        let mut probes = valid.clone();
        probes["stats"]["compile_fails"] = json!(2);
        anyhow::ensure!(sccache_counts(&probes, cache)? == (5, 1, 2));
        let mut remote = valid.clone();
        remote["cache_location"] = json!("S3 bucket: shared");
        anyhow::ensure!(sccache_counts(&remote, cache).is_err());
        let mut incomplete = valid.clone();
        incomplete["stats"]
            .as_object_mut()
            .expect("object")
            .remove("cache_write_errors");
        anyhow::ensure!(sccache_counts(&incomplete, cache).is_err());
        let mut error = valid.clone();
        error["stats"]["cache_errors"]["counts"]["Rust"] = json!(1);
        anyhow::ensure!(sccache_counts(&error, cache).is_err());
        let mut worker = valid;
        worker["stats"]["dist_compiles"]["worker"] = json!(1);
        anyhow::ensure!(sccache_counts(&worker, cache).is_err());
        Ok(())
    }

    #[test]
    fn output_refuses_ancestor_configuration_before_preparation() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join(".cargo"))?;
        fs::write(
            directory.path().join(".cargo/config.toml"),
            "[build]\nrustflags = ['--cfg=foreign']\n",
        )?;
        let output = directory.path().join("run");
        fs::create_dir(&output)?;
        let error = reject_ancestor_config(&output).expect_err("ancestor policy must be refused");
        anyhow::ensure!(error.to_string().contains(".cargo/config.toml"));
        anyhow::ensure!(!output.join("native").exists());
        Ok(())
    }
}
