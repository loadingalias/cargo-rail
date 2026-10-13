//! `cargo rail release`: plan from the workspace, finish from what Git, the registry, and GitHub record.
//!
//! A release keeps no state of its own. The release commit carries a `Rail-Release-Crates`
//! trailer naming each crate and version, the remote tag for each names the release commit,
//! and the registry and GitHub show what was published. Each run reads those, finishes the
//! most recent release if any effect is missing, and otherwise plans a new one.
//!
//! Effects run in this order, and none repeats once observed:
//! release commit and tags, one atomic push (or a release pull request),
//! draft GitHub releases until every configured asset is attached,
//! registry publication in dependency order, GitHub release publication, and aliases.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::Version;
use serde::Serialize;

use crate::config::{ReleaseChecks, ReleaseConfig};
use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::release::github::{Checks, GitHub, NewRelease, Release as GithubRelease};
use crate::release::planner::{ReleasePlan, ReleasePlanner};
use crate::release::registry::PublicationRegistry;
use crate::release::version::BumpRequest;
use crate::workspace::WorkspaceContext;

/// Git trailer that names a release commit's crates and versions.
pub(crate) const RELEASE_TRAILER: &str = "Rail-Release-Crates";
/// Trailer of release commits made by Cargo-Rail 0.31 and earlier, whose subject names the versions.
const LEGACY_RELEASE_TRAILER: &str = "Rail-Release-Intent";
/// Where Cargo-Rail 0.31 and earlier kept release records on origin.
const LEGACY_RECORDS: &str = "refs/notes/cargo-rail/*";
const REMOTE: &str = "origin";
/// Hooks may tell a release apart from ordinary Git work.
const OPERATION_ENV: &[(&str, &str)] = &[("CARGO_RAIL_OPERATION", "release"), ("CARGO_RAIL_RELEASE_PUSH", "1")];

/// What the user asked for.
#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) crates: Option<Vec<String>>,
    pub(crate) bump: BumpRequest,
    pub(crate) pull_request: bool,
    pub(crate) yes: bool,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
}

/// Where a release stands after this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    /// No crate has release-worthy changes, and the last release is complete.
    NothingToRelease,
    /// `--dry-run`: every preflight passed and these effects would run.
    Planned,
    /// The user declined the confirmation; nothing changed.
    Cancelled,
    /// A release pull request is open; merge it, then run again.
    PullRequest,
    /// The source commit's checks have not finished; nothing changed.
    AwaitingChecks,
    /// Draft releases wait for configured assets before anything irreversible happens.
    AwaitingAssets,
    /// Every effect is complete.
    Released,
}

/// The state of one effect for one crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    Done,
    Pending,
    Skipped,
}

/// One crate in the release.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CrateReport {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) previous_version: Option<String>,
    pub(crate) version: String,
    pub(crate) tag: String,
    /// Why a new release selects this version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// The changelog section a new release adds for this version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notes: Option<String>,
    pub(crate) registry: Effect,
    pub(crate) github_release: Effect,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) missing_assets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) release_url: Option<String>,
}

/// The result of one run.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    pub(crate) state: State,
    pub(crate) branch: String,
    /// The release commit, once it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commit: Option<String>,
    pub(crate) crates: Vec<CrateReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pull_request: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) pending_checks: Vec<String>,
    /// Crates the planner left out, with the reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) skipped: Vec<SkippedReport>,
    /// The one command that continues, when the release is not complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next: Option<String>,
    /// Facts the operator should know that did not stop the release.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<String>,
    /// The plan behind a new release, for `--dry-run` and review.
    #[serde(skip)]
    pub(crate) plan: Option<ReleasePlan>,
}

/// A crate left out of a new release.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedReport {
    pub(crate) name: String,
    pub(crate) reason: String,
}

/// One crate version that a release commit releases.
#[derive(Debug, Clone)]
struct Unit {
    name: String,
    version: Version,
    previous: Option<Version>,
    tag: String,
    /// Publishes to the configured registry.
    publish: bool,
    assets: Vec<String>,
    alias: Option<String>,
    /// Worktree-relative changelog path, when the crate keeps one.
    changelog: Option<PathBuf>,
}

/// A release whose effects are incomplete.
struct Unfinished {
    commit: String,
    units: Vec<Unit>,
    /// Whether the release commit is on origin; otherwise only this checkout has it.
    pushed: bool,
}

/// What the remote shows for one unit.
#[derive(Debug, Default)]
struct Observed {
    tag_commit: Option<String>,
    registry: Option<Effect>,
    release: Option<GithubRelease>,
    alias_commit: Option<String>,
}

/// Run `cargo rail release`.
pub(crate) fn run(ctx: &WorkspaceContext, options: &Options) -> RailResult<Report> {
    let session = Session::open(ctx, options)?;
    let tip = session.upstream_tip()?;
    if let Some(unfinished) = session.unfinished(&tip)? {
        return session.finish(&unfinished.commit, unfinished.units, unfinished.pushed, &tip);
    }
    if let Some(github) = &session.github {
        let open = github.open_release_pull_requests()?;
        if let Some(pull) = open.first() {
            crate::progress!("release pull request #{} is open: {}", pull.number, pull.html_url);
            return Ok(session.report(State::PullRequest, None, Vec::new(), |report| {
                report.pull_request = Some(pull.html_url.clone());
                report.next = Some("merge the release pull request, then run `cargo rail release`".to_owned());
            }));
        }
    }
    session.release(&tip)
}

struct Session<'a> {
    ctx: &'a WorkspaceContext,
    config: ReleaseConfig,
    options: &'a Options,
    git: SystemGit,
    branch: String,
    github: Option<GitHub>,
    github_releases: bool,
    checks: ReleaseChecks,
    warnings: std::cell::RefCell<Vec<String>>,
}

impl<'a> Session<'a> {
    fn warn(&self, warning: String) {
        crate::warn!("{warning}");
        self.warnings.borrow_mut().push(warning);
    }

    fn open(ctx: &'a WorkspaceContext, options: &'a Options) -> RailResult<Self> {
        let config = ctx.config().map(|config| config.release.clone()).unwrap_or_default();
        let git = SystemGit::open(&ctx.git()?.git().worktree_root)?;
        if git.is_detached_head()? {
            return Err(RailError::with_help(
                "a release needs a branch, but HEAD is detached",
                "check out the branch to release, such as `git switch main`",
            ));
        }
        let branch = git.current_branch()?;
        let origin = super::remote::release_repository(&git.worktree_root)?;
        let repository = super::github::github_repository(&origin);
        let github_releases = config
            .creates_github_release(repository.is_some())
            .map_err(RailError::Config)?;
        let checks = config
            .effective_checks(repository.is_some())
            .map_err(RailError::Config)?;
        let github = match &repository {
            Some((owner, repo)) if github_releases || checks == ReleaseChecks::All || options.pull_request => {
                Some(GitHub::connect(owner, repo, origin.host().unwrap_or("github.com"))?)
            }
            _ => None,
        };
        if options.pull_request && github.is_none() {
            return Err(RailError::with_help(
                "--pr opens a GitHub pull request, but origin is not a GitHub repository",
                "omit --pr to push the release commit directly",
            ));
        }
        Ok(Self {
            ctx,
            config,
            options,
            git,
            branch,
            github,
            github_releases,
            checks,
            warnings: std::cell::RefCell::new(Vec::new()),
        })
    }

    fn report(
        &self,
        state: State,
        commit: Option<String>,
        crates: Vec<CrateReport>,
        adjust: impl FnOnce(&mut Report),
    ) -> Report {
        let mut report = Report {
            state,
            branch: self.branch.clone(),
            commit,
            crates,
            registry: self.config.publish.registry().map(str::to_owned),
            pull_request: None,
            pending_checks: Vec::new(),
            skipped: Vec::new(),
            next: None,
            warnings: self.warnings.borrow().clone(),
            plan: None,
        };
        adjust(&mut report);
        report
    }

