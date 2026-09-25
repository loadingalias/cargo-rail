//! `cargo rail plan --cases`: compare reviewed path cases with planner decisions.
//!
//! Each case becomes an unreferenced commit whose tree is `HEAD` plus the case's
//! file changes. The planner compares `HEAD` with that commit in object mode, so
//! the worktree, the index, refs, and untracked files never affect or receive a
//! case. Git can garbage-collect the unreferenced objects afterward.
//!
//! The result is diagnostic. A passing case never becomes negative evidence.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::planning::{WorkCause, WorkDecision, WorkInputKind, WorkPlan};
use crate::workspace::WorkspaceContext;

use super::{PlanComparison, PlanOptions};

const MAX_CASE_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseFile {
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    change: Vec<String>,
    #[serde(default = "default_append")]
    append: String,
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    skipped: Vec<String>,
    #[serde(default)]
    precise: bool,
}

fn default_append() -> String {
    "\n".to_string()
}

/// Plan every case in `cases_file` against `HEAD` and report route parity.
pub(crate) fn run_plan_cases(
    workspace_root: &Path,
    config_override: Option<&Path>,
    cases_file: &Path,
) -> RailResult<()> {
    let cases = read_cases(cases_file)?;
    let git = SystemGit::open(workspace_root)?;
    let head = git.head_commit()?;
    let scratch = tempfile::Builder::new()
        .prefix("cargo-rail-plan-cases-")
        .tempdir()
        .map_err(|error| RailError::message(format!("failed to create a case index directory: {error}")))?;

    let mut report = String::new();
    let mut failed = 0usize;
    let mut expanded_only = 0usize;
    for (index, case) in cases.iter().enumerate() {
        let commit = case_commit(&git, &head, case, &scratch.path().join(format!("index-{index}")))?;
        let context =
            WorkspaceContext::build_historical_planning_with_config(workspace_root, &head, &commit, config_override)?;
        let plan = super::build_work_plan(
            &context,
            &PlanOptions {
                comparison: PlanComparison::Objects {
                    from: head.clone(),
                    to: commit,
                },
                json: false,
                explain: false,
                explain_work: None,
                all: false,
                evidence: None,
            },
        )?;
        let outcome = evaluate(case, &plan)?;
        failed += usize::from(!outcome.failures.is_empty());
        expanded_only += usize::from(outcome.failures.is_empty() && outcome.expanded_expectations);
        render_case(&mut report, case, &plan, &outcome);
    }
    report.push_str(&format!("\n{} of {} cases passed", cases.len() - failed, cases.len()));
    if expanded_only > 0 {
        report.push_str(&format!(
            "; {expanded_only} passed only through conservative expansion, not precise routing"
        ));
    }
    report.push('\n');

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(report.as_bytes())?;
    stdout.flush()?;
    if failed > 0 {
        return Err(RailError::ExitWithCode { code: 1 });
    }
    Ok(())
}

