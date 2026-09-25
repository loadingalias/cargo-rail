//! Portable planning-evidence producer for one ordinary Cargo build.
//!
//! Cargo runs a staged recorder as `RUSTC_WRAPPER`. The recorder writes each workspace-local
//! compiler invocation and then runs the compiler unchanged, so outputs and Cargo freshness are
//! the same as in an unrecorded build. After Cargo finishes, the producer correlates Cargo's
//! artifact messages with the recorded invocations, reads the dep-info files that rustc wrote,
//! and reads the rerun declarations of each executed build script. That is the input set Cargo
//! itself uses to decide freshness. A unit that Cargo reports as fresh was not compiled in this run;
//! its dep-info from the earlier compile is still current, because Cargo checked exactly those
//! inputs, so the producer reads it from the location Cargo's artifact names determine. A unit
//! whose inputs cannot be located leaves its work item incomplete instead of guessed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use cargo_metadata::{Message, Package};
use serde::{Deserialize, Serialize};

use super::evidence::{
    EvidenceProvider, ObservedDirectory, ObservedInput, ObservedWorkEvidence, PlanningEvidenceManifest,
    load_signed_manifest, secret_capability_name, sign_manifest,
};
use crate::compiler::view_context;
use crate::error::{RailError, RailResult, ResultExt as _};
use crate::workspace::WorkspaceContext;

pub(crate) const RECORD_DIRECTORY: &str = "CARGO_RAIL_EVIDENCE_RECORD_DIRECTORY";
pub(crate) const RECORD_SOURCE_ROOT: &str = "CARGO_RAIL_EVIDENCE_SOURCE_ROOT";
pub(crate) const RECORD_INNER_WRAPPER: &str = "CARGO_RAIL_EVIDENCE_INNER_WRAPPER";

const RECORD_VERSION: u32 = 1;
const RECORDABLE_WORK: [&str; 3] = ["cargo.build", "cargo.clippy", "cargo.test"];
const PROVIDER_CAPABILITIES: [&str; 5] = [
    "build_script_reads",
    "compiler_reads",
    "process_domain",
    "proc_macro_reads",
    "rustc_dep_info",
];
const MAX_REPORTED_UNITS: usize = 8;

/// One workspace-local compiler invocation written by the recorder before the compiler runs.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedInvocation {
    version: u32,
    /// `None` when a path or argument is not valid UTF-8 and cannot be interpreted exactly.
    current_dir: Option<String>,
    manifest_dir: Option<String>,
    arguments: Option<Vec<String>>,
}

/// Record one compiler invocation when it compiles a workspace-local unit.
///
/// `arguments` is Cargo's wrapper argv after the recorder: an optional workspace wrapper,
/// the compiler, and the compiler arguments.
pub(crate) fn record_compiler_invocation(arguments: &[OsString]) -> RailResult<()> {
    if !arguments
        .iter()
        .any(|argument| argument == "--crate-name" || argument.to_string_lossy().starts_with("--crate-name="))
    {
        return Ok(());
    }
    let (Some(directory), Some(source_root)) = (
        view_context::context_var_os(RECORD_DIRECTORY),
        view_context::context_var_os(RECORD_SOURCE_ROOT),
    ) else {
        return Err(RailError::message("evidence recorder context is incomplete"));
    };
    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return Ok(());
    };
    let manifest_dir = crate::utils::canonicalize_existing(Path::new(&manifest_dir)).map_err(|error| {
        RailError::message(format!(
            "cannot resolve compiled package directory '{}': {error}",
            Path::new(&manifest_dir).display()
        ))
    })?;
    if !manifest_dir.starts_with(&source_root) {
        return Ok(());
    }
    let current_dir = std::env::current_dir()?;
    let record = RecordedInvocation {
        version: RECORD_VERSION,
        current_dir: current_dir.to_str().map(str::to_string),
        manifest_dir: manifest_dir.to_str().map(str::to_string),
        arguments: arguments
            .iter()
            .map(|argument| argument.to_str().map(str::to_string))
            .collect(),
    };
    let mut file = tempfile::Builder::new()
        .prefix("unit-")
        .suffix(".json")
        .tempfile_in(&directory)?;
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.keep().map_err(|error| RailError::message(error.to_string()))?;
    Ok(())
}

/// Planning bindings shared with `cargo rail plan`.
pub(crate) struct RecordBindings {
    pub(crate) cargo_configuration_identity: String,
    pub(crate) toolchain_identity: String,
    pub(crate) target_identity: String,
}

/// Result of recording one work item.
#[derive(Debug, Serialize)]
pub(crate) struct RecordSummary {
    pub(crate) work: String,
    pub(crate) output: PathBuf,
    pub(crate) identity: String,
    pub(crate) source_base: String,
    pub(crate) complete: bool,
    pub(crate) bypasses: Vec<String>,
    pub(crate) units: usize,
    pub(crate) inputs: usize,
    pub(crate) directories: usize,
    /// A bounded sample of units whose inputs were not observed in this run.
    pub(crate) unobserved_units: Vec<String>,
    pub(crate) bytes: u64,
}

