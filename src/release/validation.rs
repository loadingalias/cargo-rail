//! Exact GitHub workflow attempts that authorize release effects.

use super::{process, remote::RemoteRepository};
use crate::error::{RailError, RailResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowRun {
    pub workflow: String,
    pub workflow_id: u64,
    pub run_id: u64,
    pub attempt: u64,
    pub jobs: Vec<WorkflowJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowJob {
    pub name: String,
    pub id: u64,
}

pub(crate) enum Observation {
    Verified(Vec<WorkflowRun>),
    Waiting(String),
    Failed(String),
}

pub(crate) fn observe(
    root: &Path,
    repository: &RemoteRepository,
    source: &str,
    required: &BTreeMap<String, Vec<String>>,
    retained: &[WorkflowRun],
    dispatched: &BTreeMap<String, u64>,
) -> RailResult<Observation> {
    if required.is_empty() {
        return Err(RailError::with_help(
            "GitHub release validation has no required workflows",
            "configure release.validation with workflow paths and required job names before preparing the release",
        ));
    }
    let host = repository
        .host()
        .ok_or_else(|| RailError::message("GitHub validation requires a hosted repository"))?;
    let repository = repository.path();
    let current_run = std::env::var("GITHUB_RUN_ID")
        .ok()
        .and_then(|id| id.parse::<u64>().ok());
    let mut verified = Vec::new();
    for (workflow, required_jobs) in required {
        let workflow_id = workflow_id(root, host, repository, workflow)?;
        let run = if let Some(previous) = retained.iter().find(|run| run.workflow == *workflow) {
            api(
                root,
                host,
                &format!("repos/{repository}/actions/runs/{}", previous.run_id),
            )?
        } else if let Some(dispatched) = dispatched.get(workflow) {
            api(root, host, &format!("repos/{repository}/actions/runs/{dispatched}"))?
        } else {
            let Some(selected) = select_run(root, host, repository, workflow_id, source)? else {
                return Ok(Observation::Waiting(format!(
                    "{workflow} has no validation run for {source}"
                )));
            };
            selected
        };
        let run_id = positive(&run, "id")?;
        let attempt = positive(&run, "run_attempt")?;
        if dispatched.get(workflow).is_some_and(|dispatch| *dispatch != run_id) {
            return Err(RailError::message("dispatched validation run changed"));
        }
        if Some(run_id) == current_run {
            return Err(RailError::message(
                "a release executor cannot authorize itself as validation",
            ));
        }
        if retained
            .iter()
            .find(|run| run.workflow == *workflow)
            .is_some_and(|previous| {
                previous.run_id != run_id || previous.attempt != attempt || previous.workflow_id != workflow_id
            })
        {
            return Err(RailError::message(
                "retained validation attempt changed; the release evidence cannot be replaced",
            ));
        }
        let exact = api(
            root,
            host,
            &format!("repos/{repository}/actions/runs/{run_id}/attempts/{attempt}"),
        )?;
        match inspect_run(&exact, repository, source, workflow, workflow_id, run_id, attempt)? {
            RunStatus::Waiting => {
                return Ok(Observation::Waiting(format!(
                    "{workflow} run {run_id}, attempt {attempt} is incomplete"
                )));
            }
            RunStatus::Failed => {
                return Ok(Observation::Failed(format!(
                    "{workflow} run {run_id}, attempt {attempt} did not succeed"
                )));
            }
            RunStatus::Succeeded => {}
        }
        let jobs = api(
            root,
            host,
            &format!("repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"),
        )?;
        let jobs = inspect_jobs(&jobs, required_jobs, run_id, attempt, source)?;
        verified.push(WorkflowRun {
            workflow: workflow.clone(),
            workflow_id,
            run_id,
            attempt,
            jobs,
        });
    }
    if !retained.is_empty() && retained != verified {
        return Err(RailError::message(
            "validation results differ from retained release evidence",
        ));
    }
    Ok(Observation::Verified(verified))
}

pub(crate) fn validate_records(required: &BTreeMap<String, Vec<String>>, runs: &[WorkflowRun]) -> RailResult<()> {
    if runs.is_empty() {
        return Ok(());
    }
    if runs.len() != required.len() {
        return Err(RailError::message(
            "release validation evidence does not cover the required workflows",
        ));
    }
    let mut ids = BTreeSet::new();
    for ((workflow, jobs), run) in required.iter().zip(runs) {
        if &run.workflow != workflow
            || run.workflow_id == 0
            || run.run_id == 0
            || run.attempt == 0
            || !ids.insert(run.run_id)
            || run.jobs.len() != jobs.len()
            || run
                .jobs
                .iter()
                .zip(jobs)
                .any(|(actual, expected)| &actual.name != expected || actual.id == 0)
            || run.jobs.iter().map(|job| job.id).collect::<BTreeSet<_>>().len() != jobs.len()
        {
            return Err(RailError::message(
                "release validation evidence has inconsistent workflow or job identities",
            ));
        }
    }
    Ok(())
}

enum RunStatus {
    Waiting,
    Failed,
    Succeeded,
}

fn inspect_run(
    run: &Value,
    repository: &str,
    source: &str,
    workflow: &str,
    workflow_id: u64,
    run_id: u64,
    attempt: u64,
) -> RailResult<RunStatus> {
    if run["id"] != run_id
        || run["run_attempt"] != attempt
        || run["workflow_id"] != workflow_id
        || run["head_sha"] != source
        || run["path"].as_str().is_none_or(|path| {
            path != workflow
                && path
                    .strip_prefix(workflow)
                    .is_none_or(|suffix| !suffix.starts_with('@') || suffix.len() == 1)
        })
        || run["head_repository"]["full_name"]
            .as_str()
            .is_none_or(|name| !name.eq_ignore_ascii_case(repository))
        || run["repository"]["full_name"]
            .as_str()
            .is_none_or(|name| !name.eq_ignore_ascii_case(repository))
        || !matches!(
            run["event"].as_str(),
            Some("push" | "workflow_dispatch" | "workflow_call")
        )
    {
        return Err(RailError::message(
            "validation run does not match its required workflow, attempt, repository, and release commit",
        ));
    }
    match run["status"].as_str() {
        Some("completed") if run["conclusion"] == "success" => Ok(RunStatus::Succeeded),
        Some("completed") => Ok(RunStatus::Failed),
        Some("queued" | "requested" | "waiting" | "pending" | "in_progress") => Ok(RunStatus::Waiting),
        _ => Err(RailError::message("validation run has an unavailable status")),
    }
}

fn inspect_jobs(
    value: &Value,
    required: &[String],
    run_id: u64,
    attempt: u64,
    source: &str,
) -> RailResult<Vec<WorkflowJob>> {
    let rows = bounded_rows(value, "jobs")?;
    let mut jobs = Vec::new();
    for name in required {
        let mut matching = rows.iter().filter(|job| job["name"] == *name);
        let job = matching
            .next()
            .ok_or_else(|| RailError::message(format!("required validation job '{name}' is missing")))?;
        if matching.next().is_some()
            || job["run_id"] != run_id
            || job.get("run_attempt").is_some_and(|value| value != attempt)
            || job["head_sha"] != source
            || job["status"] != "completed"
            || job["conclusion"] != "success"
        {
            return Err(RailError::message(format!(
                "required validation job '{name}' is ambiguous, skipped, incomplete, failed, or incorrectly bound"
            )));
        }
        jobs.push(WorkflowJob {
            name: name.clone(),
            id: positive(job, "id")?,
        });
    }
    Ok(jobs)
}

pub(super) fn bounded_rows<'a>(value: &'a Value, field: &str) -> RailResult<&'a [Value]> {
    let rows = value[field]
        .as_array()
        .ok_or_else(|| RailError::message("GitHub validation response has no result array"))?;
    if rows.len() > 100 || value["total_count"].as_u64() != Some(rows.len() as u64) {
        return Err(RailError::message(
            "GitHub validation response is incomplete or exceeds its 100-result bound",
        ));
    }
    Ok(rows)
}

