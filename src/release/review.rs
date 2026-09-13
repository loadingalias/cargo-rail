//! Reviewed preparation and exact merged-source binding for the release transaction.

use super::{
    process,
    state::{Preparation, ReleasePhase, ReleaseState, Step, StepStatus},
    validation,
};
use crate::{
    error::{RailError, RailResult},
    git::SystemGit,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Write, path::Path};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewState {
    pub pushed: Step,
    pub request: Option<u64>,
    pub merge: Option<ReviewedMerge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewedMerge {
    pub commit: String,
    pub tree: String,
}

pub(crate) fn branch(state: &ReleaseState) -> String {
    format!("rail/{}", state.transaction_id)
}

pub(crate) fn validate_record(state: &ReleaseState) -> RailResult<()> {
    if state.intent.review != state.review.is_some() {
        return Err(RailError::message(
            "review progress disagrees with the authorized release mode",
        ));
    }
    let Some(review) = &state.review else {
        return Ok(());
    };
    let prepared = match &state.preparation {
        Preparation::Complete { commit } => Some(commit),
        _ => None,
    };
    if !state.remote_storage
        || !state.intent.release_config.remote_effects.pushes()
        || state
            .intent
            .remote_repository
            .as_ref()
            .is_none_or(|repository| repository.host() != Some("github.com"))
        || review.pushed.status != StepStatus::Pending
            && (prepared.is_none() || review.pushed.object.as_ref() != prepared)
        || review.pushed.status == StepStatus::Pending && review.pushed.object.is_some()
        || review.request.is_some_and(|id| id == 0 || !review.pushed.is_complete())
        || review
            .merge
            .as_ref()
            .is_some_and(|merge| review.request.is_none() || !oid(&merge.commit) || !oid(&merge.tree))
        || review.merge.is_none()
            && (state.package_seal.is_some() || !state.validation.is_empty() || state.phase >= ReleasePhase::Ready)
    {
        return Err(RailError::message(
            "reviewed release has inconsistent source, authority, or effect ordering",
        ));
    }
    Ok(())
}

pub(crate) fn require_successor(previous: &ReleaseState, next: &ReleaseState) -> RailResult<()> {
    if let Some(previous) = &previous.review {
        let next = next
            .review
            .as_ref()
            .ok_or_else(|| RailError::message("cannot remove release review progress"))?;
        if previous.request.is_some() && previous.request != next.request
            || previous.merge.is_some() && previous.merge != next.merge
            || previous.pushed.object.is_some() && previous.pushed.object != next.pushed.object
            || previous.pushed.is_complete() && !next.pushed.is_complete()
            || previous.pushed.status == StepStatus::InProgress && next.pushed.status == StepStatus::Pending
        {
            return Err(RailError::message(
                "release review progress conflicts with retained evidence",
            ));
        }
    }
    Ok(())
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn prepare_branch(git: &SystemGit, state: &ReleaseState) -> RailResult<()> {
    if state.review.is_none() || !matches!(state.preparation, Preparation::Pending) {
        return Ok(());
    }
    let branch = branch(state);
    if git.head_commit()? != state.intent.initial_head {
        return Err(RailError::message("review preparation source moved"));
    }
    if git.current_branch()? == branch {
        return Ok(());
    }
    if git.current_branch()? != state.intent.branch
        || git.run_git_check(&["show-ref", "--verify", &format!("refs/heads/{branch}")])
    {
        return Err(RailError::message(
            "release review branch already exists or checkout branch changed",
        ));
    }
    git.run_git(&["switch", "-c", &branch, &state.intent.initial_head])?;
    Ok(())
}

pub(crate) fn reconcile(git: &SystemGit, state: &mut ReleaseState, path: &Path) -> RailResult<bool> {
    if state.review.is_none() {
        return Ok(true);
    }
    let prepared = match &state.preparation {
        Preparation::Complete { commit } => commit.clone(),
        _ => return Err(RailError::message("review has no prepared commit")),
    };
    let repository = state
        .intent
        .remote_repository
        .clone()
        .ok_or_else(|| RailError::message("review has no repository"))?;
    let host = repository
        .host()
        .ok_or_else(|| RailError::message("review has no GitHub host"))?;
    let branch = branch(state);
    let reference = format!("refs/heads/{branch}");
    if !state.review.as_ref().expect("review mode").pushed.is_complete() {
        let step = &mut state.review.as_mut().expect("review mode").pushed;
        step.status = StepStatus::InProgress;
        step.object = Some(prepared.clone());
        state.save(path)?;
        match remote_head(git, &reference)? {
            Some(head) if head != prepared => {
                return Err(RailError::message(
                    "remote review branch differs from the prepared commit",
                ));
            }
            Some(_) => {}
            None => {
                git.run_git_observable_with_env(
                    &[
                        "push",
                        "--atomic",
                        &format!("--force-with-lease={reference}:"),
                        "origin",
                        &format!("{prepared}:{reference}"),
                    ],
                    super::publisher::RELEASE_PUSH_ENV,
                )?;
            }
        }
        if remote_head(git, &reference)?.as_deref() != Some(&prepared) {
            return Err(RailError::message("review branch push is not observable"));
        }
        state.review.as_mut().expect("review mode").pushed.status = StepStatus::Complete;
        state.save(path)?;
    }
    let root = &git.worktree_root;
    let pull = if let Some(number) = state.review.as_ref().expect("review mode").request {
        validation::api(root, host, &format!("repos/{}/pulls/{number}", repository.path()))?
    } else {
        let owner = repository
            .path()
            .split('/')
            .next()
            .ok_or_else(|| RailError::message("review has no owner"))?;
        let endpoint = format!(
            "repos/{}/pulls?state=all&head={owner}:{}&base={}&per_page=100",
            repository.path(),
            encode(&branch),
            encode(&state.intent.branch)
        );
        let mut found = find_request(&validation::api(root, host, &endpoint)?)?;
        if found.is_none() {
            let body = json!({"title":format!("Release {}", state.intent.plan.crates.iter().map(|package| format!("{} {}",package.name,package.new_version)).collect::<Vec<_>>().join(", ")),
                "head":branch,"base":state.intent.branch,"body":format!("Release transaction `{}`.\n\nIntent: `{}`.\n\nThe merged tree must match prepared commit `{prepared}`; validation and packaging run on the merged commit.", state.transaction_id,state.intent.identity)});
            let mut input = tempfile::NamedTempFile::new()?;
            input.write_all(&serde_json::to_vec(&body)?)?;
            let result = process::run(
                "gh",
                &[
                    "api",
                    "--hostname",
                    host,
                    "--method",
                    "POST",
                    &format!("repos/{}/pulls", repository.path()),
                    "--input",
                    input
                        .path()
                        .to_str()
                        .ok_or_else(|| RailError::message("review request path is not UTF-8"))?,
                ],
                Some(root),
            )?;
            if !result.status.success() {
                return Err(RailError::message(
                    "release review request was not acknowledged; resume to reconcile",
                ));
            }
            found = find_request(&validation::api(root, host, &endpoint)?)?;
        }
        let found = found.ok_or_else(|| RailError::message("release review request is not observable"))?;
        let number = validation::positive(&found, "number")?;
        validation::api(root, host, &format!("repos/{}/pulls/{number}", repository.path()))?
    };
    let number = validate_pull(state, &pull)?;
    state.review.as_mut().expect("review mode").request = Some(number);
    if state.phase < ReleasePhase::AwaitingReview {
        state.phase = ReleasePhase::AwaitingReview;
    }
    state.save(path)?;
    let Some(commit) = merged_commit(state, &pull)? else {
        crate::progress!(
            "release {} awaits review: https://{}/{}/pull/{number}",
            state.transaction_id,
            host,
            repository.path()
        );
        return Ok(false);
    };
    if git.head_commit()? != commit {
        return Err(RailError::message(format!(
            "review continuation requires checkout of merged commit {commit}"
        )));
    }
    let tree = git.run_git_stdout(&["show", "-s", "--format=%T", commit])?;
    if tree != git.run_git_stdout(&["show", "-s", "--format=%T", &prepared])? {
        return Err(RailError::message(
            "merged release tree differs from the reviewed preparation; a new reviewed intent is required",
        ));
    }
    let tip = remote_head(git, &format!("refs/heads/{}", state.intent.branch))?
        .ok_or_else(|| RailError::message("review base branch is missing"))?;
    git.run_git(&["fetch", "--no-tags", "--no-write-fetch-head", "origin", &tip])?;
    if !git.run_git_check(&["merge-base", "--is-ancestor", commit, &tip]) {
        return Err(RailError::message(
            "reviewed merge is no longer contained in its authorized base branch",
        ));
    }
    state.review.as_mut().expect("review mode").merge = Some(ReviewedMerge {
        commit: commit.to_owned(),
        tree,
    });
    state.commit_push.status = StepStatus::Complete;
    state.commit_push.object = Some(commit.to_owned());
    state.save(path)?;
    Ok(true)
}

/// Observe a retained review before choosing the hosted continuation checkout.
pub(crate) fn merged_source(root: &Path, state: &ReleaseState) -> RailResult<Option<String>> {
    let Some(number) = state.review.as_ref().and_then(|review| review.request) else {
        return Ok(None);
    };
    let repository = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("review has no repository"))?;
    let pull = validation::api(
        root,
        "github.com",
        &format!("repos/{}/pulls/{number}", repository.path()),
    )?;
    validate_pull(state, &pull)?;
    Ok(merged_commit(state, &pull)?.map(str::to_owned))
}

fn validate_pull(state: &ReleaseState, pull: &Value) -> RailResult<u64> {
    let prepared = match &state.preparation {
        Preparation::Complete { commit } => commit,
        _ => return Err(RailError::message("review has no prepared commit")),
    };
    let branch = branch(state);
    let repository = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("review has no repository"))?;
    let number = validation::positive(pull, "number")?;
    if pull["head"]["sha"] != *prepared
        || pull["head"]["ref"] != branch
        || pull["base"]["ref"] != state.intent.branch
        || !same_repository(&pull["head"]["repo"]["full_name"], repository.path())
        || !same_repository(&pull["base"]["repo"]["full_name"], repository.path())
        || state
            .review
            .as_ref()
            .expect("review mode")
            .request
            .is_some_and(|id| id != number)
    {
        return Err(RailError::message(
            "release review request changed its repository, branch, or prepared source",
        ));
    }
    Ok(number)
}

