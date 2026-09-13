//! GitHub execution of the same durable release transaction.

use super::{
    contract, process,
    remote::RemoteRepository,
    review,
    state::{ReleaseState, ReleaseStatus},
    validation,
};
use crate::{
    config::ReleaseConfig,
    error::{RailError, RailResult},
    git::SystemGit,
};
use serde_json::{Value, json};
use std::{io::Write, path::Path};

pub(crate) fn validate_record(state: &ReleaseState) -> RailResult<()> {
    if state.intent.hosted
        && (!state.remote_storage
            || state.intent.release_config.hosted_workflow.is_none()
            || state
                .intent
                .remote_repository
                .as_ref()
                .is_none_or(|remote| remote.host() != Some("github.com")))
        || state.executor.is_some_and(|id| id == 0 || !state.intent.hosted)
        || state.validation_dispatches.iter().any(|(workflow, run)| {
            !state.intent.hosted
                || !state.intent.release_config.validation.contains_key(workflow)
                || *run == 0
                || Some(*run) == state.executor
        })
        || state.validation.iter().any(|run| {
            state
                .validation_dispatches
                .get(&run.workflow)
                .is_some_and(|dispatch| *dispatch != run.run_id)
        })
    {
        return Err(RailError::message("hosted release has inconsistent workflow authority"));
    }
    Ok(())
}

pub(crate) fn require_successor(previous: &ReleaseState, next: &ReleaseState) -> RailResult<()> {
    if previous
        .validation_dispatches
        .iter()
        .any(|(workflow, run)| next.validation_dispatches.get(workflow) != Some(run))
    {
        return Err(RailError::message("cannot replace a retained validation dispatch"));
    }
    Ok(())
}

pub(crate) fn dispatch(root: &Path, state: &ReleaseState) -> RailResult<u64> {
    if !state.intent.hosted || state.status != ReleaseStatus::Active {
        return Err(RailError::message("release has no active hosted request"));
    }
    let repository = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("hosted release has no repository"))?;
    let workflow = state
        .intent
        .release_config
        .hosted_workflow
        .as_deref()
        .ok_or_else(|| RailError::message("hosted release has no workflow"))?;
    let git = SystemGit::open(root)?;
    let tip = review::remote_head(&git, &format!("refs/heads/{}", state.intent.branch))?
        .ok_or_else(|| RailError::message("hosted release base branch is missing"))?;
    let source = state.release_commit().unwrap_or(&state.intent.initial_head);
    if !git.run_git_check(&["merge-base", "--is-ancestor", source, &tip]) {
        git.run_git(&["fetch", "--no-tags", "--no-write-fetch-head", "origin", &tip])?;
        if !git.run_git_check(&["merge-base", "--is-ancestor", source, &tip]) && state.review.is_none() {
            return Err(RailError::message(
                "release source is not contained in the authorized remote branch",
            ));
        }
    }
    let run = dispatch_workflow(
        root,
        repository,
        workflow,
        &state.intent.branch,
        json!({"transaction":state.transaction_id,"intent":state.intent.identity,"source":source}),
    )?;
    crate::progress!(
        "release {} submitted: https://github.com/{}/actions/runs/{}",
        state.transaction_id,
        repository.path(),
        run
    );
    Ok(run)
}