    /// The branch tip on origin, fetched so later reads can use it.
    ///
    /// The same read finds release records that Cargo-Rail 0.31 and earlier kept on origin.
    fn upstream_tip(&self) -> RailResult<String> {
        let branch = format!("refs/heads/{}", self.branch);
        let listing = self
            .git
            .run_git_stdout(&["ls-remote", "--", REMOTE, &branch, LEGACY_RECORDS])?;
        let mut tip = None;
        let mut legacy = Vec::new();
        for (object, reference) in listing.lines().filter_map(|line| line.split_once('\t')) {
            if reference == branch {
                if tip.replace(object.to_owned()).is_some() {
                    return Err(RailError::message(format!("origin reports '{branch}' more than once")));
                }
            } else if reference.starts_with("refs/notes/cargo-rail/") {
                legacy.push(reference.to_owned());
            }
        }
        if !legacy.is_empty() {
            self.warn(format!(
                "origin holds {} release record ref(s) from Cargo-Rail 0.31 or earlier; releases no longer read them. \
                 Remove them with: git push origin --delete {}",
                legacy.len(),
                legacy.join(" ")
            ));
        }
        let tip = tip.ok_or_else(|| {
            RailError::with_help(
                format!("branch '{}' does not exist on origin", self.branch),
                format!("push it first: git push -u origin {}", self.branch),
            )
        })?;
        if !self
            .git
            .run_git_check(&["cat-file", "-e", &format!("{tip}^{{commit}}")])
        {
            self.git.run_git(&[
                "fetch",
                "--no-tags",
                "--quiet",
                REMOTE,
                &format!("refs/heads/{}", self.branch),
            ])?;
        }
        Ok(tip)
    }

