//! Exact GitHub draft and publication reconciliation for the shared transaction.

use super::{
    artifacts::{ArtifactEvidence, Assets},
    planner::CrateReleasePlan,
    process,
    remote::RemoteRepository,
};
use crate::error::{RailError, RailResult};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Write, path::Path};

pub(crate) struct GithubRelease<'a> {
    pub root: &'a Path,
    pub repository: &'a RemoteRepository,
    pub source: &'a str,
    pub package: &'a CrateReleasePlan,
    pub evidence: Option<&'a ArtifactEvidence>,
    pub assets: &'a Assets,
}

impl GithubRelease<'_> {
    pub(crate) fn reconcile(
        &self,
        retained_id: Option<&str>,
        publish: bool,
        mut retain_id: impl FnMut(&str) -> RailResult<()>,
    ) -> RailResult<String> {
        let Self {
            root,
            repository,
            source,
            package,
            evidence,
            assets,
        } = *self;
        let endpoint = format!(
            "repos/{}/releases/tags/{}",
            repository.path(),
            encode(&package.tag_name)
        );
        let mut release = get(root, repository, &endpoint)?;
        if release.is_none() {
            if retained_id.is_some() || publish {
                return Err(RailError::message("retained GitHub release is missing"));
            }
            let presentation = package
                .presentation
                .as_ref()
                .ok_or_else(|| RailError::message("release has no captured presentation"))?;
            let body = json!({"tag_name": package.tag_name, "target_commitish": source,
            "name": format!("{} v{}", package.name, package.new_version), "body": presentation.release_notes,
            "draft": true, "prerelease": !package.new_version.pre.is_empty()});
            request(
                root,
                repository,
                "POST",
                &format!("repos/{}/releases", repository.path()),
                &body,
            )?;
            release = get(root, repository, &endpoint)?;
        }
        let release = release.ok_or_else(|| RailError::message("created GitHub release is not observable"))?;
        let (id, missing) = inspect(&release, source, package, evidence)?;
        if retained_id.is_some_and(|previous| previous != id.to_string()) {
            return Err(RailError::message(
                "GitHub release identity differs from the retained object",
            ));
        }
        retain_id(&id.to_string())?;
        for name in missing {
            let path = assets.path(&package.name, &name);
            let output = process::run(
                "gh",
                &[
                    "release",
                    "upload",
                    &package.tag_name,
                    path.to_str()
                        .ok_or_else(|| RailError::message("asset path is not UTF-8"))?,
                    "--repo",
                    &repository.selector(),
                ],
                Some(root),
            )?;
            if !output.status.success() {
                return Err(RailError::message(format!(
                    "GitHub asset upload was not acknowledged for '{name}'; resume to reconcile"
                )));
            }
        }
        let verified =
            get(root, repository, &endpoint)?.ok_or_else(|| RailError::message("GitHub release disappeared"))?;
        let (verified_id, missing) = inspect(&verified, source, package, evidence)?;
        if verified_id != id || !missing.is_empty() {
            return Err(RailError::message(
                "GitHub draft has no complete verified asset inventory",
            ));
        }
        if publish && verified["draft"] == true {
            request(
                root,
                repository,
                "PATCH",
                &format!("repos/{}/releases/{id}", repository.path()),
                &json!({"draft":false}),
            )?;
            let public = get(root, repository, &endpoint)?
                .ok_or_else(|| RailError::message("published GitHub release is not observable"))?;
            let (public_id, missing) = inspect(&public, source, package, evidence)?;
            if public_id != id || public["draft"] != false || !missing.is_empty() {
                return Err(RailError::message("GitHub release publication has not been verified"));
            }
        }
        Ok(id.to_string())
    }
}