fn dispatch_workflow(
    root: &Path,
    repository: &RemoteRepository,
    workflow: &str,
    reference: &str,
    inputs: Value,
) -> RailResult<u64> {
    let host = repository
        .host()
        .ok_or_else(|| RailError::message("workflow dispatch requires GitHub"))?;
    let id = validation::workflow_id(root, host, repository.path(), workflow)?;
    let mut input = tempfile::NamedTempFile::new()?;
    input.write_all(&serde_json::to_vec(&json!({"ref":reference,"inputs":inputs}))?)?;
    let output = process::run(
        "gh",
        &[
            "api",
            "--hostname",
            host,
            "-H",
            "X-GitHub-Api-Version: 2026-03-10",
            "--method",
            "POST",
            &format!("repos/{}/actions/workflows/{id}/dispatches", repository.path()),
            "--input",
            input
                .path()
                .to_str()
                .ok_or_else(|| RailError::message("workflow request path is not UTF-8"))?,
        ],
        Some(root),
    )?;
    if !output.status.success() {
        return Err(RailError::message(
            "workflow dispatch was not acknowledged; the original request is retained for resume",
        ));
    }
    let response: Value = contract::decode(&output.stdout)?;
    let run_id = validation::positive(&response, "workflow_run_id")?;
    let run = validation::api(
        root,
        host,
        &format!("repos/{}/actions/runs/{run_id}", repository.path()),
    )?;
    if run["workflow_id"] != id
        || run["event"] != "workflow_dispatch"
        || run["repository"]["full_name"]
            .as_str()
            .is_none_or(|name| !name.eq_ignore_ascii_case(repository.path()))
    {
        return Err(RailError::message(
            "dispatched workflow does not match its authorized producer",
        ));
    }
    Ok(run_id)
}

pub(crate) fn dispatch_validation(root: &Path, state: &mut ReleaseState, path: &Path) -> RailResult<()> {
    if !state.intent.hosted || state.intent.skip_tag && state.intent.skip_publish {
        return Ok(());
    }
    let repository = state
        .intent
        .remote_repository
        .clone()
        .ok_or_else(|| RailError::message("hosted validation has no repository"))?;
    let source = state
        .release_commit()
        .ok_or_else(|| RailError::message("validation has no prepared source"))?
        .to_owned();
    for workflow in state
        .intent
        .release_config
        .validation
        .keys()
        .cloned()
        .collect::<Vec<_>>()
    {
        if state.validation_dispatches.contains_key(&workflow) {
            continue;
        }
        let workflow_id = validation::workflow_id(root, "github.com", repository.path(), &workflow)?;
        if let Some(existing) = validation::select_run(root, "github.com", repository.path(), workflow_id, &source)? {
            state
                .validation_dispatches
                .insert(workflow, validation::positive(&existing, "id")?);
            state.save(path)?;
            continue;
        }
        let git = SystemGit::open(root)?;
        if review::remote_head(&git, &format!("refs/heads/{}", state.intent.branch))?.as_deref() != Some(&source) {
            return Err(RailError::message(
                "validation dispatch requires the exact release commit at the authorized branch tip",
            ));
        }
        let run = dispatch_workflow(root, &repository, &workflow, &state.intent.branch, json!({}))?;
        let observed = validation::api(
            root,
            "github.com",
            &format!("repos/{}/actions/runs/{}", repository.path(), run),
        )?;
        if observed["head_sha"] != source {
            return Err(RailError::message(
                "branch moved during validation dispatch; the wrong source cannot authorize publication",
            ));
        }
        state.validation_dispatches.insert(workflow, run);
        state.save(path)?;
    }
    Ok(())
}

pub(crate) fn event(root: &Path, config: &ReleaseConfig) -> RailResult<Value> {
    let repository = super::remote::release_repository(root)?;
    let workflow = config
        .hosted_workflow
        .as_deref()
        .ok_or_else(|| RailError::message("hosted executor requires release.hosted_workflow"))?;
    let expected = format!("{}/{workflow}@refs/heads/", repository.path());
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || std::env::var("GITHUB_REPOSITORY").as_deref() != Ok(repository.path())
        || std::env::var("GITHUB_WORKFLOW_REF")
            .ok()
            .is_none_or(|value| !value.starts_with(&expected))
        || !matches!(
            std::env::var("GITHUB_EVENT_NAME").as_deref(),
            Ok("workflow_dispatch" | "pull_request_target")
        )
    {
        return Err(RailError::message(
            "release executor is outside its configured GitHub workflow and repository",
        ));
    }
    let path =
        std::env::var("GITHUB_EVENT_PATH").map_err(|_| RailError::message("release executor has no event payload"))?;
    if std::fs::metadata(&path)?.len() > contract::MAX_RECORD_BYTES as u64 {
        return Err(RailError::message("release event exceeds its size limit"));
    }
    contract::decode(&std::fs::read(path)?)
}

