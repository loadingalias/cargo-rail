//! API evidence from the external cargo-semver-checks executable.

use crate::config::SemverCheckPolicy;
use crate::error::{RailError, RailResult};
use crate::release::process;
use crate::release::version::BumpLevel;
use crate::workspace::WorkspaceContext;
use serde::{Deserialize, Serialize};

/// The summary line cargo-semver-checks prints when breaking changes require
/// a major version bump.
const BREAKING_MARKER: &str = "semver requires new major version";

/// Outcome of checking one crate against its previous public API.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SemverCheck {
    /// No semver-breaking API changes detected.
    Pass,
    /// Breaking API changes require a major version bump.
    Breaking {
        /// Human-readable summary from the checker.
        message: String,
    },
    /// The check could not produce a verdict (missing baseline, network or
    /// build failure). This does not prove compatibility.
    Inconclusive {
        /// Why no verdict was possible.
        message: String,
    },
}

/// API validation bound to a Git baseline and an explicit compiler scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiEvidence {
    /// Observed check outcome; unavailable is never a pass.
    pub outcome: ApiOutcome,
    /// Exact baseline commit, absent when no comparison applies.
    pub baseline: Option<String>,
    /// Explicit target used for both sides of the comparison.
    pub target: String,
    /// Whether unavailable evidence blocks release execution.
    pub required: bool,
    /// Checker result or reason the comparison did not run.
    pub detail: String,
}

/// Result of API validation with the package's default features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiOutcome {
    /// Comparison passed or the reviewed major intent covers detected breakage.
    Pass,
    /// Detected API breakage exceeds reviewed intent.
    Fail,
    /// No conclusive comparison was available.
    Unavailable,
    /// Policy, package kind, or absence of a baseline excludes comparison.
    NotApplicable,
}

impl ApiEvidence {
    pub(crate) fn require_ready(&self, crate_name: &str) -> RailResult<()> {
        if self.outcome == ApiOutcome::Fail || self.required && self.outcome == ApiOutcome::Unavailable {
            return Err(RailError::with_help(
                format!("API validation for '{crate_name}' blocks release: {}", self.detail),
                "restore the required checker and baseline, or revise the reviewed change entry before preparing the release",
            ));
        }
        Ok(())
    }
}

pub(crate) fn assess(
    ctx: &WorkspaceContext,
    crate_name: &str,
    publish: bool,
    baseline: Option<&str>,
    policy: SemverCheckPolicy,
    reviewed_level: Option<BumpLevel>,
) -> RailResult<ApiEvidence> {
    let target = ctx.snapshot()?.toolchain().host_target().to_owned();
    let mut evidence = ApiEvidence {
        outcome: ApiOutcome::NotApplicable,
        baseline: None,
        target,
        required: policy == SemverCheckPolicy::Deny,
        detail: String::new(),
    };
    evidence.detail = if policy == SemverCheckPolicy::Off {
        "API validation is disabled by release policy".into()
    } else if !publish || !has_library_target(ctx, crate_name) {
        "package has no published library API".into()
    } else if let Some(baseline) = baseline {
        let baseline =
            ctx.git()?
                .git()
                .run_git_stdout(&["rev-parse", "--verify", &format!("{baseline}^{{commit}}")])?;
        evidence.baseline = Some(baseline.clone());
        match check_release(ctx, crate_name, &baseline, &evidence.target) {
            Ok(SemverCheck::Pass) => {
                evidence.outcome = ApiOutcome::Pass;
                "no incompatible API change detected with default features".into()
            }
            Ok(SemverCheck::Breaking { message }) if reviewed_level >= Some(BumpLevel::Major) => {
                evidence.outcome = ApiOutcome::Pass;
                format!("reviewed major intent covers detected breakage: {message}")
            }
            Ok(SemverCheck::Breaking { message }) => {
                evidence.outcome = ApiOutcome::Fail;
                format!(
                    "requires a major release: {message}; revise the reviewed change entry for '{crate_name}' to major"
                )
            }
            Ok(SemverCheck::Inconclusive { message }) => {
                evidence.outcome = ApiOutcome::Unavailable;
                message
            }
            Err(error) => {
                evidence.outcome = ApiOutcome::Unavailable;
                format!("cargo-semver-checks could not run: {error}")
            }
        }
    } else {
        "first release has no prior release tag to compare".into()
    };
    evidence.require_ready(crate_name)?;
    Ok(evidence)
}