fn merged_commit<'a>(state: &ReleaseState, pull: &'a Value) -> RailResult<Option<&'a str>> {
    let retained = state.review.as_ref().and_then(|review| review.merge.as_ref());
    if pull["merged"] == false && pull["state"] == "open" && retained.is_none() {
        return Ok(None);
    }
    if pull["merged"] != true || pull["state"] != "closed" || pull["merged_at"].as_str().is_none_or(str::is_empty) {
        return Err(RailError::message("release review has no observable merge"));
    }
    let commit = pull["merge_commit_sha"]
        .as_str()
        .filter(|sha| oid(sha))
        .ok_or_else(|| RailError::message("review has no exact merged commit"))?;
    if retained.is_some_and(|merge| merge.commit != commit) {
        return Err(RailError::message("release review changed its retained merged commit"));
    }
    Ok(Some(commit))
}

fn same_repository(value: &Value, expected: &str) -> bool {
    value.as_str().is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn find_request(value: &Value) -> RailResult<Option<Value>> {
    let rows = value
        .as_array()
        .ok_or_else(|| RailError::message("review lookup has no complete result array"))?;
    if rows.len() > 1 {
        return Err(RailError::message("release review lookup is ambiguous or incomplete"));
    }
    Ok(rows.first().cloned())
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

pub(crate) fn remote_head(git: &SystemGit, reference: &str) -> RailResult<Option<String>> {
    let response = git.run_git_stdout(&["ls-remote", "--refs", "origin", reference])?;
    if response.is_empty() {
        return Ok(None);
    }
    let (head, actual_ref) = response
        .split_once('\t')
        .ok_or_else(|| RailError::message("remote review ref is malformed"))?;
    if actual_ref != reference || !oid(head) {
        return Err(RailError::message("remote review ref is ambiguous"));
    }
    Ok(Some(head.to_owned()))
}