pub(crate) fn checkout(root: &Path, state: &ReleaseState) -> RailResult<()> {
    let event = event(root, &state.intent.release_config)?;
    let repository = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("executor has no repository"))?;
    let expected = format!(
        "{}/{}@refs/heads/{}",
        repository.path(),
        state.intent.release_config.hosted_workflow.as_deref().unwrap_or(""),
        state.intent.branch
    );
    if std::env::var("GITHUB_WORKFLOW_REF").as_deref() != Ok(&expected) {
        return Err(RailError::message(
            "executor workflow branch differs from the authorized release branch",
        ));
    }
    let mut observed_merge = None;
    let source = if std::env::var("GITHUB_EVENT_NAME").as_deref() == Ok("pull_request_target") {
        let review = state
            .review
            .as_ref()
            .ok_or_else(|| RailError::message("release has no review authorization"))?;
        if event["action"] != "closed"
            || event["pull_request"]["merged"] != true
            || event["number"].as_u64() != review.request
            || event["pull_request"]["head"]["sha"] != serde_json::to_value(&state.preparation)?["commit"]
            || event["pull_request"]["base"]["ref"] != state.intent.branch
        {
            return Err(RailError::message(
                "merge event does not continue the retained release review",
            ));
        }
        event["pull_request"]["merge_commit_sha"]
            .as_str()
            .ok_or_else(|| RailError::message("merge event has no commit"))?
    } else {
        if event["inputs"]["transaction"] != state.transaction_id
            || event["inputs"]["intent"] != state.intent.identity
            || (event["inputs"]["source"] != state.release_commit().unwrap_or(&state.intent.initial_head)
                && event["inputs"]["source"] != state.intent.initial_head)
        {
            return Err(RailError::message(
                "workflow event does not authorize the original release request",
            ));
        }
        observed_merge = review::merged_source(root, state)?;
        observed_merge
            .as_deref()
            .unwrap_or_else(|| state.release_commit().unwrap_or(&state.intent.initial_head))
    };
    if !matches!(source.len(), 40 | 64) || !source.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RailError::message("executor source is not an exact Git commit"));
    }
    let git = SystemGit::open(root)?;
    if git.is_dirty()? {
        return Err(RailError::message("hosted checkout requires a clean worktree"));
    }
    git.run_git(&["fetch", "--no-tags", "--no-write-fetch-head", "origin", source])?;
    let branch = if observed_merge.is_none()
        && state.review.is_some()
        && state.review.as_ref().is_some_and(|review| review.merge.is_none())
        && std::env::var("GITHUB_EVENT_NAME").as_deref() != Ok("pull_request_target")
        && state.release_commit().is_some()
    {
        review::branch(state)
    } else {
        state.intent.branch.clone()
    };
    git.run_git(&["switch", "-C", &branch, source])?;
    Ok(())
}

pub(crate) fn url(state: &ReleaseState) -> Option<String> {
    state
        .executor
        .zip(state.intent.remote_repository.as_ref())
        .map(|(run, repo)| format!("https://github.com/{}/actions/runs/{run}", repo.path()))
}

pub(crate) fn refresh(root: &Path, transaction: Option<&str>) -> RailResult<()> {
    let directory = super::state::state_dir(root);
    if directory.try_exists()? {
        let records = std::fs::read_dir(&directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
            .map(|path| ReleaseState::load(&path))
            .collect::<RailResult<Vec<_>>>()?;
        if !records.is_empty() && records.iter().all(|state| !state.remote_storage) {
            return Ok(());
        }
    }
    let git = SystemGit::open(root)?;
    if !git.run_git_check(&["remote", "get-url", "origin"]) {
        return Ok(());
    }
    let reference = transaction
        .map(super::storage::reference)
        .transpose()?
        .unwrap_or_else(|| super::storage::ACTIVE.to_owned());
    if review::remote_head(&git, &reference)?.is_some() {
        super::transfer::fetch(root, transaction)?;
    }
    Ok(())
}