/// Run one Cargo build under the recorder and write or merge its planning evidence.
pub(crate) fn record_planning_evidence(
    ctx: &WorkspaceContext,
    work: &str,
    output: &Path,
    cargo_arguments: &[String],
    bindings: RecordBindings,
) -> RailResult<RecordSummary> {
    validate_request(work, cargo_arguments)?;
    let git = ctx.git()?.git();
    require_clean_tracked_worktree(ctx)?;
    let source_base = git.head_commit()?;
    let cargo_identity = super::work::planning_cargo_identity(ctx)?;
    let toolchain = ctx.toolchain_identity()?;

    let staging = tempfile::Builder::new()
        .prefix("cargo-rail-evidence-")
        .tempdir()
        .with_context(|| "creating the evidence recorder directory".to_string())?;
    let records = staging.path().join("records");
    fs::create_dir(&records)?;
    let executable = crate::utils::current_executable()
        .with_context(|| "locating cargo-rail to stage the evidence recorder".to_string())?;
    let recorder = view_context::stage_evidence_recorder(&executable, staging.path())?;
    let source_root = crate::utils::canonicalize_existing(ctx.planning_authority_source_root())?;
    let mut values = BTreeMap::from([
        (RECORD_DIRECTORY, records.as_os_str()),
        (RECORD_SOURCE_ROOT, source_root.as_os_str()),
    ]);
    if let Some(inner) = toolchain.rustc_wrapper_program() {
        values.insert(RECORD_INNER_WRAPPER, inner);
    }
    let _context = view_context::begin_context(staging.path(), &values)?;

    let messages = run_recorded_cargo(ctx, toolchain.cargo_program(), &recorder, cargo_arguments)?;
    require_clean_tracked_worktree(ctx).map_err(|error| {
        error.context("the recorded build changed tracked files, so its inputs no longer describe HEAD")
    })?;

    let observation = Observation::collect(ctx, &messages, &load_records(&records)?)?;
    let evidence = observation.resolve(ctx, &source_base)?;
    let summary_units = observation.units;
    let unobserved_units = observation
        .unobserved
        .iter()
        .take(MAX_REPORTED_UNITS)
        .cloned()
        .collect();

    let mut manifest = PlanningEvidenceManifest {
        planning_evidence_version: super::evidence::EVIDENCE_VERSION,
        identity: String::new(),
        provider: EvidenceProvider {
            identity: format!("cargo-rail {} rustc dep-info recorder", env!("CARGO_PKG_VERSION")),
            capabilities: PROVIDER_CAPABILITIES.iter().map(|value| (*value).to_string()).collect(),
        },
        source_base: source_base.clone(),
        cargo_identity,
        cargo_configuration_identity: bindings.cargo_configuration_identity,
        toolchain_identity: bindings.toolchain_identity,
        target_identity: bindings.target_identity,
        platform: super::work::host_platform(),
        environment: evidence.environment.iter().cloned().collect(),
        base_model: super::work::portable_base_model(ctx)?,
        work: BTreeMap::from([(work.to_string(), evidence.work.clone())]),
    };
    if let Some(existing) = load_existing(output)? {
        merge_existing(&mut manifest, existing, work, output)?;
    }
    let manifest = sign_manifest(manifest).map_err(|(code, description)| {
        RailError::message(format!("recorded planning evidence is invalid ({code}): {description}"))
    })?;
    let bytes = write_manifest(output, &manifest)?;
    Ok(RecordSummary {
        work: work.to_string(),
        output: output.to_path_buf(),
        identity: manifest.identity,
        source_base,
        complete: evidence.work.complete,
        bypasses: evidence.work.bypasses.clone(),
        units: summary_units,
        inputs: evidence.work.inputs.len(),
        directories: evidence.work.directories.len(),
        unobserved_units,
        bytes,
    })
}

fn validate_request(work: &str, cargo_arguments: &[String]) -> RailResult<()> {
    if !RECORDABLE_WORK.contains(&work) {
        return Err(RailError::with_help(
            format!("planning evidence cannot be recorded for '{work}'"),
            format!(
                "record one of {}; other Cargo work keeps widening without evidence",
                RECORDABLE_WORK.join(", ")
            ),
        ));
    }
    let Some(subcommand) = cargo_arguments.first() else {
        return Err(RailError::with_help(
            "planning evidence needs the Cargo build to record",
            "pass the Cargo arguments after `--`, for example `-- test --workspace --no-run`",
        ));
    };
    if subcommand.starts_with('+') || subcommand.starts_with('-') {
        return Err(RailError::with_help(
            format!("the recorded Cargo command must start with its subcommand, not '{subcommand}'"),
            "select the toolchain with rust-toolchain.toml so plans and evidence bind the same toolchain",
        ));
    }
    for argument in cargo_arguments.iter().take_while(|argument| *argument != "--") {
        if ["--message-format", "--manifest-path"]
            .iter()
            .any(|option| argument == option || argument.starts_with(&format!("{option}=")))
        {
            return Err(RailError::message(format!(
                "the recorded Cargo command cannot set '{argument}'; Cargo-Rail owns the message format and workspace"
            )));
        }
    }
    Ok(())
}