    fn planner(&self) -> ReleasePlanner<'_> {
        ReleasePlanner::new(self.ctx, &self.config)
    }

    // ----- Observation -----------------------------------------------------------------

    /// The most recent release whose effects are incomplete.
    fn unfinished(&self, tip: &str) -> RailResult<Option<Unfinished>> {
        // An earlier run may have committed and tagged locally, then failed to push.
        let head = self.git.head_commit()?;
        if head != tip
            && let Some(units) = self.release_units(&head)?
            && self.git.run_git_stdout(&["rev-parse", &format!("{head}^")])? == tip
        {
            return Ok(Some(Unfinished {
                commit: head,
                units,
                pushed: false,
            }));
        }
        let release = match (self.latest_release(tip)?, self.merged_pull_release(tip)?) {
            (Some(trailer), Some(merged)) if self.is_ancestor(&merged.0, &trailer.0) => Some(trailer),
            (_, Some(merged)) => Some(merged),
            (trailer, None) => trailer,
        };
        let Some((commit, units)) = release else {
            return Ok(None);
        };
        let observed = self.observe(&commit, &units, false)?;
        let complete = units.iter().zip(&observed).all(|(unit, observed)| {
            observed.tag_commit.is_some()
                && observed.registry.is_none_or(|effect| effect == Effect::Done)
                && (!self.github_releases || observed.release.as_ref().is_some_and(|release| !release.draft))
                && unit.alias.is_none() == observed.alias_commit.is_none()
        });
        Ok((!complete).then_some(Unfinished {
            commit,
            units,
            pushed: true,
        }))
    }

    /// The newest commit reachable from `tip` that names a release, and its units.
    fn latest_release(&self, tip: &str) -> RailResult<Option<(String, Vec<Unit>)>> {
        let commit = self.git.run_git_stdout(&[
            "log",
            "-n",
            "1",
            "--format=%H",
            "--extended-regexp",
            &format!("--grep=^({RELEASE_TRAILER}|{LEGACY_RELEASE_TRAILER}):"),
            tip,
        ])?;
        if commit.is_empty() {
            return Ok(None);
        }
        Ok(self.release_units(&commit)?.map(|units| (commit, units)))
    }

    /// The units a release commit names, or `None` when `commit` is not a release commit.
    ///
    /// A release commit carries a `Rail-Release-Crates` trailer. One made by Cargo-Rail 0.31 or
    /// earlier carries `Rail-Release-Intent` and names its versions in the subject instead, so
    /// this release can finish it.
    fn release_units(&self, commit: &str) -> RailResult<Option<Vec<Unit>>> {
        let output = self.git.run_git_stdout(&[
            "show",
            "-s",
            &format!(
                "--format=%(trailers:key={RELEASE_TRAILER},valueonly,separator=%x20)%x00%(trailers:key={LEGACY_RELEASE_TRAILER},valueonly)%x00%s"
            ),
            commit,
        ])?;
        let mut fields = output.splitn(3, '\0');
        let (current, legacy, subject) = (
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
        );
        let malformed =
            |entry: &str| RailError::message(format!("release commit {commit} names a malformed version '{entry}'"));
        let entries = if !current.trim().is_empty() {
            current
                .split_whitespace()
                .map(|entry| {
                    entry
                        .rsplit_once('@')
                        .and_then(|(name, version)| Some((name.to_owned(), Version::parse(version).ok()?)))
                        .ok_or_else(|| malformed(entry))
                })
                .collect::<RailResult<Vec<_>>>()?
        } else if let Some(names) = subject
            .strip_prefix("chore(release): ")
            .filter(|_| !legacy.trim().is_empty())
        {
            names
                .split(", ")
                .map(|entry| {
                    entry
                        .rsplit_once(" v")
                        .and_then(|(name, version)| Some((name.to_owned(), Version::parse(version).ok()?)))
                        .ok_or_else(|| malformed(entry))
                })
                .collect::<RailResult<Vec<_>>>()?
        } else {
            return Ok(None);
        };
        let mut units = Vec::with_capacity(entries.len());
        for (name, version) in entries {
            if self.ctx.cargo().get_package(&name).is_none() {
                return Err(RailError::with_help(
                    format!("release commit {commit} names '{name}', which this workspace no longer has"),
                    "finish that release with a checkout that still contains the crate",
                ));
            }
            units.push(self.unit(&name, version, None)?);
        }
        Ok(Some(units))
    }

    /// The release a squash-merged release pull request landed, when its message lost the trailer.
    ///
    /// GitHub keeps each pull request's head, the release commit with its trailer, so the
    /// units come from there and the merged commit on the branch is what the release tags.
    fn merged_pull_release(&self, tip: &str) -> RailResult<Option<(String, Vec<Unit>)>> {
        let Some(github) = &self.github else {
            return Ok(None);
        };
        let Some(pull) = github.merged_release_pull_request()? else {
            return Ok(None);
        };
        if !self
            .git
            .run_git_check(&["cat-file", "-e", &format!("{}^{{commit}}", pull.merge_commit)])
            || !self.is_ancestor(&pull.merge_commit, tip)
        {
            return Ok(None);
        }
        if !self
            .git
            .run_git_check(&["cat-file", "-e", &format!("{}^{{commit}}", pull.head)])
        {
            self.git.run_git(&[
                "fetch",
                "--no-tags",
                "--quiet",
                REMOTE,
                &format!("refs/pull/{}/head", pull.number),
            ])?;
        }
        // A merge commit or a rebase keeps the release commit on the branch; the trailer finds it.
        if self.is_ancestor(&pull.head, tip) {
            return Ok(None);
        }
        let Some(units) = self.release_units(&pull.head)? else {
            return Ok(None);
        };
        let tree = |commit: &str| self.git.run_git_stdout(&["rev-parse", &format!("{commit}^{{tree}}")]);
        if tree(&pull.merge_commit)? != tree(&pull.head)? {
            self.warn(format!(
                "release pull request #{} merged onto a branch that had moved, so {} also contains changes the \
                 release changelog does not describe; protect the branch with \"require branches to be up to date\"",
                pull.number,
                short(&pull.merge_commit)
            ));
        }
        Ok(Some((pull.merge_commit, units)))
    }

    fn unit(&self, name: &str, version: Version, previous: Option<Version>) -> RailResult<Unit> {
        let package = self
            .ctx
            .cargo()
            .get_package(name)
            .ok_or_else(|| RailError::message(format!("crate '{name}' not found")))?;
        let crate_config = self.ctx.config().and_then(|config| config.crates.get(name));
        let publish = self
            .config
            .publish
            .registry()
            .is_some_and(|registry| crate::workspace::CargoState::package_allows_registry(package, registry))
            && crate_config
                .and_then(|config| config.release.as_ref())
                .is_none_or(|release| release.publish);
        let changelog_config = crate_config.and_then(|config| config.changelog.as_ref());
        let changelog = if changelog_config.is_some_and(|config| config.skip) {
            None
        } else {
            let path = changelog_config
                .and_then(|config| config.path.clone())
                .unwrap_or_else(|| PathBuf::from(&self.config.changelog.path));
            let base = match changelog_config
                .and_then(|config| config.relative_to)
                .unwrap_or(self.config.changelog.relative_to)
            {
                crate::config::ChangelogRelativeTo::Crate => package
                    .manifest_path
                    .parent()
                    .map(|parent| parent.as_std_path().to_path_buf())
                    .ok_or_else(|| RailError::message("crate manifest has no directory"))?,
                crate::config::ChangelogRelativeTo::Workspace => self.ctx.workspace_root().to_path_buf(),
            };
            Some(self.worktree_relative(&base.join(path))?)
        };
        Ok(Unit {
            name: name.to_owned(),
            tag: self.planner().format_tag(name, &version),
            alias: self.config.alias_for(name, &version),
            assets: if self.github_releases {
                self.config.assets_for(name, &version)
            } else {
                Vec::new()
            },
            version,
            previous,
            publish,
            changelog,
        })
    }

    fn worktree_relative(&self, path: &Path) -> RailResult<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.ctx.workspace_root().join(path)
        };
        crate::utils::path_relative_to(&self.git.worktree_root, &absolute).map_err(|error| {
            RailError::message(format!(
                "release path '{}' is outside the repository: {error}",
                absolute.display()
            ))
        })
    }

    /// What origin, the registry, and GitHub show for each unit of the release at `commit`.
    ///
    /// `verify_sources` downloads each published package to prove it came from `commit`;
    /// otherwise a published GitHub release stands for the registry upload it follows.
    fn observe(&self, commit: &str, units: &[Unit], verify_sources: bool) -> RailResult<Vec<Observed>> {
        let refs = self.remote_tags(
            units
                .iter()
                .flat_map(|unit| [Some(&unit.tag), unit.alias.as_ref()])
                .flatten(),
        )?;
        let registry = self.registry_for(units)?;
        let mut observed = Vec::with_capacity(units.len());
        for unit in units {
            let tag_commit = refs.get(&unit.tag).map(|(_, commit)| commit.clone());
            if let Some(tagged) = &tag_commit
                && tagged != commit
            {
                return Err(RailError::with_help(
                    format!("tag {} on origin names {tagged}, not release commit {commit}", unit.tag),
                    "inspect the tag; a release never moves a published tag",
                ));
            }
            let release = match (&self.github, self.github_releases) {
                (Some(github), true) => github.release(&unit.tag)?,
                _ => None,
            };
            let published_release = release.as_ref().is_some_and(|release| !release.draft);
            let registry = match (&registry, unit.publish) {
                (Some(registry), true) => Some(match registry.version(&unit.name, &unit.version)? {
                    None => Effect::Pending,
                    Some(published) => {
                        if verify_sources || !published_release {
                            self.require_source(registry, unit, &published, commit)?;
                        }
                        Effect::Done
                    }
                }),
                _ => None,
            };
            let alias_commit = unit
                .alias
                .as_ref()
                .and_then(|alias| refs.get(alias))
                .map(|(_, commit)| commit.clone())
                .filter(|alias| alias == commit || self.is_ancestor(commit, alias));
            observed.push(Observed {
                tag_commit,
                registry,
                release,
                alias_commit,
            });
        }
        Ok(observed)
    }

    /// Prove a published package came from `commit`, or stop.
    fn require_source(
        &self,
        registry: &PublicationRegistry,
        unit: &Unit,
        published: &super::registry::Published,
        commit: &str,
    ) -> RailResult<()> {
        if published.yanked {
            return Err(RailError::with_help(
                format!("{} {} is yanked from {}", unit.name, unit.version, registry.name()),
                "a yanked version cannot complete this release; release a new version",
            ));
        }
        match registry.source_commit(&unit.name, &unit.version, published)? {
            Some(source) if source == commit => Ok(()),
            source => Err(RailError::with_help(
                format!(
                    "{} {} on {} was published from {}, not release commit {commit}",
                    unit.name,
                    unit.version,
                    registry.name(),
                    source.as_deref().unwrap_or("an unrecorded source")
                ),
                "this version belongs to another source; release a new version",
            )),
        }
    }

    fn registry_for(&self, units: &[Unit]) -> RailResult<Option<PublicationRegistry>> {
        let Some(name) = self.config.publish.registry() else {
            return Ok(None);
        };
        if !units.iter().any(|unit| unit.publish) {
            return Ok(None);
        }
        let snapshot = self.ctx.snapshot()?;
        PublicationRegistry::resolve(name, Some(snapshot.cargo_config().effective_file_settings())).map(Some)
    }

    /// Tag objects and peeled commits on origin for exactly these tag names.
    fn remote_tags<'t>(
        &self,
        names: impl Iterator<Item = &'t String>,
    ) -> RailResult<BTreeMap<String, (String, String)>> {
        let names = names.collect::<BTreeSet<_>>();
        if names.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut arguments = vec![
            "ls-remote".to_owned(),
            "--tags".to_owned(),
            "--".to_owned(),
            REMOTE.to_owned(),
        ];
        arguments.extend(names.iter().map(|name| format!("refs/tags/{name}*")));
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let output = self.git.run_git_stdout(&arguments)?;
        let mut objects = BTreeMap::new();
        let mut peeled = BTreeMap::new();
        for line in output.lines() {
            let Some((object, reference)) = line.split_once('\t') else {
                continue;
            };
            let Some(name) = reference.strip_prefix("refs/tags/") else {
                continue;
            };
            if let Some(name) = name.strip_suffix("^{}") {
                peeled.insert(name.to_owned(), object.to_owned());
            } else {
                objects.insert(name.to_owned(), object.to_owned());
            }
        }
        Ok(objects
            .into_iter()
            .filter(|(name, _)| names.contains(name))
            .map(|(name, object)| {
                let commit = peeled.get(&name).cloned().unwrap_or_else(|| object.clone());
                (name, (object, commit))
            })
            .collect())
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.git
            .run_git_check(&["merge-base", "--is-ancestor", ancestor, descendant])
    }

    // ----- New release -----------------------------------------------------------------

    fn release(&self, tip: &str) -> RailResult<Report> {
        let head = self.git.head_commit()?;
        if head != tip {
            let (problem, help) = if self.is_ancestor(&head, tip) {
                ("is behind", "git pull --ff-only")
            } else if self.is_ancestor(tip, &head) {
                ("is ahead of", "push its commits first, or release from origin's tip")
            } else {
                ("has diverged from", "reconcile the branch with origin first")
            };
            return Err(RailError::with_help(
                format!(
                    "branch '{}' {problem} origin/{}; a release starts from origin's tip",
                    self.branch, self.branch
                ),
                help,
            ));
        }
        let dirty = self.git.changed_paths()?;
        if !dirty.is_empty() {
            return Err(RailError::with_help(
                format!(
                    "a release starts from a clean worktree; changed: {}",
                    dirty
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "commit, stash, or remove the changes, then run again",
            ));
        }

        let planner = self.planner();
        let mut plan = planner.plan(self.options.crates.clone(), &self.options.bump)?;
        let skipped = plan
            .skipped
            .iter()
            .map(|skip| SkippedReport {
                name: skip.name.clone(),
                reason: skip.reason.clone(),
            })
            .collect::<Vec<_>>();
        if plan.crates.is_empty() {
            return Ok(self.report(State::NothingToRelease, None, Vec::new(), |report| {
                report.skipped = skipped;
            }));
        }
        let units = plan
            .crates
            .iter()
            .map(|planned| {
                self.unit(
                    &planned.name,
                    planned.new_version.clone(),
                    Some(planned.current_version.clone()),
                )
            })
            .collect::<RailResult<Vec<_>>>()?;
        let mut crates = units
            .iter()
            .map(|unit| self.crate_report(unit, &Observed::default()))
            .collect::<Vec<_>>();
        for ((report, unit), planned) in crates.iter_mut().zip(&units).zip(&plan.crates) {
            report.missing_assets.clone_from(&unit.assets);
            report.reason = Some(planned.bump_reason.clone());
            report.notes = Some(planned.changelog_body.clone());
        }

        // Checks are cheap to read and decide everything else, so they come first.
        let gate = self.gate(&head)?;
        if let Some(pending) = gate {
            return Ok(self.report(State::AwaitingChecks, None, crates, |report| {
                report.pending_checks = pending;
                report.skipped = skipped;
                report.next = Some("run `cargo rail release` again when the checks finish".to_owned());
                report.plan = Some(plan.clone());
            }));
        }

        render_new_release(self, &plan, &units);
        if !self.confirm()? {
            return Ok(self.report(State::Cancelled, None, crates, |report| report.skipped = skipped));
        }

        // Preflight: every check that can fail before an effect.
        crate::progress!("checking the release before any effect");
        self.preflight_versions(&units)?;
        let registry = self.registry_for(&units)?;
        if let Some(registry) = &registry {
            for unit in units.iter().filter(|unit| unit.publish) {
                if registry.version(&unit.name, &unit.version)?.is_some() {
                    return Err(RailError::with_help(
                        format!("{} {} is already on {}", unit.name, unit.version, registry.name()),
                        "choose a new version",
                    ));
                }
            }
            require_registry_token(self.ctx, registry.name())?;
        }
        for planned in &plan.crates {
            let evidence = super::semver_checks::assess(
                self.ctx,
                &planned.name,
                planned.publish,
                planned.previous_tag.as_deref(),
                self.config.semver_check,
                planned.reviewed_level,
            )?;
            evidence.require_ready(&planned.name)?;
            if evidence.outcome == super::semver_checks::ApiOutcome::Unavailable {
                self.warn(format!(
                    "API check for {} is unavailable: {}",
                    planned.name, evidence.detail
                ));
            }
            if self.github_releases && planned.changelog_body.trim().is_empty() {
                return Err(RailError::with_help(
                    format!("{} {} has no release notes", planned.name, planned.new_version),
                    "add a change file or changelog entry that describes the release",
                ));
            }
        }
        planner.project_writes(&mut plan)?;
        let commit = self.release_commit(&plan, &units)?;
        if let Some(registry) = &registry {
            let clone = SourceClone::new(self.ctx, &self.git, &commit)?;
            clone.package(self.ctx, registry.name(), &units)?;
        }
        if self.options.dry_run {
            // The commit object exists only to package; its identity includes the time, so it is not reported.
            return Ok(self.report(State::Planned, None, crates, |report| {
                report.skipped = skipped;
                report.plan = Some(plan);
            }));
        }

        if self.options.pull_request {
            return self.open_pull_request(&commit, &units, crates, &plan);
        }
        self.land(&head, &commit, &units)?;
        let mut report = self.finish(&commit, units, true, &commit)?;
        for (finished, planned) in report.crates.iter_mut().zip(&crates) {
            finished.reason.clone_from(&planned.reason);
            finished.notes.clone_from(&planned.notes);
        }
        report.skipped = skipped;
        report.plan = Some(plan);
        Ok(report)
    }

    /// The checks gate on `commit`: `None` when it passes, the pending checks when it must wait.
    fn gate(&self, commit: &str) -> RailResult<Option<Vec<String>>> {
        let (ReleaseChecks::All, Some(github)) = (self.checks, &self.github) else {
            return Ok(None);
        };
        match github.checks(commit)? {
            Checks::Passed(count) => {
                crate::progress!("checks on {}: {count} passed", short(commit));
                Ok(None)
            }
            Checks::Pending(names) => {
                crate::progress!(
                    "waiting for {} check(s) on {}: {}",
                    names.len(),
                    short(commit),
                    names.join(", ")
                );
                Ok(Some(names))
            }
            Checks::Failed(names) => Err(RailError::with_help(
                format!("checks failed on {}: {}", short(commit), names.join(", ")),
                "fix or rerun the failing checks; nothing was released",
            )),
            Checks::Missing => Err(RailError::with_help(
                format!("GitHub reports no checks on {}", short(commit)),
                "wait for CI to start, or set release.checks = \"none\" for a repository without CI",
            )),
        }
    }

    fn confirm(&self) -> RailResult<bool> {
        if self.options.yes || self.options.dry_run {
            return Ok(true);
        }
        use std::io::IsTerminal as _;
        if self.options.json || !std::io::stdin().is_terminal() {
            return Err(RailError::with_help(
                "a release asks for confirmation, but no terminal is attached",
                "review with `cargo rail release --dry-run`, then pass --yes",
            ));
        }
        crate::utils::prompt_for_confirmation()
    }

    /// No unit's tag exists on origin or locally, and no literal major alias points at another major.
    fn preflight_versions(&self, units: &[Unit]) -> RailResult<()> {
        for unit in units {
            let Some(alias) = &unit.alias else { continue };
            let major = alias
                .strip_prefix('v')
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|digits| digits.parse::<u64>().ok());
            if major.is_some_and(|major| major != unit.version.major) {
                return Err(RailError::with_help(
                    format!(
                        "alias {alias} would name {} {}, a different major version",
                        unit.name, unit.version
                    ),
                    format!(
                        "set release.aliases.{} = \"v{{major}}\" so the alias follows each major line",
                        unit.name
                    ),
                ));
            }
        }
        let remote = self.remote_tags(units.iter().map(|unit| &unit.tag))?;
        for unit in units {
            if remote.contains_key(&unit.tag) || self.git.tag_exists(&unit.tag)? {
                return Err(RailError::with_help(
                    format!("tag {} already exists", unit.tag),
                    "choose a new version, or delete the stray local tag after confirming it is not a release",
                ));
            }
        }
        Ok(())
    }

    /// Build the release commit object on top of HEAD without touching the worktree.
    fn release_commit(&self, plan: &ReleasePlan, units: &[Unit]) -> RailResult<String> {
        let mut writes: BTreeMap<PathBuf, Option<Vec<u8>>> = BTreeMap::new();
        for planned in &plan.crates {
            if let Some(changelog) = planned
                .presentation
                .as_ref()
                .and_then(|presentation| presentation.changelog.as_ref())
            {
                writes.insert(
                    self.worktree_relative(&changelog.path)?,
                    Some(changelog.content.clone().into_bytes()),
                );
            }
        }
        for write in &plan.cargo_writes {
            writes.insert(
                self.worktree_relative(&write.path)?,
                Some(write.content.clone().into_bytes()),
            );
        }
        for path in &plan.change_files_to_delete {
            writes.insert(self.worktree_relative(path)?, None);
        }
        for update in &plan.change_files_to_update {
            writes.insert(
                self.worktree_relative(&update.path)?,
                Some(update.content.clone().into_bytes()),
            );
        }
        let index = tempfile::Builder::new().prefix("cargo-rail-release-index-").tempdir()?;
        let index = index.path().join("index");
        let index_env = index
            .to_str()
            .ok_or_else(|| RailError::message("temporary index path is not UTF-8"))?;
        let git = |arguments: &[&str], input: Option<&[u8]>| -> RailResult<String> {
            git_with(
                &self.git.worktree_root,
                arguments,
                &[("GIT_INDEX_FILE", index_env)],
                input,
            )
        };
        git(&["read-tree", "HEAD"], None)?;
        for (path, content) in &writes {
            let path = crate::utils::path_to_git_format(path);
            match content {
                Some(bytes) => {
                    let blob = git(
                        &["hash-object", "-w", "--stdin", &format!("--path={path}")],
                        Some(bytes),
                    )?;
                    let mode = git(&["ls-files", "-s", "--", &path], None)?
                        .split_whitespace()
                        .next()
                        .filter(|mode| *mode == "100755")
                        .unwrap_or("100644")
                        .to_owned();
                    git(
                        &["update-index", "--add", "--cacheinfo", &format!("{mode},{blob},{path}")],
                        None,
                    )?;
                }
                None => {
                    git(&["update-index", "--force-remove", "--", &path], None)?;
                }
            }
        }
        let tree = git(&["write-tree"], None)?;
        let summary = units
            .iter()
            .map(|unit| format!("{} v{}", unit.name, unit.version))
            .collect::<Vec<_>>()
            .join(", ");
        let trailer = units
            .iter()
            .map(|unit| format!("{}@{}", unit.name, unit.version))
            .collect::<Vec<_>>()
            .join(" ");
        let message = format!("chore(release): {summary}\n\n{RELEASE_TRAILER}: {trailer}\n");
        git_with(
            &self.git.worktree_root,
            &["commit-tree", &tree, "-p", "HEAD", "-F", "-"],
            OPERATION_ENV,
            Some(message.as_bytes()),
        )
    }

    /// Move the branch to the release commit and push it with its tags in one atomic update.
    fn land(&self, parent: &str, commit: &str, units: &[Unit]) -> RailResult<()> {
        self.git.run_git(&["merge", "--ff-only", "--quiet", commit])?;
        let tagged = (|| -> RailResult<()> {
            for unit in units {
                self.create_tag(unit, commit)?;
            }
            self.push(Some(parent), commit, units)
        })();
        if let Err(error) = tagged {
            // The push is atomic: when origin holds neither the release commit nor any of its
            // tags, nothing was released, so the checkout goes back where it was.
            let landed = self.git.remote_branch_head(REMOTE, &self.branch)?.as_deref() == Some(commit)
                || !self.remote_tags(units.iter().map(|unit| &unit.tag))?.is_empty();
            if !landed {
                for unit in units {
                    drop(self.git.run_git(&["tag", "-d", &unit.tag]));
                }
                self.git.run_git(&["reset", "--keep", "--quiet", parent])?;
                return Err(error.context("nothing was released; the checkout is back at its starting commit"));
            }
            return Err(error);
        }
        crate::progress!("pushed {} and {} tag(s) to origin", self.branch, units.len());
        Ok(())
    }

    fn create_tag(&self, unit: &Unit, commit: &str) -> RailResult<()> {
        let reference = format!("refs/tags/{}", unit.tag);
        if self.git.run_git_check(&["show-ref", "--verify", "--quiet", &reference]) {
            let target = self
                .git
                .run_git_stdout(&["rev-parse", &format!("{reference}^{{commit}}")])?;
            if target != commit {
                return Err(RailError::with_help(
                    format!("local tag {} names {target}, not release commit {commit}", unit.tag),
                    "delete the stray local tag after confirming it is not a release",
                ));
            }
            return Ok(());
        }
        let message = format!("Release {} v{}", unit.name, unit.version);
        let mut arguments = if self.config.sign_tags {
            vec!["tag", "-s"]
        } else {
            vec!["-c", "tag.gpgsign=false", "tag", "-a"]
        };
        arguments.extend(["-m", &message, &unit.tag, commit]);
        self.git.run_git(&arguments)?;
        Ok(())
    }

    /// Push the branch (leased on `parent`) and each unit's tag (leased on its absence) atomically.
    fn push(&self, parent: Option<&str>, commit: &str, units: &[Unit]) -> RailResult<()> {
        let mut arguments = vec!["push".to_owned(), "--atomic".to_owned(), "--quiet".to_owned()];
        if let Some(parent) = parent {
            arguments.push(format!("--force-with-lease=refs/heads/{}:{parent}", self.branch));
        }
        for unit in units {
            arguments.push(format!("--force-with-lease=refs/tags/{}:", unit.tag));
        }
        arguments.extend(["--".to_owned(), REMOTE.to_owned()]);
        if parent.is_some() {
            arguments.push(format!("{commit}:refs/heads/{}", self.branch));
        }
        arguments.extend(
            units
                .iter()
                .map(|unit| format!("refs/tags/{0}:refs/tags/{0}", unit.tag)),
        );
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        if let Err(error) = self.git.run_git_observable_with_env(&arguments, OPERATION_ENV) {
            // A lost acknowledgment is success when origin shows exactly this release.
            let branch_landed =
                parent.is_none() || self.git.remote_branch_head(REMOTE, &self.branch)?.as_deref() == Some(commit);
            let tags = self.remote_tags(units.iter().map(|unit| &unit.tag))?;
            if branch_landed
                && units
                    .iter()
                    .all(|unit| tags.get(&unit.tag).is_some_and(|(_, tagged)| tagged == commit))
            {
                return Ok(());
            }
            return Err(error.context(format!(
                "origin rejected the release push; if '{}' moved, pull and run `cargo rail release` again",
                self.branch
            )));
        }
        Ok(())
    }

    fn open_pull_request(
        &self,
        commit: &str,
        units: &[Unit],
        crates: Vec<CrateReport>,
        plan: &ReleasePlan,
    ) -> RailResult<Report> {
        let github = self
            .github
            .as_ref()
            .ok_or_else(|| RailError::message("--pr requires GitHub"))?;
        let first = &units[0];
        let head = format!(
            "{}{}-v{}",
            super::github::RELEASE_BRANCH_PREFIX,
            first.name,
            first.version
        );
        // No release pull request is open, so a leftover branch under the release prefix is stale.
        let stale = self.git.remote_branch_head(REMOTE, &head)?.unwrap_or_default();
        let pushed = self.git.run_git_observable_with_env(
            &[
                "push",
                "--quiet",
                &format!("--force-with-lease=refs/heads/{head}:{stale}"),
                "--",
                REMOTE,
                &format!("{commit}:refs/heads/{head}"),
            ],
            OPERATION_ENV,
        );
        if let Err(error) = pushed
            && self.git.remote_branch_head(REMOTE, &head)?.as_deref() != Some(commit)
        {
            return Err(error.context(format!("could not push release branch {head}")));
        }
        let title = self.git.run_git_stdout(&["show", "-s", "--format=%s", commit])?;
        let mut body = String::new();
        for planned in &plan.crates {
            body.push_str(&format!(
                "## {} {} -> {}\n\n{}\n\n",
                planned.name,
                planned.current_version,
                planned.new_version,
                planned.changelog_body.trim()
            ));
        }
        let trailer = self.git.run_git_stdout(&[
            "show",
            "-s",
            &format!("--format=%(trailers:key={RELEASE_TRAILER})"),
            commit,
        ])?;
        body.push_str(&format!(
            "Merge this pull request, then run `cargo rail release` to tag and publish it.\nKeep this trailer in the merge commit message:\n\n{trailer}\n"
        ));
        let pull = github.open_pull_request(&head, &self.branch, &title, &body)?;
        crate::progress!("opened release pull request #{}: {}", pull.number, pull.html_url);
        Ok(
            self.report(State::PullRequest, Some(commit.to_owned()), crates, |report| {
                report.pull_request = Some(pull.html_url);
                report.next = Some("merge the release pull request, then run `cargo rail release`".to_owned());
            }),
        )
    }

    // ----- Finishing -------------------------------------------------------------------

    /// Complete every missing effect of the release at `commit`, in order.
    fn finish(&self, commit: &str, units: Vec<Unit>, pushed: bool, tip: &str) -> RailResult<Report> {
        let fresh = commit == tip && pushed && self.git.head_commit()? == commit;
        let mut observed = self.observe(commit, &units, true)?;
        let untagged = units
            .iter()
            .zip(&observed)
            .filter(|(_, observed)| observed.tag_commit.is_none())
            .map(|(unit, _)| unit.clone())
            .collect::<Vec<_>>();
        if !fresh {
            render_finish(self, commit, &units, &observed);
            if !self.confirm()? {
                let crates = self.crate_reports(&units, &observed);
                return Ok(self.report(State::Cancelled, Some(commit.to_owned()), crates, |_| {}));
            }
            if self.options.dry_run {
                let crates = self.crate_reports(&units, &observed);
                return Ok(self.report(State::Planned, Some(commit.to_owned()), crates, |_| {}));
            }
        }

        // 1. Tags: a merged release pull request, or a local commit that never reached origin.
        if !untagged.is_empty() {
            if pushed {
                if let Some(pending) = self.gate(commit)? {
                    let crates = self.crate_reports(&units, &observed);
                    return Ok(
                        self.report(State::AwaitingChecks, Some(commit.to_owned()), crates, |report| {
                            report.pending_checks = pending;
                            report.next = Some("run `cargo rail release` again when the checks finish".to_owned());
                        }),
                    );
                }
                for unit in &untagged {
                    self.create_tag(unit, commit)?;
                }
                self.push(None, commit, &untagged)?;
            } else {
                for unit in &untagged {
                    self.create_tag(unit, commit)?;
                }
                let parent = self.git.run_git_stdout(&["rev-parse", &format!("{commit}^")])?;
                self.push(Some(&parent), commit, &untagged)?;
            }
            crate::progress!(
                "tagged {}",
                untagged
                    .iter()
                    .map(|unit| unit.tag.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for observed in &mut observed {
                observed.tag_commit.get_or_insert_with(|| commit.to_owned());
            }
        }

        // 2. Draft releases collect assets before anything irreversible happens.
        let mut waiting = false;
        if let Some(github) = &self.github
            && self.github_releases
        {
            for (unit, observed) in units.iter().zip(&mut observed) {
                if unit.assets.is_empty() || observed.release.as_ref().is_some_and(|release| !release.draft) {
                    continue;
                }
                if observed.release.is_none() {
                    observed.release = Some(github.create_release(&NewRelease {
                        tag: &unit.tag,
                        commit,
                        name: &release_name(unit),
                        body: &self.notes(unit, commit)?,
                        draft: true,
                        prerelease: !unit.version.pre.is_empty(),
                    })?);
                    crate::progress!("created draft release {}", unit.tag);
                }
                let missing = observed
                    .release
                    .as_ref()
                    .map(|release| release.missing_assets(&unit.assets))
                    .unwrap_or_default();
                if !missing.is_empty() {
                    crate::progress!(
                        "draft release {} waits for {} asset(s): {}",
                        unit.tag,
                        missing.len(),
                        missing.join(", ")
                    );
                    waiting = true;
                }
            }
        }
        if waiting {
            let crates = self.crate_reports(&units, &observed);
            return Ok(
                self.report(State::AwaitingAssets, Some(commit.to_owned()), crates, |report| {
                    report.next =
                        Some("attach the listed assets to the draft release, then run `cargo rail release`".to_owned());
                }),
            );
        }
        if let Some(github) = &self.github {
            for (unit, observed) in units.iter().zip(&observed) {
                if let Some(draft) = observed.release.as_ref().filter(|release| release.draft) {
                    self.verify_assets(github, unit, draft, commit)?;
                }
            }
        }

        // 3. Registry publication, in dependency order.
        let pending = units
            .iter()
            .zip(&observed)
            .filter(|(_, observed)| observed.registry == Some(Effect::Pending))
            .map(|(unit, _)| unit.clone())
            .collect::<Vec<_>>();
        if !pending.is_empty() {
            let registry = self
                .registry_for(&pending)?
                .ok_or_else(|| RailError::message("registry publication has no registry"))?;
            require_registry_token(self.ctx, registry.name())?;
            let clone = SourceClone::new(self.ctx, &self.git, commit)?;
            for unit in &pending {
                clone.publish(self.ctx, registry.name(), unit)?;
                let published = wait_for_version(&registry, unit)?;
                self.require_source(&registry, unit, &published, commit)?;
                crate::progress!("published {} {} to {}", unit.name, unit.version, registry.name());
            }
            for observed in &mut observed {
                if observed.registry == Some(Effect::Pending) {
                    observed.registry = Some(Effect::Done);
                }
            }
        }

        // 4. GitHub releases.
        if let Some(github) = &self.github
            && self.github_releases
        {
            for (unit, observed) in units.iter().zip(&mut observed) {
                let release = match observed.release.take() {
                    Some(release) if !release.draft => release,
                    Some(draft) => github.publish_release(draft.id)?,
                    None => github.create_release(&NewRelease {
                        tag: &unit.tag,
                        commit,
                        name: &release_name(unit),
                        body: &self.notes(unit, commit)?,
                        draft: false,
                        prerelease: !unit.version.pre.is_empty(),
                    })?,
                };
                crate::progress!("published GitHub release {}: {}", unit.tag, release.html_url);
                observed.release = Some(release);
            }
        }

        // 5. Aliases follow the published release; they never move backward.
        let aliases = self.remote_tags(units.iter().filter_map(|unit| unit.alias.as_ref()))?;
        let tags = self.remote_tags(units.iter().map(|unit| &unit.tag))?;
        for (unit, observed) in units.iter().zip(&mut observed) {
            let Some(alias) = &unit.alias else {
                continue;
            };
            let current = aliases.get(alias);
            if current.is_some_and(|(_, target)| target == commit || self.is_ancestor(commit, target)) {
                observed.alias_commit = Some(commit.to_owned());
                continue;
            }
            if let Some((_, target)) = current
                && !self.is_ancestor(target, commit)
            {
                return Err(RailError::with_help(
                    format!("alias {alias} names {target}, which is not an earlier release of this line"),
                    "inspect the alias; a release moves an alias only forward",
                ));
            }
            let (tag_object, _) = tags
                .get(&unit.tag)
                .ok_or_else(|| RailError::message(format!("tag {} is missing on origin", unit.tag)))?;
            let expected = current.map_or("", |(object, _)| object.as_str());
            self.git.run_git_observable_with_env(
                &[
                    "push",
                    "--quiet",
                    &format!("--force-with-lease=refs/tags/{alias}:{expected}"),
                    "--",
                    REMOTE,
                    &format!("{tag_object}:refs/tags/{alias}"),
                ],
                OPERATION_ENV,
            )?;
            crate::progress!("moved alias {alias} to {}", unit.tag);
            observed.alias_commit = Some(commit.to_owned());
        }

        let crates = self.crate_reports(&units, &observed);
        crate::progress!("release complete");
        Ok(self.report(State::Released, Some(commit.to_owned()), crates, |_| {}))
    }

    /// Prove each attached asset is intact and, unless attestations are off, that this
    /// repository's workflows built it from the release commit.
    fn verify_assets(&self, github: &GitHub, unit: &Unit, draft: &GithubRelease, commit: &str) -> RailResult<()> {
        if unit.assets.is_empty() {
            return Ok(());
        }
        let directory = tempfile::Builder::new()
            .prefix("cargo-rail-release-assets-")
            .tempdir()?;
        let repository = github.full_name();
        // An attestation names the commit its workflow run started from: the release commit, or,
        // when the same run created the release commit, the commit it was prepared from.
        let mut sources = vec![commit.to_owned()];
        if let Ok(parent) = self.git.run_git_stdout(&["rev-parse", &format!("{commit}^")]) {
            sources.push(parent);
        }
        for name in &unit.assets {
            let asset = draft
                .assets
                .iter()
                .find(|asset| &asset.name == name)
                .ok_or_else(|| RailError::message(format!("draft release {} lost asset {name}", unit.tag)))?;
            let path = directory.path().join(name);
            let digest = github.download_asset(asset, &path)?;
            if asset
                .digest
                .as_deref()
                .is_some_and(|recorded| recorded != format!("sha256:{digest}"))
            {
                return Err(RailError::with_help(
                    format!("release asset {name} does not match the digest GitHub recorded at upload"),
                    "delete the asset from the draft and upload it again",
                ));
            }
            if self.config.asset_attestations == crate::config::AssetAttestations::Off {
                continue;
            }
            let path_text = path
                .to_str()
                .ok_or_else(|| RailError::message("asset path is not UTF-8"))?;
            let output = Command::new("gh")
                .args([
                    "attestation",
                    "verify",
                    path_text,
                    "--repo",
                    &repository,
                    "--signer-repo",
                    &repository,
                    "--format",
                    "json",
                ])
                .envs(github.cli_environment())
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|error| {
                    RailError::with_help(
                        format!("verifying asset attestations needs the GitHub CLI: {error}"),
                        "install gh 2.49 or later, or set release.asset_attestations = \"off\"",
                    )
                })?;
            let attested = if output.status.success() {
                attested_sources(&output.stdout)
            } else {
                Vec::new()
            };
            if !attested.iter().any(|source| sources.contains(source)) {
                let detail = if output.status.success() {
                    format!("its attestations name {}", attested.join(", "))
                } else {
                    String::from_utf8_lossy(&output.stderr).trim().to_owned()
                };
                return Err(RailError::with_help(
                    format!(
                        "release asset {name} has no attestation from {repository} built at release commit {} or the commit it was prepared from: {detail}",
                        short(commit)
                    ),
                    "build and attest it in the release workflow with `loadingalias/cargo-rail-action/release/asset`, \
                     or set release.asset_attestations = \"off\" for a repository that cannot attest",
                ));
            }
        }
        crate::progress!("verified {} asset(s) of {}", unit.assets.len(), unit.tag);
        Ok(())
    }

    /// Release notes: the crate's changelog section for this version at the release commit.
    fn notes(&self, unit: &Unit, commit: &str) -> RailResult<String> {
        let section = unit.changelog.as_ref().and_then(|path| {
            self.git
                .run_git_stdout(&["show", &format!("{commit}:{}", crate::utils::path_to_git_format(path))])
                .ok()
                .and_then(|content| super::presentation::extract_section(&content, &unit.version.to_string()))
        });
        let notes = section.unwrap_or_else(|| format!("Release {} v{}.", unit.name, unit.version));
        if notes.len() > super::github::MAX_RELEASE_BODY {
            return Err(RailError::with_help(
                format!("release notes for {} exceed GitHub's limit", unit.tag),
                "shorten the changelog section for this version",
            ));
        }
        Ok(notes)
    }

    fn crate_report(&self, unit: &Unit, observed: &Observed) -> CrateReport {
        CrateReport {
            name: unit.name.clone(),
            previous_version: unit.previous.as_ref().map(Version::to_string),
            version: unit.version.to_string(),
            tag: unit.tag.clone(),
            reason: None,
            notes: None,
            registry: observed
                .registry
                .unwrap_or(if unit.publish { Effect::Pending } else { Effect::Skipped }),
            github_release: if !self.github_releases {
                Effect::Skipped
            } else if observed.release.as_ref().is_some_and(|release| !release.draft) {
                Effect::Done
            } else {
                Effect::Pending
            },
            missing_assets: observed
                .release
                .as_ref()
                .filter(|release| release.draft)
                .map(|release| {
                    release
                        .missing_assets(&unit.assets)
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            alias: unit.alias.clone(),
            release_url: observed.release.as_ref().map(|release| release.html_url.clone()),
        }
    }

    fn crate_reports(&self, units: &[Unit], observed: &[Observed]) -> Vec<CrateReport> {
        units
            .iter()
            .zip(observed)
            .map(|(unit, observed)| self.crate_report(unit, observed))
            .collect()
    }
}

/// The source commits that verified `gh attestation verify --format json` results name.
fn attested_sources(output: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Vec<serde_json::Value>>(output)
        .unwrap_or_default()
        .iter()
        .filter_map(|result| {
            result["verificationResult"]["signature"]["certificate"]["sourceRepositoryDigest"]
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

fn release_name(unit: &Unit) -> String {
    format!("{} v{}", unit.name, unit.version)
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// Fail before any effect when Cargo has no credential for `registry`.
///
/// Cargo resolves credentials itself; this rejects only the case where it cannot have one:
/// no token in the environment or Cargo's credentials, and no credential provider.
fn require_registry_token(ctx: &WorkspaceContext, registry: &str) -> RailResult<()> {
    let (token, provider) = if registry == "crates-io" {
        (
            "CARGO_REGISTRY_TOKEN".to_owned(),
            "CARGO_REGISTRY_CREDENTIAL_PROVIDER".to_owned(),
        )
    } else {
        let name = registry.to_ascii_uppercase().replace('-', "_");
        (
            format!("CARGO_REGISTRIES_{name}_TOKEN"),
            format!("CARGO_REGISTRIES_{name}_CREDENTIAL_PROVIDER"),
        )
    };
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set(&token) || set(&provider) || set("CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS") {
        return Ok(());
    }
    let snapshot = ctx.snapshot()?;
    let cargo = snapshot.cargo_config();
    let table = |root: &serde_json::Value| {
        if registry == "crates-io" {
            root["registry"].clone()
        } else {
            root["registries"][registry].clone()
        }
    };
    let settings = table(cargo.effective_file_settings());
    let credentials = table(cargo.credential_capabilities());
    let global_providers = cargo.effective_file_settings()["registry"]["global-credential-providers"]
        .as_array()
        .is_some_and(|providers| providers.iter().any(|provider| provider != "cargo:token"));
    if !settings["token"].is_null()
        || !settings["credential-provider"].is_null()
        || !credentials["token"].is_null()
        || global_providers
    {
        return Ok(());
    }
    Err(RailError::with_help(
        format!("publishing to {registry} needs a token, and Cargo has none"),
        format!("set {token}, or run `cargo login`; in GitHub Actions use crates.io trusted publishing"),
    ))
}

/// Wait until the registry index shows the version Cargo just published.
fn wait_for_version(registry: &PublicationRegistry, unit: &Unit) -> RailResult<super::registry::Published> {
    for attempt in 0..30 {
        if let Some(published) = registry.version(&unit.name, &unit.version)? {
            return Ok(published);
        }
        std::thread::sleep(std::time::Duration::from_secs(if attempt < 5 { 1 } else { 5 }));
    }
    Err(RailError::with_help(
        format!(
            "{} {} is not visible on {} yet",
            unit.name,
            unit.version,
            registry.name()
        ),
        "run `cargo rail release` again; it checks the registry before uploading",
    ))
}

/// A throwaway clone of one commit that shares the repository's objects.
struct SourceClone {
    _directory: tempfile::TempDir,
    root: PathBuf,
    target: PathBuf,
}

impl SourceClone {
    fn new(ctx: &WorkspaceContext, git: &SystemGit, commit: &str) -> RailResult<Self> {
        let directory = tempfile::Builder::new().prefix("cargo-rail-release-").tempdir()?;
        let root = directory.path().join("source");
        let source = git.run_git_stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let root_text = root
            .to_str()
            .ok_or_else(|| RailError::message("temporary clone path is not UTF-8"))?;
        git_with(
            &git.worktree_root,
            &["clone", "--quiet", "--shared", "--no-checkout", &source, root_text],
            &[],
            None,
        )?;
        git_with(&root, &["checkout", "--quiet", "--detach", commit], &[], None)?;
        // Reuse dependency builds across releases without touching the user's target directory.
        let target = crate::workspace::cargo_rail_state_root(ctx.workspace_root()).join("release-build");
        Ok(Self {
            _directory: directory,
            root,
            target,
        })
    }

    fn cargo(&self, ctx: &WorkspaceContext) -> RailResult<Command> {
        let mut command = Command::new(ctx.snapshot()?.toolchain().cargo_program());
        let workspace = crate::utils::path_relative_to(&ctx.git()?.git().worktree_root, ctx.workspace_root())?;
        command
            .current_dir(self.root.join(workspace))
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO_RAIL_OPERATION", "release");
        Ok(command)
    }

    /// Verify every publishing unit packages and builds, without registry or forge credentials.
    fn package(&self, ctx: &WorkspaceContext, registry: &str, units: &[Unit]) -> RailResult<()> {
        let publishing = units.iter().filter(|unit| unit.publish).collect::<Vec<_>>();
        if publishing.is_empty() {
            return Ok(());
        }
        let mut command = self.cargo(ctx)?;
        command.args(["package", "--locked", "--registry", registry]);
        for unit in &publishing {
            command.args(["--package", &unit.name]);
        }
        super::process::remove_release_credentials(&mut command);
        let output = command.output()?;
        if !output.status.success() {
            return Err(RailError::with_help(
                format!(
                    "`cargo package` failed for the release commit:\n{}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                "fix the package, then run `cargo rail release` again; nothing was released",
            ));
        }
        Ok(())
    }

    /// Upload one already verified unit.
    fn publish(&self, ctx: &WorkspaceContext, registry: &str, unit: &Unit) -> RailResult<()> {
        let mut command = self.cargo(ctx)?;
        // The release commit was packaged and built before any effect; uploading runs no build scripts.
        command.args([
            "publish",
            "--locked",
            "--no-verify",
            "--registry",
            registry,
            "--package",
            &unit.name,
        ]);
        let output = command.output()?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already exists") {
            // An earlier upload landed; the caller proves it came from this commit.
            return Ok(());
        }
        Err(RailError::with_help(
            format!(
                "`cargo publish` failed for {} {}:\n{}",
                unit.name,
                unit.version,
                stderr.trim()
            ),
            "run `cargo rail release` again; it checks the registry before uploading",
        ))
    }
}

/// Run Git in `directory` with extra environment and optional standard input.
fn git_with(directory: &Path, arguments: &[&str], env: &[(&str, &str)], input: Option<&[u8]>) -> RailResult<String> {
    use std::io::Write as _;
    // The isolated command drops inherited GIT_DIR, GIT_WORK_TREE, and GIT_INDEX_FILE, such as a hook's.
    let mut command = crate::git::git_cmd_for_path(directory);
    command
        .args(arguments)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .envs(env.iter().copied())
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("git input is unavailable"))?
            .write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "git {} failed: {}",
            arguments.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

// ----- Preview -----------------------------------------------------------------------------

fn render_new_release(session: &Session<'_>, plan: &ReleasePlan, units: &[Unit]) {
    if session.options.json {
        return;
    }
    let mut out = String::new();
    out.push_str(&format!(
        "Release from {} ({}):\n",
        session.branch,
        short(&session.git.head_commit().unwrap_or_default())
    ));
    for (unit, planned) in units.iter().zip(&plan.crates) {
        out.push_str(&format!(
            "\n  {} {} -> {}  ({})\n",
            unit.name, planned.current_version, unit.version, planned.bump_reason
        ));
        out.push_str(&format!("    tag            {}\n", unit.tag));
        out.push_str(&format!(
            "    registry       {}\n",
            match (unit.publish, session.config.publish.registry()) {
                (true, Some(registry)) => format!("publish to {registry}"),
                _ => "not published".to_owned(),
            }
        ));
        if session.github_releases {
            out.push_str(&format!(
                "    GitHub release {}\n",
                if unit.assets.is_empty() {
                    "published with the changelog section".to_owned()
                } else {
                    format!("draft until {} asset(s) are attached", unit.assets.len())
                }
            ));
        }
        if let Some(alias) = &unit.alias {
            out.push_str(&format!("    alias          {alias}\n"));
        }
        for line in planned.changelog_body.trim().lines() {
            out.push_str(&format!("    | {line}\n"));
        }
    }
    if !plan.skipped.is_empty() {
        out.push_str("\n  unchanged: ");
        out.push_str(
            &plan
                .skipped
                .iter()
                .map(|skip| skip.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push('\n');
    }
    let mut effects = vec!["commits the versions and changelogs".to_owned()];
    if session.options.pull_request {
        effects.push("opens a release pull request".to_owned());
    } else {
        effects.push(format!("pushes {} with the tags", session.branch));
        if let Some(registry) = session.config.publish.registry()
            && units.iter().any(|unit| unit.publish)
        {
            effects.push(format!("publishes to {registry}"));
        }
        if session.github_releases {
            effects.push("creates GitHub releases".to_owned());
        }
    }
    let last = effects.pop().unwrap_or_default();
    let effects = if effects.is_empty() {
        last
    } else {
        format!("{}, and {last}", effects.join(", "))
    };
    out.push_str(&format!("\nThis {effects}."));
    println!("{out}");
}

fn render_finish(session: &Session<'_>, commit: &str, units: &[Unit], observed: &[Observed]) {
    if session.options.json {
        return;
    }
    println!("Finish the release at {}:", short(commit));
    for (unit, observed) in units.iter().zip(observed) {
        let mut steps = Vec::new();
        if observed.tag_commit.is_none() {
            steps.push(format!("tag {}", unit.tag));
        }
        if observed.registry == Some(Effect::Pending) {
            steps.push("publish to the registry".to_owned());
        }
        if session.github_releases && observed.release.as_ref().is_none_or(|release| release.draft) {
            steps.push("publish the GitHub release".to_owned());
        }
        if let Some(alias) = &unit.alias
            && observed.alias_commit.is_none()
        {
            steps.push(format!("move alias {alias}"));
        }
        println!("  {} {}: {}", unit.name, unit.version, steps.join(", "));
    }
}
