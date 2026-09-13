//! Native release bytes supplied by exact validated workflow attempts.

use super::{
    planner::CrateReleasePlan,
    state::{ReleasePhase, ReleaseState},
    validation,
};
use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::source::{ContentDigest, RepositoryPath};
use rscrypto::Sha256;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FILES: usize = 64;

/// One package's resolved artifact producer and complete release-file inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRequirement {
    /// Released package that owns these assets.
    pub package: String,
    /// Authorized workflow path; its required jobs belong to release validation policy.
    pub workflow: String,
    /// Exact asset names in lexical order.
    pub files: Vec<AssetRequirement>,
}

/// A resolved release asset, independent of runner paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRequirement {
    /// Flat portable filename.
    pub name: String,
    /// Target asserted by the authorized product packaging job.
    pub target: Option<String>,
    /// Optional Git-root-relative regular file to compare at the release commit.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactEvidence {
    pub package: String,
    pub run_id: u64,
    pub attempt: u64,
    pub artifact_id: u64,
    pub bytes: u64,
    pub sha256: String,
    pub expires_at: u64,
    pub files: Vec<AssetEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetEvidence {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

pub(crate) struct Assets {
    directory: tempfile::TempDir,
}

impl Assets {
    pub(crate) fn path(&self, package: &str, name: &str) -> PathBuf {
        self.directory.path().join(package).join(name)
    }
}

fn filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !name.ends_with('.')
        && !matches!(
            name.split('.').next().unwrap_or_default().to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

pub(crate) fn validate_config(config: &ReleaseConfig) -> RailResult<()> {
    for (package, artifact) in &config.artifacts {
        if !filename(package)
            || package.len() > 64
            || !config.validation.contains_key(&artifact.workflow)
            || artifact.files.is_empty()
            || artifact.files.len() > MAX_FILES
        {
            return Err(RailError::message(
                "release artifact requires a package, an authorized validation workflow, and 1..64 files",
            ));
        }
        let mut names = BTreeSet::new();
        for (template, file) in &artifact.files {
            let name = template.replace("{crate}", package).replace("{version}", "0.0.0");
            if !filename(&name)
                || !names.insert(name.to_ascii_lowercase())
                || file
                    .target
                    .as_ref()
                    .is_some_and(|target| !filename(target) || !target.contains('-'))
                || file.source.as_ref().is_some_and(|source| {
                    source.contains(['\\', ':'])
                        || RepositoryPath::new(Path::new(source)).is_err()
                        || RepositoryPath::new(Path::new(source)).is_ok_and(|path| path.as_str() != source)
                })
            {
                return Err(RailError::message(
                    "release asset has an unsafe or conflicting filename, target, or source path",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn plan(config: &ReleaseConfig, packages: &[CrateReleasePlan]) -> RailResult<Vec<ArtifactRequirement>> {
    validate_config(config)?;
    let mut requirements = Vec::new();
    for (name, artifact) in &config.artifacts {
        let Some(package) = packages.iter().find(|package| package.name == *name) else {
            continue;
        };
        let mut files = artifact
            .files
            .iter()
            .map(|(template, file)| AssetRequirement {
                name: template
                    .replace("{crate}", name)
                    .replace("{version}", &package.new_version.to_string()),
                target: file.target.clone(),
                source: file.source.clone(),
            })
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.name.cmp(&b.name));
        let mut names = BTreeSet::new();
        if files
            .iter()
            .any(|file| !filename(&file.name) || !names.insert(file.name.to_ascii_lowercase()))
        {
            return Err(RailError::message(
                "resolved release asset filenames are unsafe or collide",
            ));
        }
        requirements.push(ArtifactRequirement {
            package: name.clone(),
            workflow: artifact.workflow.clone(),
            files,
        });
    }
    Ok(requirements)
}

pub(crate) fn validate_record(state: &ReleaseState) -> RailResult<()> {
    let required = &state.intent.plan.artifacts;
    if *required != plan(&state.intent.release_config, &state.intent.plan.crates)? {
        return Err(RailError::message(
            "release artifact requirements disagree with captured configuration",
        ));
    }
    if !required.is_empty()
        && (state.intent.skip_tag
            || !state.intent.release_config.remote_effects.creates_forge_release()
            || !state.intent.remote_repository.as_ref().is_some_and(|repository| {
                repository.host() == Some("github.com")
                    || state.intent.release_config.remote_effects == crate::config::ReleaseRemoteEffects::Github
            }))
    {
        return Err(RailError::message(
            "native release artifacts require authorized GitHub release effects and tags",
        ));
    }
    if state.artifacts.is_empty() && state.phase < ReleasePhase::Ready {
        return Ok(());
    }
    if state.artifacts.len() != required.len() || !state.artifacts.is_empty() && !state.readiness.is_complete() {
        return Err(RailError::message(
            "release has no complete sealed native artifact inventory",
        ));
    }
    let mut ids = BTreeSet::new();
    for (requirement, evidence) in required.iter().zip(&state.artifacts) {
        let run = state.validation.iter().find(|run| run.workflow == requirement.workflow);
        if evidence.package != requirement.package
            || evidence.artifact_id == 0
            || !ids.insert(evidence.artifact_id)
            || evidence.bytes == 0
            || evidence.bytes > MAX_ARCHIVE_BYTES
            || !checksum(&evidence.sha256)
            || evidence.expires_at == 0
            || run.is_none_or(|run| run.run_id != evidence.run_id || run.attempt != evidence.attempt)
            || evidence.files.len() != requirement.files.len()
            || evidence.files.iter().zip(&requirement.files).any(|(actual, expected)| {
                actual.name != expected.name || actual.bytes > MAX_ASSET_BYTES || !checksum(&actual.sha256)
            })
        {
            return Err(RailError::message(
                "sealed native artifact evidence has inconsistent producer, bytes, or inventory",
            ));
        }
    }
    Ok(())
}

fn checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Download and verify before any publication. A retained identity may never select another artifact.
pub(crate) fn acquire(root: &Path, state: &mut ReleaseState, state_path: &Path) -> RailResult<Assets> {
    let assets = Assets {
        directory: tempfile::tempdir()?,
    };
    if state.intent.plan.artifacts.is_empty() {
        return Ok(assets);
    }
    let repository = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("artifacts have no repository"))?;
    let host = repository
        .host()
        .ok_or_else(|| RailError::message("artifacts have no GitHub host"))?;
    let source = state
        .release_commit()
        .ok_or_else(|| RailError::message("artifacts have no release commit"))?;
    let mut sealed = Vec::new();
    for required in &state.intent.plan.artifacts {
        let run = state
            .validation
            .iter()
            .find(|run| run.workflow == required.workflow)
            .ok_or_else(|| RailError::message("artifact producer has no verified workflow attempt"))?;
        let name = format!("release-{}-{}-{}", required.package, run.run_id, run.attempt);
        let retained = state
            .artifacts
            .iter()
            .find(|artifact| artifact.package == required.package);
        let metadata = if let Some(previous) = retained {
            validation::api(
                root,
                host,
                &format!("repos/{}/actions/artifacts/{}", repository.path(), previous.artifact_id),
            )?
        } else {
            let list = validation::api(
                root,
                host,
                &format!(
                    "repos/{}/actions/runs/{}/artifacts?per_page=100",
                    repository.path(),
                    run.run_id
                ),
            )?;
            let mut matching = validation::bounded_rows(&list, "artifacts")?
                .iter()
                .filter(|artifact| artifact["name"] == name);
            let metadata = matching
                .next()
                .ok_or_else(|| RailError::message(format!("required artifact '{name}' is missing")))?
                .clone();
            if matching.next().is_some() {
                return Err(RailError::message("required artifact name is ambiguous"));
            }
            metadata
        };
        let attempt = validation::api(
            root,
            host,
            &format!(
                "repos/{}/actions/runs/{}/attempts/{}",
                repository.path(),
                run.run_id,
                run.attempt
            ),
        )?;
        let mut evidence = inspect_metadata(required, run, source, &name, &metadata, &attempt)?;
        if retained.is_some_and(|previous| {
            previous.artifact_id != evidence.artifact_id
                || previous.bytes != evidence.bytes
                || previous.sha256 != evidence.sha256
                || previous.expires_at != evidence.expires_at
        }) {
            return Err(RailError::message(
                "retained artifact identity or retention changed; rebuilding is not authorized",
            ));
        }
        let endpoint = format!(
            "repos/{}/actions/artifacts/{}/zip",
            repository.path(),
            evidence.artifact_id
        );
        let mut archive = download(root, host, &endpoint, evidence.bytes)?;
        let (bytes, digest) = hash(&mut archive)?;
        if bytes != evidence.bytes || digest != evidence.sha256 {
            return Err(RailError::message(
                "downloaded artifact does not match the authorized archive digest and size",
            ));
        }
        archive.seek(SeekFrom::Start(0))?;
        let directory = assets.directory.path().join(&required.package);
        std::fs::create_dir(&directory)?;
        evidence.files = extract(root, source, required, archive, &directory)?;
        if retained.is_some_and(|previous| *previous != evidence) {
            return Err(RailError::message("retained release asset bytes changed"));
        }
        sealed.push(evidence);
    }
    if state.artifacts.is_empty() {
        state.artifacts = sealed;
        state.save(state_path)?;
    }
    Ok(assets)
}

fn timestamp(value: &Value, field: &str) -> RailResult<u64> {
    value[field]
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .and_then(|time| u64::try_from(time.timestamp()).ok())
        .ok_or_else(|| RailError::message(format!("artifact producer has no valid {field}")))
}

fn inspect_metadata(
    required: &ArtifactRequirement,
    run: &validation::WorkflowRun,
    source: &str,
    name: &str,
    value: &Value,
    attempt: &Value,
) -> RailResult<ArtifactEvidence> {
    let artifact_id = validation::positive(value, "id")?;
    let bytes = validation::positive(value, "size_in_bytes")?;
    let expires_at = timestamp(value, "expires_at")?;
    let created_at = timestamp(value, "created_at")?;
    let digest = value["digest"]
        .as_str()
        .and_then(|text| text.strip_prefix("sha256:"))
        .unwrap_or_default();
    if value["name"] != name
        || value["expired"] != false
        || bytes > MAX_ARCHIVE_BYTES
        || !checksum(digest)
        || expires_at <= chrono::Utc::now().timestamp().cast_unsigned()
        || expires_at <= created_at
        || value["workflow_run"]["id"] != run.run_id
        || value["workflow_run"]["head_sha"] != source
        || value["workflow_run"]["repository_id"].as_u64().is_none_or(|id| id == 0)
        || value["workflow_run"]["repository_id"] != value["workflow_run"]["head_repository_id"]
        || attempt["id"] != run.run_id
        || attempt["run_attempt"] != run.attempt
        || attempt["head_sha"] != source
        || attempt["status"] != "completed"
        || attempt["conclusion"] != "success"
        || created_at < timestamp(attempt, "run_started_at")?
        || created_at > timestamp(attempt, "updated_at")?
    {
        return Err(RailError::message(
            "artifact is expired or has the wrong source, producer attempt, time, or byte identity",
        ));
    }
    Ok(ArtifactEvidence {
        package: required.package.clone(),
        run_id: run.run_id,
        attempt: run.attempt,
        artifact_id,
        bytes,
        sha256: digest.to_owned(),
        expires_at,
        files: Vec::new(),
    })
}

fn download(root: &Path, host: &str, endpoint: &str, bytes: u64) -> RailResult<File> {
    let mut output = tempfile::tempfile()?;
    let mut command = Command::new("gh");
    command
        .current_dir(root)
        .args(["api", "--hostname", host, endpoint])
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if output.metadata()?.len() > bytes || Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            return Err(RailError::message(
                "artifact download exceeded its declared size or ten-minute deadline",
            ));
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(RailError::message("GitHub artifact download failed"));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    output.seek(SeekFrom::Start(0))?;
    Ok(output)
}

fn hash(reader: &mut impl Read) -> RailResult<(u64, String)> {
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > MAX_ARCHIVE_BYTES {
            return Err(RailError::message("release artifact exceeds byte limit"));
        }
        digest.update(&buffer[..n]);
    }
    Ok((bytes, ContentDigest::from_sha256_bytes(digest.finalize()).to_string()))
}

fn extract(
    root: &Path,
    source: &str,
    required: &ArtifactRequirement,
    archive: File,
    directory: &Path,
) -> RailResult<Vec<AssetEvidence>> {
    let mut zip = zip::ZipArchive::new(archive)
        .map_err(|error| RailError::message(format!("invalid release artifact ZIP: {error}")))?;
    if zip.len() != required.files.len() {
        return Err(RailError::message("artifact ZIP has missing or extra release files"));
    }
    let mut names = BTreeSet::new();
    let mut evidence = Vec::new();
    let mut total = 0;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| RailError::message(format!("invalid artifact ZIP entry: {error}")))?;
        let name = entry.name().to_owned();
        if !filename(&name)
            || !names.insert(name.to_ascii_lowercase())
            || entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
            || entry.size() > MAX_ASSET_BYTES
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(RailError::message(
                "artifact ZIP has an unsafe, oversized, duplicate, or unsupported file",
            ));
        }
        let expected = required
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or_else(|| RailError::message("artifact ZIP has an undeclared file"))?;
        total += entry.size();
        if total > MAX_ARCHIVE_BYTES {
            return Err(RailError::message("artifact ZIP exceeds expanded byte limit"));
        }
        let path = directory.join(&name);
        let mut output = File::options().write(true).read(true).create_new(true).open(&path)?;
        let size = entry.size();
        let copied = std::io::copy(&mut (&mut entry).take(size + 1), &mut output)?;
        if copied != size {
            return Err(RailError::message("artifact ZIP file has a contradictory size"));
        }
        output.flush()?;
        output.seek(SeekFrom::Start(0))?;
        let (bytes, sha256) = hash(&mut output)?;
        if let Some(source_path) = &expected.source {
            let spec = format!("{source}:{source_path}");
            let mode = super::process::run("git", &["ls-tree", source, "--", source_path], Some(root))?;
            let row = String::from_utf8_lossy(&mode.stdout);
            if !mode.status.success()
                || !row.starts_with("100644 blob ") && !row.starts_with("100755 blob ")
                || row.lines().count() != 1
            {
                return Err(RailError::message(
                    "static release asset source is not one committed regular file",
                ));
            }
            let length = super::process::run("git", &["cat-file", "-s", &spec], Some(root))?;
            if !length.status.success()
                || String::from_utf8_lossy(&length.stdout).trim().parse::<u64>().ok() != Some(bytes)
            {
                return Err(RailError::message(
                    "release asset differs from its committed source file size",
                ));
            }
            let original = super::process::run("git", &["cat-file", "blob", &spec], Some(root))?;
            if !original.status.success() || ContentDigest::sha256(&original.stdout).to_string() != sha256 {
                return Err(RailError::message(
                    "release asset differs from its committed source file",
                ));
            }
        }
        evidence.push(AssetEvidence { name, bytes, sha256 });
    }
    evidence.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(evidence)
}