fn require_clean_tracked_worktree(ctx: &WorkspaceContext) -> RailResult<()> {
    let git = ctx.git()?;
    let output = git
        .git()
        .git_cmd()
        .current_dir(git.repo_root())
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=none",
        ])
        .output()
        .with_context(|| "reading tracked worktree status".to_string())?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let changed = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| record.len() > 3)
        .map(|record| String::from_utf8_lossy(&record[3..]).into_owned())
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return Ok(());
    }
    Err(RailError::with_help(
        format!(
            "planning evidence describes HEAD, but {} tracked file(s) differ from it, starting with '{}'",
            changed.len(),
            changed[0]
        ),
        "commit or restore tracked changes before recording evidence; untracked files are allowed",
    ))
}

/// Cargo messages retained from one recorded build.
struct CargoMessages {
    artifacts: Vec<cargo_metadata::Artifact>,
    build_scripts: Vec<cargo_metadata::BuildScript>,
}

fn run_recorded_cargo(
    ctx: &WorkspaceContext,
    cargo: &OsStr,
    recorder: &Path,
    cargo_arguments: &[String],
) -> RailResult<CargoMessages> {
    let mut command = Command::new(cargo);
    crate::compiler::native_cache::remove_observation_environment(&mut command);
    command
        .current_dir(ctx.workspace_root())
        .env("RUSTC_WRAPPER", recorder)
        .arg(&cargo_arguments[0])
        .arg("--message-format=json-render-diagnostics")
        .args(&cargo_arguments[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .with_context(|| format!("starting `cargo {}`", cargo_arguments.join(" ")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RailError::message("Cargo standard output is unavailable"))?;
    let mut messages = CargoMessages {
        artifacts: Vec::new(),
        build_scripts: Vec::new(),
    };
    let mut stderr = std::io::stderr().lock();
    let mut malformed = None;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        match serde_json::from_str::<Message>(&line) {
            Ok(Message::CompilerArtifact(artifact)) => messages.artifacts.push(artifact),
            Ok(Message::BuildScriptExecuted(script)) => messages.build_scripts.push(script),
            Ok(_) => {}
            Err(_) if line.starts_with('{') && line.contains("\"reason\"") => {
                malformed.get_or_insert(line);
            }
            // Test and other program output is not a Cargo message; keep it visible.
            Err(_) => writeln!(stderr, "{line}")?,
        }
    }
    drop(stderr);
    let status = child.wait()?;
    if !status.success() {
        eprintln!("cargo-rail: the recorded Cargo build failed; no planning evidence was written");
        return Err(RailError::ExitWithCode {
            code: status.code().unwrap_or(1),
        });
    }
    if let Some(line) = malformed {
        return Err(RailError::message(format!(
            "Cargo emitted a message this Cargo-Rail cannot read: {}",
            line.chars().take(200).collect::<String>()
        )));
    }
    Ok(messages)
}

fn load_records(directory: &Path) -> RailResult<Vec<RecordedInvocation>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let record: RecordedInvocation = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|error| RailError::message(format!("invalid evidence record '{}': {error}", path.display())))?;
        if record.version != RECORD_VERSION {
            return Err(RailError::message("evidence record has an unsupported version"));
        }
        records.push(record);
    }
    Ok(records)
}

/// Rustc arguments that locate one unit and its dep-info file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct UnitKey {
    manifest_dir: PathBuf,
    /// Root source file; a binary and a library can share one crate name in test mode.
    source: PathBuf,
    crate_name: String,
    test: bool,
    /// Empty for test harness units, whose crate type Cargo leaves to `--test`.
    crate_types: BTreeSet<String>,
}

struct ParsedInvocation {
    key: UnitKey,
    current_dir: PathBuf,
    dep_info: Option<PathBuf>,
}