/// Whether a workspace package has a library-like public API target.
pub(crate) fn has_library_target(ctx: &WorkspaceContext, crate_name: &str) -> bool {
    let Some(package) = ctx.cargo().get_package(crate_name) else {
        return false;
    };

    package.targets.iter().any(|target| {
        target.kind.iter().any(|kind| {
            matches!(
                kind,
                cargo_metadata::TargetKind::Lib
                    | cargo_metadata::TargetKind::RLib
                    | cargo_metadata::TargetKind::DyLib
                    | cargo_metadata::TargetKind::CDyLib
                    | cargo_metadata::TargetKind::StaticLib
                    | cargo_metadata::TargetKind::ProcMacro
            )
        })
    })
}

/// Run `cargo semver-checks check-release` for one crate.
fn check_release(ctx: &WorkspaceContext, crate_name: &str, baseline: &str, target: &str) -> RailResult<SemverCheck> {
    let package = ctx
        .cargo()
        .get_package(crate_name)
        .ok_or_else(|| RailError::message(format!("crate '{}' not found", crate_name)))?;

    let manifest_path = package.manifest_path.as_std_path();
    let Some(manifest) = manifest_path.to_str() else {
        return Err(RailError::message("manifest path is not valid UTF-8"));
    };

    let output = process::run(
        "cargo",
        &[
            "semver-checks",
            "check-release",
            "--manifest-path",
            manifest,
            "--baseline-rev",
            baseline,
            "--default-features",
            "--target",
            target,
        ],
        Some(ctx.workspace_root()),
    )?;

    Ok(classify_output(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    ))
}

/// Classify checker output into a verdict.
fn classify_output(success: bool, stdout: &str, stderr: &str) -> SemverCheck {
    if success {
        return SemverCheck::Pass;
    }

    if let Some(line) = marker_line(stdout).or_else(|| marker_line(stderr)) {
        return SemverCheck::Breaking { message: line };
    }

    SemverCheck::Inconclusive {
        message: first_message(stderr, stdout),
    }
}

fn marker_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains(BREAKING_MARKER))
        .map(|line| line.trim().to_string())
}

fn first_message(stderr: &str, stdout: &str) -> String {
    for line in stderr.lines().chain(stdout.lines()) {
        let line = line.trim();
        if !line.is_empty() {
            return line.to_string();
        }
    }
    "cargo-semver-checks exited without output".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_pass() {
        assert_eq!(classify_output(true, "", ""), SemverCheck::Pass);
    }

    #[test]
    fn breaking_marker_is_breaking() {
        let stdout = "Checking lib-a v1.2.3\nSummary semver requires new major version: 1 major check failed\n";
        let verdict = classify_output(false, stdout, "");
        assert_eq!(
            verdict,
            SemverCheck::Breaking {
                message: "Summary semver requires new major version: 1 major check failed".to_string(),
            }
        );
    }

    #[test]
    fn marker_on_stderr_is_breaking() {
        let stderr = "Summary semver requires new major version: 2 major checks failed";
        assert!(matches!(
            classify_output(false, "", stderr),
            SemverCheck::Breaking { .. }
        ));
    }

    #[test]
    fn nonzero_without_marker_is_inconclusive() {
        // First release: no published baseline on crates.io.
        let stderr = "error: the crate lib-a has no published versions to use as a baseline\n";
        let verdict = classify_output(false, "", stderr);
        assert_eq!(
            verdict,
            SemverCheck::Inconclusive {
                message: "error: the crate lib-a has no published versions to use as a baseline".to_string(),
            }
        );
    }

    #[test]
    fn silent_failure_is_inconclusive_with_note() {
        assert_eq!(
            classify_output(false, "", ""),
            SemverCheck::Inconclusive {
                message: "cargo-semver-checks exited without output".to_string(),
            }
        );
    }
}