pub(super) fn positive(value: &Value, field: &str) -> RailResult<u64> {
    value[field]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or_else(|| RailError::message(format!("GitHub validation has no positive {field}")))
}

pub(super) fn api(root: &Path, host: &str, path: &str) -> RailResult<Value> {
    let output = process::run("gh", &["api", "--hostname", host, path], Some(root))?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "GitHub validation observation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    super::contract::decode(&output.stdout)
}

pub(super) fn workflow_id(root: &Path, host: &str, repository: &str, workflow: &str) -> RailResult<u64> {
    let filename = workflow
        .rsplit('/')
        .next()
        .ok_or_else(|| RailError::message("invalid validation workflow"))?;
    let descriptor = api(root, host, &format!("repos/{repository}/actions/workflows/{filename}"))?;
    if descriptor["path"] != workflow || descriptor["state"] != "active" {
        return Err(RailError::message(
            "required workflow is unavailable or has another path",
        ));
    }
    positive(&descriptor, "id")
}

pub(super) fn select_run(
    root: &Path,
    host: &str,
    repository: &str,
    workflow_id: u64,
    source: &str,
) -> RailResult<Option<Value>> {
    let runs = api(
        root,
        host,
        &format!("repos/{repository}/actions/workflows/{workflow_id}/runs?head_sha={source}&per_page=100"),
    )?;
    let rows = bounded_rows(&runs, "workflow_runs")?;
    Ok(rows
        .iter()
        .filter(|run| {
            run["head_sha"] == source
                && matches!(
                    run["event"].as_str(),
                    Some("push" | "workflow_dispatch" | "workflow_call")
                )
        })
        .max_by_key(|run| run["id"].as_u64())
        .cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Value {
        serde_json::json!({"id": 42, "run_attempt": 3, "workflow_id": 7, "head_sha": "a".repeat(40),
            "path": ".github/workflows/ci.yml", "repository":{"full_name":"example/repository"},
            "head_repository":{"full_name":"example/repository"}, "event":"workflow_dispatch", "status":"completed", "conclusion":"success"})
    }

    #[test]
    fn workflow_authorization_rejects_wrong_source_workflow_attempt_and_repository() {
        let valid = run();
        assert!(matches!(
            inspect_run(
                &valid,
                "example/repository",
                &"a".repeat(40),
                ".github/workflows/ci.yml",
                7,
                42,
                3
            )
            .unwrap(),
            RunStatus::Succeeded
        ));
        for (field, replacement) in [
            ("id", serde_json::json!(43)),
            ("run_attempt", serde_json::json!(2)),
            ("workflow_id", serde_json::json!(8)),
            ("head_sha", serde_json::json!("b".repeat(40))),
            ("path", serde_json::json!(".github/workflows/release.yml")),
            (
                "head_repository",
                serde_json::json!({"full_name":"attacker/repository"}),
            ),
            ("event", serde_json::json!("pull_request")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = replacement;
            let error = inspect_run(
                &invalid,
                "example/repository",
                &"a".repeat(40),
                ".github/workflows/ci.yml",
                7,
                42,
                3,
            )
            .err()
            .expect("wrong binding must fail");
            assert!(error.to_string().contains("does not match"), "{field}: {error}");
        }
    }

    #[test]
    fn required_jobs_reject_skipped_missing_duplicate_and_wrong_attempt() {
        let valid = serde_json::json!({"total_count":1,"jobs":[{"id":17,"name":"tests","run_id":42,"run_attempt":3,"head_sha":"a".repeat(40),"status":"completed","conclusion":"success"}]});
        assert_eq!(
            inspect_jobs(&valid, &["tests".to_owned()], 42, 3, &"a".repeat(40)).unwrap(),
            vec![WorkflowJob {
                name: "tests".to_owned(),
                id: 17
            }]
        );
        let mut documented_response = valid.clone();
        documented_response["jobs"][0]
            .as_object_mut()
            .unwrap()
            .remove("run_attempt");
        assert_eq!(
            inspect_jobs(&documented_response, &["tests".to_owned()], 42, 3, &"a".repeat(40)).unwrap(),
            vec![WorkflowJob {
                name: "tests".to_owned(),
                id: 17
            }],
        );
        for conclusion in ["skipped", "neutral", "failure", "cancelled"] {
            let mut invalid = valid.clone();
            invalid["jobs"][0]["conclusion"] = serde_json::json!(conclusion);
            assert!(
                inspect_jobs(&invalid, &["tests".to_owned()], 42, 3, &"a".repeat(40))
                    .unwrap_err()
                    .to_string()
                    .contains("required validation job")
            );
        }
        let mut invalid = valid.clone();
        invalid["jobs"].as_array_mut().unwrap().push(valid["jobs"][0].clone());
        invalid["total_count"] = serde_json::json!(2);
        assert!(
            inspect_jobs(&invalid, &["tests".to_owned()], 42, 3, &"a".repeat(40))
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(
            inspect_jobs(&valid, &["absent".to_owned()], 42, 3, &"a".repeat(40))
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert!(
            inspect_jobs(&valid, &["tests".to_owned()], 42, 4, &"a".repeat(40))
                .unwrap_err()
                .to_string()
                .contains("incorrectly bound")
        );
    }
}