fn parse_invocation(record: &RecordedInvocation) -> Option<ParsedInvocation> {
    let arguments = record.arguments.as_ref()?;
    let current_dir = PathBuf::from(record.current_dir.as_ref()?);
    let mut crate_name = None;
    let mut crate_types = BTreeSet::new();
    let mut out_dir = None;
    let mut extra_filename = String::new();
    let mut emit = Vec::new();
    let mut test = false;
    let mut source = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        let mut value = |option: &str| -> Option<String> {
            if argument == option {
                index += 1;
                arguments.get(index).cloned()
            } else {
                argument.strip_prefix(&format!("{option}=")).map(str::to_string)
            }
        };
        if let Some(name) = value("--crate-name") {
            crate_name = Some(name);
        } else if let Some(kind) = value("--crate-type") {
            crate_types.extend(kind.split(',').map(str::to_string));
        } else if let Some(directory) = value("--out-dir") {
            out_dir = Some(directory);
        } else if let Some(modes) = value("--emit") {
            emit.extend(modes.split(',').map(str::to_string));
        } else if let Some(codegen) = value("-C").or_else(|| value("--codegen")) {
            if let Some(extra) = codegen.strip_prefix("extra-filename=") {
                extra_filename = extra.to_string();
            }
        } else if let Some(extra) = argument.strip_prefix("-Cextra-filename=") {
            extra_filename = extra.to_string();
        } else if argument == "--test" {
            test = true;
        } else if source.is_none()
            && !argument.starts_with('-')
            && Path::new(argument)
                .extension()
                .is_some_and(|extension| extension == "rs")
        {
            source = Some(canonical_or_lexical(&current_dir.join(argument)));
        }
        index += 1;
    }
    let crate_name = crate_name?;
    let dep_info = emit.iter().find_map(|mode| {
        if let Some(path) = mode.strip_prefix("dep-info=") {
            Some(current_dir.join(path))
        } else if mode == "dep-info" {
            out_dir.as_ref().map(|directory| {
                current_dir
                    .join(directory)
                    .join(format!("{crate_name}{extra_filename}.d"))
            })
        } else {
            None
        }
    });
    Some(ParsedInvocation {
        key: UnitKey {
            manifest_dir: PathBuf::from(record.manifest_dir.as_ref()?),
            source: source?,
            crate_name,
            test,
            crate_types: if test { BTreeSet::new() } else { crate_types },
        },
        current_dir,
        dep_info,
    })
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    crate::utils::canonicalize_existing(path).unwrap_or_else(|_| normalize_lexically(path))
}

