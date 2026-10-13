//! `cargo rail release`: preview, confirm, and run or finish one release.

use crate::commands::cli::ReleaseArgs;
use crate::error::{RailError, RailResult};
use crate::release::engine::{self, Effect, Report, State};
use crate::release::version::BumpRequest;
use crate::workspace::WorkspaceContext;

/// Run `cargo rail release`.
pub fn run_release(ctx: &WorkspaceContext, args: ReleaseArgs) -> RailResult<()> {
    let options = engine::Options {
        crates: (!args.crate_names.is_empty()).then_some(args.crate_names),
        bump: args.bump.parse::<BumpRequest>()?,
        pull_request: args.pr,
        yes: args.yes,
        dry_run: args.dry_run,
        json: args.format.is_json(),
    };
    let warnings = match ctx.config() {
        Some(config) => config.release_warnings(ctx.graph().workspace_members())?,
        None => Vec::new(),
    };
    if !options.json {
        for warning in &warnings {
            crate::warn!("{warning}");
        }
    }
    let report = engine::run(ctx, &options).map_err(|error| error.with_warnings(warnings.clone()))?;
    // Waiting for checks changed nothing and is not a release, so it exits like pending work.
    let exit_code = if report.state == State::AwaitingChecks { 1 } else { 0 };
    if options.json {
        let mut payload = serde_json::to_value(&report)?;
        if !warnings.is_empty() {
            payload["warnings"] = serde_json::json!(warnings);
        }
        let mode = if options.dry_run { "dry_run" } else { "apply" };
        let state = serde_json::to_value(report.state)?;
        let output = crate::output::machine_json_envelope(
            "release",
            mode,
            state.as_str().unwrap_or("unknown"),
            exit_code,
            payload,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", render(&report));
    }
    if exit_code == 1 {
        return Err(RailError::CheckHasPendingChanges);
    }
    Ok(())
}

fn render(report: &Report) -> String {
    let mut out = String::new();
    let headline = match report.state {
        State::NothingToRelease => "nothing to release".to_owned(),
        State::Planned => "dry run: every check passed; nothing changed".to_owned(),
        State::Cancelled => "cancelled; nothing changed".to_owned(),
        State::PullRequest => format!(
            "release pull request: {}",
            report.pull_request.as_deref().unwrap_or("open")
        ),
        State::AwaitingChecks => format!("waiting for checks: {}", report.pending_checks.join(", ")),
        State::AwaitingAssets => "draft release waits for assets".to_owned(),
        State::Released => "released".to_owned(),
    };
    out.push_str(&headline);
    for release in &report.crates {
        out.push_str(&format!("\n  {} {} ({})", release.name, release.version, release.tag));
        let registry = report.registry.as_deref().unwrap_or("registry");
        match release.registry {
            Effect::Done => out.push_str(&format!("\n    {registry}: published")),
            Effect::Pending if report.state != State::Planned => {
                out.push_str(&format!("\n    {registry}: not yet published"));
            }
            Effect::Pending => out.push_str(&format!("\n    {registry}: will publish")),
            Effect::Skipped => {}
        }
        match (release.github_release, &release.release_url) {
            (Effect::Done, Some(url)) => out.push_str(&format!("\n    GitHub release: {url}")),
            (Effect::Pending, Some(url)) => out.push_str(&format!("\n    draft GitHub release: {url}")),
            _ => {}
        }
        if !release.missing_assets.is_empty() && report.state == State::AwaitingAssets {
            out.push_str(&format!("\n    missing assets: {}", release.missing_assets.join(", ")));
        }
    }
    if report.state == State::NothingToRelease && !report.skipped.is_empty() && crate::output::is_verbose() {
        for skipped in &report.skipped {
            out.push_str(&format!("\n  {}: {}", skipped.name, skipped.reason));
        }
    }
    if let Some(next) = &report.next {
        out.push_str(&format!("\nnext: {next}"));
    }
    out
}