fn inspect(
    release: &Value,
    source: &str,
    package: &CrateReleasePlan,
    evidence: Option<&ArtifactEvidence>,
) -> RailResult<(u64, Vec<String>)> {
    let id = super::validation::positive(release, "id")?;
    let notes = package
        .presentation
        .as_ref()
        .ok_or_else(|| RailError::message("release has no captured presentation"))?;
    if release["tag_name"] != package.tag_name
        || release["target_commitish"] != source
        || release["name"] != format!("{} v{}", package.name, package.new_version)
        || release["body"] != notes.release_notes
        || release["prerelease"] != !package.new_version.pre.is_empty()
        || !release["draft"].is_boolean()
    {
        return Err(RailError::message(
            "GitHub release conflicts with the intended source, tag, title, notes, or prerelease policy",
        ));
    }
    let actual = release["assets"]
        .as_array()
        .ok_or_else(|| RailError::message("GitHub release has no asset inventory"))?;
    let expected = evidence.map_or(&[][..], |evidence| evidence.files.as_slice());
    if actual.len() > expected.len() {
        return Err(RailError::message("GitHub release has extra assets"));
    }
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for asset in actual {
        let name = asset["name"]
            .as_str()
            .ok_or_else(|| RailError::message("GitHub asset has no name"))?;
        let Some(expected) = expected.iter().find(|expected| expected.name == name) else {
            return Err(RailError::message("GitHub release contains an undeclared asset"));
        };
        if !names.insert(name)
            || !ids.insert(super::validation::positive(asset, "id")?)
            || asset["state"] != "uploaded"
            || asset["size"] != expected.bytes
            || asset["digest"] != format!("sha256:{}", expected.sha256)
        {
            return Err(RailError::message(
                "GitHub release asset conflicts with the sealed bytes",
            ));
        }
    }
    let missing = expected
        .iter()
        .filter(|asset| !names.contains(asset.name.as_str()))
        .map(|asset| asset.name.clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() && release["draft"] != true {
        return Err(RailError::message(
            "published GitHub release has missing assets; publication is not repairable by changing its inventory",
        ));
    }
    Ok((id, missing))
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn get(root: &Path, repository: &RemoteRepository, endpoint: &str) -> RailResult<Option<Value>> {
    let host = repository
        .host()
        .ok_or_else(|| RailError::message("GitHub release has no host"))?;
    let output = process::run("gh", &["api", "--hostname", host, "--include", endpoint], Some(root))?;
    let boundary = output
        .stdout
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            output
                .stdout
                .windows(2)
                .position(|bytes| bytes == b"\n\n")
                .map(|index| (index, 2))
        })
        .ok_or_else(|| RailError::message("GitHub release observation has no HTTP status"))?;
    let headers = std::str::from_utf8(&output.stdout[..boundary.0])
        .map_err(|_| RailError::message("invalid GitHub response headers"))?;
    let status = headers.lines().next().and_then(|line| line.split_whitespace().nth(1));
    if status == Some("404") && !output.status.success() {
        return Ok(None);
    }
    if status != Some("200") || !output.status.success() {
        return Err(RailError::message("GitHub release observation is unavailable"));
    }
    Ok(Some(super::contract::decode(
        &output.stdout[boundary.0 + boundary.1..],
    )?))
}

fn request(root: &Path, repository: &RemoteRepository, method: &str, endpoint: &str, body: &Value) -> RailResult<()> {
    let mut input = tempfile::NamedTempFile::new()?;
    input.write_all(&serde_json::to_vec(body)?)?;
    input.flush()?;
    let output = process::run(
        "gh",
        &[
            "api",
            "--hostname",
            repository
                .host()
                .ok_or_else(|| RailError::message("GitHub release has no host"))?,
            "--method",
            method,
            endpoint,
            "--input",
            input
                .path()
                .to_str()
                .ok_or_else(|| RailError::message("GitHub request path is not UTF-8"))?,
        ],
        Some(root),
    )?;
    if !output.status.success() {
        return Err(RailError::message(
            "GitHub release effect was not acknowledged; resume to reconcile",
        ));
    }
    Ok(())
}