/// Locate the dep-info file rustc wrote for a unit, from Cargo's artifact names.
///
/// Hashed artifacts (`deps/libNAME-HASH.rmeta`, `deps/NAME-HASH`, and
/// `build/PACKAGE-HASH/build-script-build`) name rustc's own dep-info file. Cargo uplifts a
/// binary by linking its hashed `deps/NAME-HASH` file, so the same file there recovers the hash.
/// Otherwise Cargo's dep-info beside an uplifted artifact lists the unit's inputs and those of
/// its local dependencies: a superset that can only widen selection.
fn located_dep_info(artifact: &cargo_metadata::Artifact) -> Option<PathBuf> {
    let crate_name = artifact.target.name.replace('-', "_");
    let files = artifact
        .filenames
        .iter()
        .chain(artifact.executable.iter())
        .map(|path| path.as_std_path())
        .collect::<Vec<_>>();
    let is_hash = |value: &str| value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    for file in &files {
        let (Some(directory), Some(name)) = (file.parent(), file.file_name().and_then(OsStr::to_str)) else {
            continue;
        };
        let hash = if name.starts_with("build-script-") {
            directory
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(|unit| unit.rsplit_once('-'))
                .map(|(_, hash)| hash)
        } else {
            let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
            let stem = stem
                .strip_prefix("lib")
                .filter(|stem| stem.starts_with(&format!("{crate_name}-")))
                .unwrap_or(stem);
            stem.strip_prefix(&format!("{crate_name}-"))
        };
        if let Some(hash) = hash.filter(|hash| is_hash(hash)) {
            let candidate = directory.join(format!("{crate_name}-{hash}.d"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for file in &files {
        let Some(deps) = file.parent().map(|directory| directory.join("deps")) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&deps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name
                .to_str()
                .map(|name| name.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(name))
            else {
                continue;
            };
            let Some(hash) = stem
                .strip_prefix(&format!("{crate_name}-"))
                .filter(|hash| is_hash(hash))
            else {
                continue;
            };
            let candidate = deps.join(format!("{crate_name}-{hash}.d"));
            if candidate.is_file() && same_file(file, &entry.path()) {
                return Some(candidate);
            }
        }
    }
    files.iter().find_map(|file| {
        let name = file.file_name()?.to_str()?;
        let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
        let candidate = file.parent()?.join(format!("{stem}.d"));
        candidate.is_file().then_some(candidate)
    })
}

/// Whether two paths name one file: the same inode where links exist, else identical bytes.
fn same_file(left: &Path, right: &Path) -> bool {
    let (Ok(left_metadata), Ok(right_metadata)) = (fs::metadata(left), fs::metadata(right)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if left_metadata.dev() == right_metadata.dev() && left_metadata.ino() == right_metadata.ino() {
            return true;
        }
    }
    left_metadata.len() == right_metadata.len()
        && matches!((fs::read(left), fs::read(right)), (Ok(left), Ok(right)) if left == right)
}

/// One changed-path candidate before its base identity is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Candidate {
    path: String,
    package: Option<String>,
    target: Option<String>,
}

/// Everything read from Cargo and the recorder, before Git identities are attached.
struct Observation {
    units: usize,
    /// Manifest directories of local packages with at least one observed unit.
    packages: BTreeSet<PathBuf>,
    unobserved: Vec<String>,
    bypasses: BTreeSet<String>,
    files: BTreeSet<Candidate>,
    directories: BTreeSet<(String, String)>,
    environment: BTreeSet<String>,
}

struct Roots {
    workspace: PathBuf,
    repository: PathBuf,
    generated: Vec<PathBuf>,
}

enum Located {
    Workspace(String),
    OutsideWorkspace,
    Ignored,
}

impl Roots {
    fn capture(ctx: &WorkspaceContext) -> RailResult<Self> {
        let metadata = ctx.cargo().metadata();
        let mut generated = vec![metadata.target_directory.as_std_path().to_path_buf()];
        if let Some(build) = &metadata.build_directory {
            generated.push(build.as_std_path().to_path_buf());
        }
        Ok(Self {
            workspace: crate::utils::canonicalize_existing(ctx.workspace_root())?,
            repository: crate::utils::canonicalize_existing(ctx.planning_authority_source_root())?,
            generated: generated
                .into_iter()
                .map(|root| crate::utils::canonicalize_allow_missing(&root).unwrap_or(root))
                .collect(),
        })
    }

    fn locate(&self, path: &Path) -> Located {
        let lexical = normalize_lexically(path);
        let resolved = crate::utils::canonicalize_existing(&lexical)
            .ok()
            .and_then(|canonical| {
                // Keep the lexical spelling when it already lies in the workspace, so a path
                // through a tracked symlink stays the tracked path Git reports.
                (!lexical.starts_with(&self.workspace)).then_some(canonical)
            })
            .unwrap_or(lexical);
        if self.generated.iter().any(|root| resolved.starts_with(root)) {
            return Located::Ignored;
        }
        if let Ok(relative) = resolved.strip_prefix(&self.workspace) {
            let relative = crate::utils::path_to_git_format(relative);
            return if relative.is_empty() {
                Located::Ignored
            } else {
                Located::Workspace(relative)
            };
        }
        if resolved.starts_with(&self.repository) {
            Located::OutsideWorkspace
        } else {
            Located::Ignored
        }
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

impl Observation {
    fn collect(ctx: &WorkspaceContext, messages: &CargoMessages, records: &[RecordedInvocation]) -> RailResult<Self> {
        let roots = Roots::capture(ctx)?;
        let local = ctx
            .cargo()
            .metadata()
            .packages
            .iter()
            .filter(|package| package.source.is_none())
            .map(|package| (&package.id, package))
            .collect::<BTreeMap<_, _>>();
        let mut observation = Self {
            units: 0,
            packages: BTreeSet::new(),
            unobserved: Vec::new(),
            bypasses: BTreeSet::new(),
            files: BTreeSet::new(),
            directories: BTreeSet::new(),
            environment: BTreeSet::new(),
        };
        let mut invocations = BTreeMap::<UnitKey, Vec<ParsedInvocation>>::new();
        for record in records {
            match parse_invocation(record) {
                Some(invocation) => invocations.entry(invocation.key.clone()).or_default().push(invocation),
                None => {
                    observation
                        .bypasses
                        .insert("compiler_invocation_unrepresentable".to_string());
                }
            }
        }

        for artifact in &messages.artifacts {
            let Some(package) = local.get(&artifact.package_id) else {
                continue;
            };
            let Some(manifest_dir) = package
                .manifest_path
                .parent()
                .and_then(|directory| crate::utils::canonicalize_existing(directory.as_std_path()).ok())
            else {
                observation.bypasses.insert("compiled_package_unlocatable".to_string());
                continue;
            };
            if !manifest_dir.starts_with(&roots.repository) {
                continue;
            }
            observation.units += 1;
            observation.packages.insert(manifest_dir.clone());
            let label = format!(
                "{} {} ({})",
                package.name,
                artifact.target.name,
                artifact
                    .target
                    .kind
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("/")
            );
            let key = UnitKey {
                manifest_dir,
                source: canonical_or_lexical(artifact.target.src_path.as_std_path()),
                crate_name: artifact.target.name.replace('-', "_"),
                test: artifact.profile.test,
                crate_types: if artifact.profile.test {
                    BTreeSet::new()
                } else {
                    artifact.target.crate_types.iter().map(ToString::to_string).collect()
                },
            };
            let sources = if artifact.fresh {
                // Cargo resolves a local unit's relative paths from the workspace root.
                located_dep_info(artifact)
                    .map(|dep_info| vec![(roots.workspace.clone(), Some(dep_info))])
                    .unwrap_or_default()
            } else {
                invocations
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .map(|invocation| (invocation.current_dir.clone(), invocation.dep_info.clone()))
                    .collect::<Vec<_>>()
            };
            if sources.is_empty() {
                observation.bypasses.insert(
                    if artifact.fresh {
                        "fresh_unit_dep_info_unlocatable"
                    } else {
                        "unit_invocation_unrecorded"
                    }
                    .to_string(),
                );
                if !observation.unobserved.contains(&label) {
                    observation.unobserved.push(label);
                }
                continue;
            }
            let package_key = super::work::portable_package_key(ctx, package);
            for (current_dir, dep_info) in sources {
                let Some(contents) = dep_info.and_then(|dep_info| fs::read_to_string(dep_info).ok()) else {
                    observation.bypasses.insert("rustc_dep_info_unavailable".to_string());
                    continue;
                };
                let (paths, environment) = parse_dep_info(&contents);
                observation.environment.extend(environment);
                for path in paths {
                    match roots.locate(&current_dir.join(path)) {
                        Located::Workspace(path) => {
                            observation.files.insert(Candidate {
                                path,
                                package: Some(package_key.clone()),
                                target: Some(artifact.target.name.clone()),
                            });
                        }
                        Located::OutsideWorkspace => {
                            observation
                                .bypasses
                                .insert("repository_input_outside_workspace".to_string());
                        }
                        Located::Ignored => {}
                    }
                }
            }
        }

        for script in &messages.build_scripts {
            let Some(package) = local.get(&script.package_id) else {
                continue;
            };
            observation.record_build_script(ctx, &roots, package, script);
        }
        // Evidence speaks for the whole work item, so a build that skipped a member cannot
        // prove that member's changes irrelevant.
        for member in ctx.cargo().metadata().workspace_packages() {
            let observed = member
                .manifest_path
                .parent()
                .and_then(|root| crate::utils::canonicalize_existing(root.as_std_path()).ok())
                .is_some_and(|root| observation.packages.contains(&root));
            if !observed {
                observation.bypasses.insert("workspace_member_unobserved".to_string());
                observation.unobserved.push(format!("{} (no unit built)", member.name));
            }
        }
        Ok(observation)
    }

    fn record_build_script(
        &mut self,
        ctx: &WorkspaceContext,
        roots: &Roots,
        package: &Package,
        script: &cargo_metadata::BuildScript,
    ) {
        let Some(root) = package
            .manifest_path
            .parent()
            .map(|root| root.as_std_path().to_path_buf())
        else {
            self.bypasses.insert("build_script_package_unlocatable".to_string());
            return;
        };
        if !matches!(roots.locate(&root.join("Cargo.toml")), Located::Workspace(_)) {
            return;
        }
        let Some(output) = script
            .out_dir
            .parent()
            .and_then(|directory| fs::read_to_string(directory.join("output")).ok())
        else {
            self.bypasses.insert("build_script_output_unavailable".to_string());
            return;
        };
        let package_key = super::work::portable_package_key(ctx, package);
        let mut declared_path = false;
        for line in output.lines() {
            match crate::build_script::result::rerun_declaration(line) {
                Some(crate::build_script::result::RerunDeclaration::Changed(declared)) => {
                    declared_path = true;
                    let path = root.join(declared);
                    if !path.exists() {
                        // Cargo reruns the script on every build while a declared path is missing.
                        self.bypasses.insert("build_script_rerun_path_missing".to_string());
                        continue;
                    }
                    match roots.locate(&path) {
                        Located::Workspace(relative) if path.is_dir() => {
                            self.directories.insert((relative, package_key.clone()));
                        }
                        Located::Workspace(relative) => {
                            self.files.insert(Candidate {
                                path: relative,
                                package: Some(package_key.clone()),
                                target: None,
                            });
                        }
                        Located::OutsideWorkspace => {
                            self.bypasses.insert("repository_input_outside_workspace".to_string());
                        }
                        Located::Ignored => {}
                    }
                }
                Some(crate::build_script::result::RerunDeclaration::EnvironmentChanged(name)) => {
                    self.environment.insert(name.to_string());
                }
                None => {}
            }
        }
        if !declared_path {
            // Without a declared path, Cargo reruns the script when any package file changes.
            match roots.locate(&root.join("Cargo.toml")) {
                Located::Workspace(manifest) => {
                    let directory = manifest
                        .strip_suffix("/Cargo.toml")
                        .map_or_else(|| ".".to_string(), str::to_string);
                    self.directories.insert((directory, package_key));
                }
                Located::OutsideWorkspace | Located::Ignored => {}
            }
        }
    }

    /// Attach base identities and produce the evidence for one work item.
    fn resolve(&self, ctx: &WorkspaceContext, source_base: &str) -> RailResult<ResolvedEvidence> {
        let mut bypasses = self.bypasses.clone();
        let git = ctx.git()?.git();
        let repository_path = |path: &str| {
            ctx.workspace_prefix()
                .map_or_else(|| PathBuf::from(path), |prefix| prefix.join(path))
        };
        let paths = self
            .files
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<BTreeSet<_>>();
        let entries = git
            .collect_tree_entries_for_paths(
                source_base,
                &paths.iter().map(|path| repository_path(path)).collect::<Vec<_>>(),
            )?
            .into_iter()
            .map(|entry| (entry.path, format!("git:{}:{}", entry.mode, entry.object_id)))
            .collect::<BTreeMap<_, _>>();
        let base_packages = super::work::portable_base_model(ctx)?
            .packages
            .into_iter()
            .map(|package| package.key)
            .collect::<BTreeSet<_>>();
        let ignored = ignored_paths(
            ctx,
            self.files
                .iter()
                .map(|candidate| candidate.path.as_str())
                .filter(|path| !entries.contains_key(&repository_path(path))),
        )?;
        let mut inputs = Vec::new();
        for candidate in &self.files {
            let Some(identity) = entries.get(&repository_path(&candidate.path)) else {
                // An ignored file cannot enter a later change; any other untracked read means
                // the build did not read only HEAD.
                if !ignored.contains(&candidate.path) {
                    bypasses.insert("untracked_compiler_input".to_string());
                }
                continue;
            };
            let known = candidate
                .package
                .as_ref()
                .is_some_and(|package| base_packages.contains(package));
            inputs.push(ObservedInput {
                path: candidate.path.clone(),
                identity: identity.clone(),
                package: known.then(|| candidate.package.clone()).flatten(),
                target: known.then(|| candidate.target.clone()).flatten(),
            });
        }
        let mut directories = Vec::new();
        for (path, package) in &self.directories {
            if !base_packages.contains(package) {
                bypasses.insert("build_script_package_unmodeled".to_string());
                continue;
            }
            if path != "." {
                let root = repository_path(path);
                let tracked = git
                    .collect_tree_entries_for_paths(source_base, std::slice::from_ref(&root))?
                    .iter()
                    .any(|entry| entry.path.starts_with(&root));
                if !tracked {
                    bypasses.insert("build_script_directory_untracked".to_string());
                    continue;
                }
            }
            directories.push(ObservedDirectory {
                path: path.clone(),
                package: package.clone(),
            });
        }
        let environment = self
            .environment
            .iter()
            .filter(|name| !crate::compiler::observation::is_cargo_provided_environment(name))
            .filter(|name| {
                let secret = secret_capability_name(name);
                if secret {
                    bypasses.insert("secret_environment_input".to_string());
                }
                !secret
            })
            .cloned()
            .collect::<BTreeSet<_>>();

        Ok(ResolvedEvidence {
            environment,
            work: ObservedWorkEvidence {
                complete: bypasses.is_empty(),
                bypasses: bypasses.into_iter().collect(),
                inputs,
                directories,
            },
        })
    }
}

/// Return the workspace-relative paths that Git ignores.
fn ignored_paths<'a>(ctx: &WorkspaceContext, paths: impl Iterator<Item = &'a str>) -> RailResult<BTreeSet<String>> {
    let paths = paths.collect::<BTreeSet<_>>();
    if paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    let git = ctx.git()?;
    let mut child = git
        .git()
        .git_cmd()
        .current_dir(ctx.workspace_root())
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "classifying untracked compiler inputs".to_string())?;
    let mut input = Vec::new();
    for path in &paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| RailError::message("git check-ignore input is unavailable"))?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| RailError::message("git check-ignore input writer panicked"))??;
    // Exit status 1 means that no path is ignored.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(RailError::message(format!(
            "git check-ignore failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

struct ResolvedEvidence {
    environment: BTreeSet<String>,
    work: ObservedWorkEvidence,
}

/// Return the input paths and environment names in one rustc dep-info file.
fn parse_dep_info(contents: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut paths = BTreeSet::new();
    let mut environment = BTreeSet::new();
    for line in contents.lines() {
        if let Some(record) = line.strip_prefix("# env-dep:") {
            let name = record.split_once('=').map_or(record, |(name, _)| name);
            environment.insert(name.to_string());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some((_, dependencies)) = line.split_once(": ") else {
            continue;
        };
        let mut current = String::new();
        let mut characters = dependencies.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\\' if characters.peek() == Some(&' ') => {
                    current.push(' ');
                    characters.next();
                }
                ' ' => {
                    if !current.is_empty() {
                        paths.insert(std::mem::take(&mut current));
                    }
                }
                other => current.push(other),
            }
        }
        if !current.is_empty() {
            paths.insert(current);
        }
    }
    (paths, environment)
}