fn read_cases(path: &Path) -> RailResult<Vec<Case>> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| RailError::message(format!("failed to inspect case file '{}': {error}", path.display())))?;
    if !metadata.is_file() || metadata.len() > MAX_CASE_FILE_BYTES {
        return Err(RailError::message(format!(
            "case file '{}' must be a regular file no larger than {MAX_CASE_FILE_BYTES} bytes",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| RailError::message(format!("failed to read case file '{}': {error}", path.display())))?;
    let file: CaseFile = toml_edit::de::from_str(&text)
        .map_err(|error| RailError::message(format!("invalid case file '{}': {error}", path.display())))?;
    if file.cases.is_empty() {
        return Err(RailError::message(format!(
            "case file '{}' declares no [[case]]",
            path.display()
        )));
    }
    let mut names = BTreeSet::new();
    for case in &file.cases {
        let subject = format!("case '{}'", case.name);
        if case.name.is_empty() || !names.insert(case.name.as_str()) {
            return Err(RailError::message(format!(
                "case names must be unique and nonempty: '{}'",
                case.name
            )));
        }
        if case.change.is_empty() {
            return Err(RailError::message(format!("{subject} changes no path")));
        }
        for path in &case.change {
            if !valid_repository_path(path) {
                return Err(RailError::message(format!(
                    "{subject} path '{path}' must be repository-relative with '/' separators"
                )));
            }
        }
        if case.append.is_empty() {
            return Err(RailError::message(format!("{subject} must append at least one byte")));
        }
        if let Some(work) = case.required.iter().find(|work| case.skipped.contains(work)) {
            return Err(RailError::message(format!(
                "{subject} expects '{work}' to be both required and skipped"
            )));
        }
    }
    Ok(file.cases)
}

fn valid_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// Write the case's blobs, tree, and commit without touching refs or the real index.
fn case_commit(git: &SystemGit, head: &str, case: &Case, index: &Path) -> RailResult<String> {
    let index = index
        .to_str()
        .ok_or_else(|| RailError::message("case index path is not valid UTF-8"))?;
    let run = |args: &[&str], input: Option<&[u8]>| -> RailResult<Vec<u8>> {
        let mut command = git.git_cmd();
        command
            .env("GIT_INDEX_FILE", index)
            .env("GIT_AUTHOR_NAME", "cargo-rail")
            .env("GIT_AUTHOR_EMAIL", "cargo-rail@invalid")
            .env("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "cargo-rail")
            .env("GIT_COMMITTER_EMAIL", "cargo-rail@invalid")
            .env("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z")
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run_with_input(command, input)?;
        if !output.status.success() {
            return Err(RailError::message(format!(
                "git {} failed for case '{}': {}",
                args.first().copied().unwrap_or_default(),
                case.name,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    };
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).trim().to_string();

    run(&["read-tree", head], None)?;
    for path in &case.change {
        let listing = run(&["ls-tree", "-z", head, "--", path], None)?;
        let (mode, mut bytes) = match listing.split(|byte| *byte == 0).find(|entry| !entry.is_empty()) {
            None => ("100644".to_string(), Vec::new()),
            Some(entry) => {
                let entry = String::from_utf8_lossy(entry);
                let (header, _) = entry
                    .split_once('\t')
                    .ok_or_else(|| RailError::message(format!("unexpected git ls-tree output for '{path}'")))?;
                let fields = header.split(' ').collect::<Vec<_>>();
                if fields.len() != 3 || fields[1] != "blob" {
                    return Err(RailError::message(format!(
                        "case '{}' path '{path}' must name a file, not a directory or submodule",
                        case.name
                    )));
                }
                (fields[0].to_string(), run(&["cat-file", "blob", fields[2]], None)?)
            }
        };
        bytes.extend_from_slice(case.append.as_bytes());
        let object = text(run(&["hash-object", "-w", "--no-filters", "--stdin"], Some(&bytes))?);
        run(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},{object},{path}"),
            ],
            None,
        )?;
    }
    let tree = text(run(&["write-tree"], None)?);
    let message = format!("cargo-rail route parity case {}", case.name);
    Ok(text(run(
        &["commit-tree", "--no-gpg-sign", "-p", head, "-m", &message, &tree],
        None,
    )?))
}

fn run_with_input(mut command: Command, input: Option<&[u8]>) -> RailResult<std::process::Output> {
    let mut child = command
        .spawn()
        .map_err(|error| RailError::message(format!("failed to start git: {error}")))?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("git stdin was unavailable"))?
            .write_all(input)
            .map_err(|error| RailError::message(format!("failed to write git input: {error}")))?;
    }
    child
        .wait_with_output()
        .map_err(|error| RailError::message(format!("failed to read git output: {error}")))
}

struct Outcome {
    failures: Vec<String>,
    /// An expected required item was selected only because evidence was incomplete.
    expanded_expectations: bool,
}

fn evaluate(case: &Case, plan: &WorkPlan) -> RailResult<Outcome> {
    let mut failures = Vec::new();
    let mut expanded_expectations = false;
    for work in case.required.iter().chain(&case.skipped) {
        if !plan.work.contains_key(work) {
            return Err(RailError::message(format!(
                "case '{}' names unregistered work '{work}'",
                case.name
            )));
        }
    }
    for work in &case.required {
        match &plan.work[work] {
            WorkDecision::Skipped { .. } => failures.push(format!("missing required work {work}")),
            WorkDecision::Required {
                cause: WorkCause::IncompleteEvidence,
                ..
            } => {
                expanded_expectations = true;
                if case.precise {
                    failures.push(format!(
                        "{work} was required only by conservative expansion, not precise routing"
                    ));
                }
            }
            WorkDecision::Required { .. } => {}
        }
    }
    for work in &case.skipped {
        if let WorkDecision::Required { cause, .. } = &plan.work[work] {
            failures.push(format!(
                "expected skipped work {work} was required ({})",
                cause_label(*cause)
            ));
        }
    }
    Ok(Outcome {
        failures,
        expanded_expectations,
    })
}

fn cause_label(cause: WorkCause) -> &'static str {
    match cause {
        WorkCause::ChangedInput => "direct",
        WorkCause::IncompleteEvidence => "expanded: incomplete evidence",
        WorkCause::ForcedAll => "forced by --all",
    }
}

fn render_case(report: &mut String, case: &Case, plan: &WorkPlan, outcome: &Outcome) {
    let status = if outcome.failures.is_empty() { "pass" } else { "FAIL" };
    report.push_str(&format!(
        "{status}  {}  (changed: {})\n",
        case.name,
        case.change.join(", ")
    ));
    for failure in &outcome.failures {
        report.push_str(&format!("  error: {failure}\n"));
    }
    let mut skipped = Vec::new();
    for (work, decision) in &plan.work {
        match decision {
            WorkDecision::Skipped { .. } => skipped.push(work.as_str()),
            WorkDecision::Required { cause, .. } => {
                let paths = plan.attribution.get(work).map_or_else(BTreeSet::new, |attribution| {
                    attribution
                        .inputs
                        .iter()
                        .filter(|input| input.kind == WorkInputKind::Path)
                        .map(|input| input.value.as_str())
                        .collect()
                });
                let paths = paths.into_iter().collect::<Vec<_>>().join(", ");
                report.push_str(&format!("  required  {work}  {}", cause_label(*cause)));
                if !paths.is_empty() {
                    report.push_str(&format!("  from {paths}"));
                }
                report.push('\n');
            }
        }
    }
    if !skipped.is_empty() {
        report.push_str(&format!("  skipped   {}\n", skipped.join(", ")));
    }
    let expansions = plan
        .work
        .values()
        .filter_map(|decision| match decision {
            WorkDecision::Required {
                cause: WorkCause::IncompleteEvidence,
                evidence,
                ..
            } => Some(evidence),
            _ => None,
        })
        .flatten()
        .filter_map(|id| plan.evidence.get(id))
        .filter(|record| !record.complete)
        .map(|record| record.description.as_str())
        .collect::<BTreeSet<_>>();
    for reason in expansions {
        report.push_str(&format!("  expanded because {reason}\n"));
    }
}