fn load_existing(output: &Path) -> RailResult<Option<PlanningEvidenceManifest>> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(RailError::message(format!(
                "evidence output '{}' is not a regular file",
                output.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    load_signed_manifest(output).map(Some).map_err(|(code, description)| {
        RailError::with_help(
            format!(
                "existing evidence '{}' cannot be extended ({code}): {description}",
                output.display()
            ),
            "remove the file or choose another --output",
        )
    })
}

fn merge_existing(
    manifest: &mut PlanningEvidenceManifest,
    existing: PlanningEvidenceManifest,
    work: &str,
    output: &Path,
) -> RailResult<()> {
    let bindings = |manifest: &PlanningEvidenceManifest| {
        (
            manifest.source_base.clone(),
            manifest.cargo_identity.clone(),
            manifest.cargo_configuration_identity.clone(),
            manifest.toolchain_identity.clone(),
            manifest.target_identity.clone(),
            manifest.platform.clone(),
            manifest.provider.identity.clone(),
        )
    };
    if bindings(manifest) != bindings(&existing) {
        return Err(RailError::with_help(
            format!(
                "existing evidence '{}' is bound to another source, Cargo universe, toolchain, target, platform, or producer",
                output.display()
            ),
            "remove the file or choose another --output before recording this checkout",
        ));
    }
    let mut environment = existing.environment.into_iter().collect::<BTreeSet<_>>();
    environment.extend(manifest.environment.drain(..));
    manifest.environment = environment.into_iter().collect();
    for (id, evidence) in existing.work {
        if id != work {
            manifest.work.insert(id, evidence);
        }
    }
    Ok(())
}

fn write_manifest(output: &Path, manifest: &PlanningEvidenceManifest) -> RailResult<u64> {
    let directory = output
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    let mut file = tempfile::Builder::new()
        .prefix(".planning-evidence-")
        .tempfile_in(directory)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(output)
        .map_err(|error| RailError::message(format!("writing '{}': {}", output.display(), error.error)))?;
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_info_yields_escaped_paths_and_environment_names() {
        let contents = "/out/demo-1.d: src/lib.rs src/with\\ space.txt /abs/data.bin\n\
                        \n\
                        /out/libdemo-1.rmeta: src/lib.rs src/with\\ space.txt /abs/data.bin\n\
                        \n\
                        src/lib.rs:\n\
                        src/with\\ space.txt:\n\
                        \n\
                        # env-dep:DEMO_MODE=fast\n\
                        # env-dep:UNSET_NAME\n";
        let (paths, environment) = parse_dep_info(contents);
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            ["/abs/data.bin", "src/lib.rs", "src/with space.txt"]
        );
        assert_eq!(environment.into_iter().collect::<Vec<_>>(), ["DEMO_MODE", "UNSET_NAME"]);
    }

    #[test]
    fn invocation_locates_cargo_dep_info_and_ignores_test_crate_types() {
        let record = RecordedInvocation {
            version: RECORD_VERSION,
            current_dir: Some("/work".to_string()),
            manifest_dir: Some("/work/crates/demo".to_string()),
            arguments: Some(
                [
                    "/toolchain/clippy-driver",
                    "/toolchain/rustc",
                    "--crate-name",
                    "demo",
                    "--edition=2024",
                    "crates/demo/src/lib.rs",
                    "--emit=dep-info,metadata",
                    "--test",
                    "-C",
                    "extra-filename=-0123",
                    "--out-dir",
                    "/work/target/debug/deps",
                ]
                .map(str::to_string)
                .to_vec(),
            ),
        };
        let parsed = parse_invocation(&record).expect("recorded unit");
        assert_eq!(parsed.key.crate_name, "demo");
        assert_eq!(parsed.key.source, Path::new("/work/crates/demo/src/lib.rs"));
        assert!(parsed.key.test && parsed.key.crate_types.is_empty());
        assert_eq!(
            parsed.dep_info.as_deref(),
            Some(Path::new("/work/target/debug/deps/demo-0123.d"))
        );
    }

    #[test]
    fn recorded_command_must_be_an_ordinary_cargo_subcommand() {
        validate_request("cargo.test", &["test".into(), "--workspace".into()]).expect("ordinary test build");
        validate_request("cargo.doc", &["doc".into()]).expect_err("rustdoc work is not recordable");
        validate_request("cargo.test", &["+nightly".into(), "test".into()]).expect_err("toolchain override");
        validate_request("cargo.test", &["test".into(), "--message-format=json".into()])
            .expect_err("message format is owned by Cargo-Rail");
        validate_request(
            "cargo.clippy",
            &["clippy".into(), "--".into(), "--message-format=short".into()],
        )
        .expect("arguments after `--` belong to the compiler driver");
    }
}
