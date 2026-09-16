//! Release execution and publishing to crates.io and forge releases.

use crate::config::{ReleaseConfig, ReleaseRemoteEffects};
use crate::error::{RailError, RailResult};
use crate::release::packages;
use crate::release::planner::{CrateReleasePlan, RELEASE_REGISTRY, ReleasePlan};
use crate::release::process;
use crate::release::registry::RegistryObservation;
use crate::release::remote::{RemoteRepository, release_repository};
use crate::release::state::{
    BackupRestorePolicy, Preparation, ReleasePhase, ReleaseState, ReleaseStateCreate, ReleaseStatus, StepStatus,
    normalize_release_path, normalize_release_paths, validate_state_path,
};
use crate::release::version::VersionBumper;
use crate::source::ContentDigest;
use crate::utils::canonicalize_existing;
use crate::workspace::WorkspaceContext;
use crate::{progress, warn};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES: usize = 120_000;
const RELEASE_REMOTE: &str = "origin";
const RELEASE_OPERATION_ENV: &[(&str, &str)] = &[("CARGO_RAIL_OPERATION", "release")];
pub(super) const RELEASE_PUSH_ENV: &[(&str, &str)] =
    &[("CARGO_RAIL_OPERATION", "release"), ("CARGO_RAIL_RELEASE_PUSH", "1")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseForge {
    Github,
    Gitlab,
}

struct ReleasePreflight {
    warnings: Vec<String>,
    remote_repository: Option<RemoteRepository>,
}

pub(crate) enum CheckReadiness {
    Green(String),
    Waiting(String),
    Failed(String),
}

impl ReleaseForge {
    fn name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Gitlab => "GitLab",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Github => "gh",
            Self::Gitlab => "glab",
        }
    }
}

/// Release publisher
#[derive(Debug)]
pub struct ReleasePublisher<'a> {
    /// Workspace context
    ctx: &'a WorkspaceContext,
    /// Release configuration
    release_config: &'a ReleaseConfig,
}

impl<'a> ReleasePublisher<'a> {
    /// Create a new release publisher
    pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
        Self { ctx, release_config }
    }

    /// Pre-flight validation: check all prerequisites before starting release
    ///
    /// This catches issues early rather than failing mid-release.
    pub fn preflight_check(&self, plan: &ReleasePlan, skip_publish: bool, skip_tag: bool) -> RailResult<Vec<String>> {
        Ok(self.preflight(plan, skip_publish, skip_tag)?.warnings)
    }

    fn preflight(&self, plan: &ReleasePlan, skip_publish: bool, skip_tag: bool) -> RailResult<ReleasePreflight> {
        for package in &plan.crates {
            package.api_evidence.require_ready(&package.name)?;
            if package.api_evidence.required
                != (self.release_config.semver_check == crate::config::SemverCheckPolicy::Deny)
            {
                return Err(RailError::message(
                    "API evidence does not match the captured release policy",
                ));
            }
        }
        let mut warnings = Vec::new();
        let git = self.ctx.git()?.git();

        if !skip_publish && self.release_config.registry_publication.registry() != Some(RELEASE_REGISTRY) {
            return Err(RailError::with_help(
                "registry publication is not authorized by the persisted release configuration",
                "set release.registry_publication = \"crates-io\" and pass --publish only after reviewing the exact plan",
            ));
        }

        if !skip_tag {
            let mut tags = BTreeSet::new();
            for crate_plan in &plan.crates {
                let tag_ref = format!("refs/tags/{}", crate_plan.tag_name);
                if crate_plan.tag_name.starts_with('-') || !git.run_git_check(&["check-ref-format", &tag_ref]) {
                    return Err(RailError::with_help(
                        format!("release tag '{}' is not a safe Git ref name", crate_plan.tag_name),
                        "fix release.tag_prefix or release.tag_format before publishing",
                    ));
                }
                if !tags.insert(&crate_plan.tag_name) {
                    return Err(RailError::with_help(
                        format!(
                            "release plan assigns tag '{}' to more than one crate",
                            crate_plan.tag_name
                        ),
                        "include {crate} in release.tag_format so every release commit has one unambiguous tag",
                    ));
                }
            }
        }

        let explicit_forge = match self.release_config.remote_effects {
            ReleaseRemoteEffects::Github => Some(ReleaseForge::Github),
            ReleaseRemoteEffects::Gitlab => Some(ReleaseForge::Gitlab),
            _ => None,
        };
        if self.release_config.remote_effects.creates_forge_release()
            && !skip_tag
            && let Some(forge) = explicit_forge
        {
            let binary = forge.binary();
            if !process::succeeds(binary, &["--version"], None) {
                return Err(RailError::with_help(
                    format!("{} releases enabled but {} CLI was not found", forge.name(), binary),
                    format!("install {} or set release.remote_effects = \"push\"", binary),
                ));
            }
        }

        let mut remote_repository = None;
        if self.release_config.remote_effects.pushes() {
            if !git.has_remote(RELEASE_REMOTE)? {
                return Err(RailError::with_help(
                    "release push enabled but remote 'origin' does not exist",
                    "add an origin remote or set [release].remote_effects = \"none\"",
                ));
            }

            let repository = release_repository(self.ctx.workspace_root())?;
            let release_forge = self.release_forge(&repository).ok();
            if release_forge == Some(ReleaseForge::Github)
                && (!skip_tag || !skip_publish)
                && self.release_config.validation.is_empty()
            {
                return Err(RailError::message(
                    "GitHub releases require release.validation workflow paths and required job names",
                ));
            }

            if self.release_config.remote_effects.creates_forge_release() && !skip_tag {
                let forge = self.release_forge(&repository)?;
                let binary = forge.binary();
                if explicit_forge.is_none() && !process::succeeds(binary, &["--version"], None) {
                    return Err(RailError::with_help(
                        format!("{} releases enabled but {} CLI was not found", forge.name(), binary),
                        format!("install {} or set release.remote_effects = \"push\"", binary),
                    ));
                }
                self.validate_forge_repository(forge, &repository)?;
                if forge == ReleaseForge::Github && !self.github_auth_succeeds(&repository) {
                    return Err(RailError::with_help(
                        "GitHub CLI is not authenticated",
                        "run 'gh auth login' or provide GITHUB_TOKEN in CI",
                    ));
                }
                for crate_plan in &plan.crates {
                    if self.forge_release_exists(forge, &repository, &crate_plan.tag_name) {
                        warnings.push(format!(
                            "{} release '{}' already exists; cargo-rail will reuse it",
                            forge.name(),
                            crate_plan.tag_name
                        ));
                    }
                }
            }

            if !skip_tag {
                for crate_plan in &plan.crates {
                    if self.remote_tag_exists(&crate_plan.tag_name)? {
                        return Err(RailError::with_help(
                            format!("remote tag '{}' already exists", crate_plan.tag_name),
                            "choose a new version or inspect the existing release state before rerunning",
                        ));
                    }
                }
            }

            if !skip_tag || !skip_publish {
                let forge = release_forge.ok_or_else(|| self.unsupported_readiness_error())?;
                let binary = forge.binary();
                if !process::succeeds(binary, &["--version"], None) {
                    return Err(RailError::with_help(
                        format!("{} readiness requires the {} CLI", forge.name(), binary),
                        format!(
                            "install {} so cargo-rail can observe checks for the exact release SHA",
                            binary
                        ),
                    ));
                }
                self.validate_forge_repository(forge, &repository)?;
                if forge == ReleaseForge::Github && !self.github_auth_succeeds(&repository) {
                    return Err(RailError::with_help(
                        "GitHub CLI is not authenticated",
                        "run 'gh auth login' or provide GITHUB_TOKEN in CI",
                    ));
                }
            }
            remote_repository = Some(repository);
        }

        if self.release_config.sign_tags && !skip_tag && !git.has_signing_configured() {
            return Err(RailError::with_help(
                "release requires signed tags but no signing key is configured",
                "configure the signing key before preparing a release; registry publication cannot precede missing signing authority",
            ));
        }

        Ok(ReleasePreflight {
            warnings,
            remote_repository,
        })
    }

    /// Execute a release plan
    #[expect(
        clippy::too_many_arguments,
        reason = "the irreversible release boundary keeps transaction and path authorities explicit"
    )]
    pub fn execute(
        &self,
        transaction_id: &str,
        plan: &ReleasePlan,
        skip_publish: bool,
        skip_tag: bool,
        local: bool,
        executor: bool,
        prepare_only: bool,
        retain_remote: bool,
        review: bool,
        planned_paths: &[PathBuf],
        control_paths: &[PathBuf],
    ) -> RailResult<()> {
        // Run pre-flight checks
        let preflight = self.preflight(plan, skip_publish, skip_tag)?;
        for warning in &preflight.warnings {
            warn!("{}", warning);
        }

        let _lock = crate::release::state::lock(self.ctx.workspace_root())?;
        let git = self.ctx.git()?.git();
        if executor {
            super::hosted::event(self.ctx.workspace_root(), self.release_config)?;
            if std::env::var("GITHUB_SHA").ok().as_deref() != Some(git.head_commit()?.as_str()) {
                return Err(RailError::message(
                    "new hosted request must start at its event's exact source commit",
                ));
            }
        }
        if !local && self.release_config.hosted_workflow.is_some() && git.is_dirty()? {
            return Err(RailError::message(
                "hosted release requests require committed, clean inputs",
            ));
        }
        let (mut state, state_path) = ReleaseState::create(ReleaseStateCreate {
            root: self.ctx.workspace_root(),
            transaction_id: transaction_id.to_string(),
            review,
            hosted: !local && self.release_config.hosted_workflow.is_some(),
            remote_storage: retain_remote || review || !local && self.release_config.hosted_workflow.is_some(),
            plan: plan.clone(),
            release_config: self.release_config.clone(),
            remote_repository: preflight.remote_repository,
            skip_publish,
            skip_tag,
            initial_head: git.head_commit()?,
            branch: git.current_branch()?,
            planned_paths: planned_paths.to_vec(),
            control_paths: control_paths.to_vec(),
        })?;
        progress!("release transaction: {}", state.transaction_id);
        if state.intent.hosted && executor {
            state.executor = Some(
                std::env::var("GITHUB_RUN_ID")
                    .ok()
                    .and_then(|id| id.parse().ok())
                    .filter(|id| *id > 0)
                    .ok_or_else(|| RailError::message("executor has no GitHub run identity"))?,
            );
            state.save(&state_path)?;
        }
        if state.intent.hosted && !executor {
            super::hosted::dispatch(self.ctx.workspace_root(), &state)?;
            return Ok(());
        }
        if let Err(error) = self.execute_state(&mut state, &state_path, executor, prepare_only) {
            return Err(error.context(format!(
                "release is recoverable from '{}'\nresume with: cargo rail release resume {}",
                state.transaction_id, state.transaction_id
            )));
        }

        Ok(())
    }

    /// Resume a previously interrupted release without replanning mutated inputs.
    pub fn resume(&self, state_path: &std::path::Path, executor: bool) -> RailResult<()> {
        let _lock = crate::release::state::lock(self.ctx.workspace_root())?;
        let state_path = validate_state_path(self.ctx.workspace_root(), state_path)?;
        let mut state = ReleaseState::load_for_recovery(&state_path)?;
        state.validate_recovery_paths(&self.ctx.git()?.git().worktree_root)?;
        if state.status != ReleaseStatus::Active {
            return Err(RailError::message(format!(
                "release state is {:?}, not active",
                state.status
            )));
        }
        let persisted_config = serde_json::to_value(&state.intent.release_config)?;
        let current_config = serde_json::to_value(self.release_config)?;
        if persisted_config != current_config {
            let changed = differing_json_fields(&persisted_config, &current_config);
            return Err(RailError::with_help(
                format!(
                    "release configuration changed since execution began: {}",
                    changed.join(", ")
                ),
                "restore the original release configuration before resuming; the persisted side-effect contract cannot change mid-release",
            ));
        }
        if state.intent.hosted && !executor {
            super::hosted::dispatch(self.ctx.workspace_root(), &state)?;
            return Ok(());
        }
        if executor {
            super::hosted::event(self.ctx.workspace_root(), self.release_config)?;
            state.executor = Some(
                std::env::var("GITHUB_RUN_ID")
                    .ok()
                    .and_then(|id| id.parse().ok())
                    .filter(|id| *id > 0)
                    .ok_or_else(|| RailError::message("executor has no GitHub run identity"))?,
            );
            state.save(&state_path)?;
        }
        let current_branch = self.ctx.git()?.git().current_branch()?;
        if current_branch != state.intent.branch
            && (!state.intent.review || current_branch != super::review::branch(&state))
        {
            return Err(RailError::with_help(
                format!("release resume requires branch '{}'", state.intent.branch),
                format!("git switch {}", state.intent.branch),
            ));
        }
        if state.release_commit().is_some() && !state.intent.review {
            self.validate_release_head(&state)?;
        }
        progress!("resuming release {}", state.transaction_id);
        self.execute_state(&mut state, &state_path, executor, false)
    }

    /// Abort an active release, optionally retaining its exact pushed preparation.
    pub fn abort(&self, state_path: &std::path::Path, retain_preparation: bool) -> RailResult<()> {
        let _lock = crate::release::state::lock(self.ctx.workspace_root())?;
        let state_path = validate_state_path(self.ctx.workspace_root(), state_path)?;
        let mut state = ReleaseState::load_for_recovery(&state_path)?;
        state.validate_recovery_paths(&self.ctx.git()?.git().worktree_root)?;
        if state.status != ReleaseStatus::Active {
            return Err(RailError::message(format!("release state is {:?}", state.status)));
        }
        if retain_preparation {
            return self.abort_retaining_preparation(&mut state, &state_path);
        }
        let pushes = state.intent.release_config.remote_effects.pushes();
        let forge = state.intent.release_config.remote_effects.creates_forge_release() && !state.intent.skip_tag;
        let push_is_proven_absent = if pushes && state.commit_push.status == StepStatus::InProgress {
            self.validate_remote_repository(&state)?;
            self.remote_push_is_absent(&state)?
        } else {
            false
        };
        let package_attempt = state.package_seal.as_ref().is_some_and(|seal| {
            seal.packages
                .iter()
                .any(|archive| archive.attempt_path(&packages::directory(&state_path)).exists())
        });
        let irreversible = state
            .review
            .as_ref()
            .is_some_and(|review| step_may_have_side_effect(&review.pushed))
            || package_attempt
            || pushes
                && ((!push_is_proven_absent && step_may_have_side_effect(&state.commit_push))
                    || step_may_have_side_effect(&state.tag_push))
            || state
                .crates
                .iter()
                .zip(&state.intent.plan.crates)
                .any(|(crate_state, planned)| {
                    forge
                        && (step_may_have_side_effect(&crate_state.forge_draft)
                            || step_may_have_side_effect(&crate_state.forge_publication))
                        || !state.intent.skip_publish
                            && planned.publish
                            && step_may_have_side_effect(&crate_state.publication)
                });
        if irreversible {
            return Err(RailError::with_help(
                "release abort refused because a remote or registry side effect may already exist",
                format!(
                    "resume with 'cargo rail release resume {}'; cargo-rail will reconcile the external state",
                    state.transaction_id
                ),
            ));
        }

        let git = self.ctx.git()?.git();
        if git.current_branch()? != state.intent.branch {
            return Err(RailError::with_help(
                format!("release abort requires branch '{}'", state.intent.branch),
                format!("git switch {}", state.intent.branch),
            ));
        }
        self.ensure_only_release_paths_changed(&state)?;
        state.abort.status = StepStatus::InProgress;
        state.abort.object = Some(state.intent.initial_head.clone());
        state.save(&state_path)?;

        for (crate_plan, crate_state) in state.intent.plan.crates.iter().zip(&state.crates) {
            if crate_state.tag.status != StepStatus::Pending
                && let Some(target) = self.local_tag_target(&crate_plan.tag_name)?
            {
                let reference = format!("refs/tags/{}", crate_plan.tag_name);
                let object = git.run_git_stdout(&["rev-parse", "--verify", &reference])?;
                if state.release_commit() != Some(target.as_str())
                    || crate_state
                        .tag_object
                        .as_ref()
                        .is_some_and(|retained| retained.id != object)
                {
                    return Err(RailError::message("release abort found a conflicting local tag object"));
                }
                git.run_git(&["update-ref", "-d", &reference, &object])?;
            }
        }
        git.run_git(&["reset", "--hard", &state.intent.initial_head])?;
        self.clean_untracked_planned_paths(&state)?;
        self.restore_local_input_backups(&state, true)?;

        state.abort.status = StepStatus::Complete;
        state.status = ReleaseStatus::Aborted;
        state.save(&state_path)?;
        progress!("release aborted and restored to {}", state.intent.initial_head);
        Ok(())
    }

    fn abort_retaining_preparation(&self, state: &mut ReleaseState, state_path: &Path) -> RailResult<()> {
        let release_commit = state
            .release_commit()
            .map(str::to_owned)
            .ok_or_else(|| RailError::message("release has no exact preparation commit to retain"))?;
        let package_attempt = state.package_seal.as_ref().is_some_and(|seal| {
            seal.packages
                .iter()
                .any(|archive| archive.attempt_path(&packages::directory(state_path)).exists())
        });
        let forge = state.intent.release_config.remote_effects.creates_forge_release() && !state.intent.skip_tag;
        let publication_effect = state
            .crates
            .iter()
            .zip(&state.intent.plan.crates)
            .any(|(crate_state, planned)| {
                (!state.intent.skip_tag
                    && (step_may_have_side_effect(&crate_state.tag) || crate_state.tag_object.is_some()))
                    || (!state.intent.skip_publish
                        && planned.publish
                        && (step_may_have_side_effect(&crate_state.publication)
                            || crate_state.publication_attempt.is_some()))
                    || (forge
                        && (step_may_have_side_effect(&crate_state.forge_draft)
                            || step_may_have_side_effect(&crate_state.forge_publication)))
                    || step_may_have_side_effect(&crate_state.alias)
            });
        if state.review.is_some() || package_attempt || step_may_have_side_effect(&state.tag_push) || publication_effect
        {
            return Err(RailError::with_help(
                "release preparation cannot be retained because a tag, registry, review, forge, or alias effect may exist",
                format!(
                    "resume with 'cargo rail release resume {}'; cargo-rail will reconcile the external state",
                    state.transaction_id
                ),
            ));
        }
        if !state.intent.release_config.remote_effects.pushes()
            || state.commit_push.status == StepStatus::Pending
            || state.commit_push.object.as_deref() != Some(release_commit.as_str())
        {
            return Err(RailError::with_help(
                "release has no observed pushed preparation to retain",
                format!(
                    "use 'cargo rail release abort {} --yes' while the transaction is still local",
                    state.transaction_id
                ),
            ));
        }

        self.validate_remote_repository(state)?;
        let git = self.ctx.git()?.git();
        if git.current_branch()? != state.intent.branch {
            return Err(RailError::with_help(
                format!("release abort requires branch '{}'", state.intent.branch),
                format!("git switch {}", state.intent.branch),
            ));
        }
        if git.is_dirty()? {
            return Err(RailError::with_help(
                format!(
                    "release checkout has uncommitted content: {}",
                    git.dirty_files()?.join(", ")
                ),
                "commit or restore the worktree before retaining the pushed preparation",
            ));
        }
        let current_head = git.head_commit()?;
        let remote_head = self
            .remote_ref_target(&format!("refs/heads/{}", state.intent.branch))?
            .ok_or_else(|| RailError::message("release branch is absent from the retained remote"))?;
        if current_head != remote_head {
            return Err(RailError::with_help(
                format!("release branch differs between the checkout ({current_head}) and origin ({remote_head})"),
                "synchronize the exact release branch before retaining the pushed preparation",
            ));
        }
        if !git.run_git_check(&["merge-base", "--is-ancestor", &release_commit, &current_head]) {
            return Err(RailError::with_help(
                format!("release preparation {release_commit} is not an ancestor of {current_head}"),
                "restore a branch that contains the exact pushed preparation before aborting",
            ));
        }

        state.commit_push.status = StepStatus::Complete;
        state.commit_push.object = Some(release_commit.clone());
        state.abort.status = StepStatus::InProgress;
        state.abort.object = Some(release_commit.clone());
        state.save(state_path)?;
        state.abort.status = StepStatus::Complete;
        state.status = ReleaseStatus::Aborted;
        state.save(state_path)?;
        progress!("release aborted; retained pushed preparation {release_commit}");
        Ok(())
    }

    fn execute_state(
        &self,
        state: &mut ReleaseState,
        state_path: &std::path::Path,
        wait_for_checks: bool,
        prepare_only: bool,
    ) -> RailResult<()> {
        self.validate_remote_repository(state)?;
        if state.phase < ReleasePhase::Prepared {
            let plan = state.intent.execution_plan(&self.ctx.git()?.git().worktree_root)?;
            for plan in &plan.crates {
                crate::release::presentation::validate_inputs(self.ctx.workspace_root(), plan)?;
            }
        }
        if !matches!(state.preparation, Preparation::Complete { .. }) {
            super::review::prepare_branch(self.ctx.git()?.git(), state)?;
            self.reconcile_preparation(state, state_path)?;
        }
        advance_phase(state, state_path, ReleasePhase::Prepared)?;
        if !super::review::reconcile(self.ctx.git()?.git(), state, state_path)? {
            return Ok(());
        }
        self.validate_release_head(state)?;
        state.validate_preparation_binding(self.ctx.git()?.git())?;
        if prepare_only {
            progress!(
                "release {} is prepared at {}",
                state.transaction_id,
                state.release_commit().unwrap_or_default()
            );
            return Ok(());
        }
        self.reconcile_packages(state, state_path)?;
        self.reconcile_commit_push(state, state_path)?;
        advance_phase(state, state_path, ReleasePhase::AwaitingChecks)?;
        super::hosted::dispatch_validation(self.ctx.workspace_root(), state, state_path)?;
        self.reconcile_readiness(state, state_path, wait_for_checks)?;
        let assets = super::artifacts::acquire(&self.ctx.git()?.git().worktree_root, state, state_path)?;
        advance_phase(state, state_path, ReleasePhase::Ready)?;
        advance_phase(state, state_path, ReleasePhase::Publishing)?;
        self.reconcile_local_tags(state, state_path)?;
        self.reconcile_publications(state, state_path)?;
        self.reconcile_tag_push(state, state_path)?;
        self.reconcile_forge_drafts(state, state_path, &assets)?;
        self.reconcile_forge_publications(state, state_path, &assets)?;
        super::aliases::reconcile(self.ctx.workspace_root(), state, state_path)?;
        state.status = ReleaseStatus::Complete;
        state.phase = ReleasePhase::Released;
        state.save(state_path)?;
        progress!("\nrelease complete");

        Ok(())
    }

    fn validate_release_head(&self, state: &ReleaseState) -> RailResult<()> {
        let expected = state
            .release_commit()
            .ok_or_else(|| RailError::message("prepared release has no exact release commit"))?;
        let git = self.ctx.git()?.git();
        let actual = git.head_commit()?;
        if actual != expected {
            return Err(RailError::with_help(
                format!(
                    "release checkout is at {}, but the persisted release commit is {}",
                    actual, expected
                ),
                format!(
                    "restore a clean checkout of {} on branch '{}' before resuming",
                    expected, state.intent.branch
                ),
            ));
        }
        Ok(())
    }

    fn validate_publish_checkout(&self, state: &ReleaseState) -> RailResult<()> {
        self.validate_release_head(state)?;
        let git = self.ctx.git()?.git();
        if git.is_dirty()? {
            return Err(RailError::with_help(
                format!(
                    "release checkout has uncommitted content: {}",
                    git.dirty_files()?.join(", ")
                ),
                "restore a clean checkout before publishing; cargo packages ambient worktree bytes",
            ));
        }
        Ok(())
    }

    fn reconcile_preparation(&self, state: &mut ReleaseState, state_path: &Path) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        if state.reconcile_preparation(git)? {
            return state.save(state_path);
        }
        if matches!(state.preparation, Preparation::Writing | Preparation::Committing { .. }) {
            crate::release::state::restore_preparation(self.ctx.workspace_root(), state_path)?;
        } else {
            state.preparation = Preparation::Writing;
            state.save(state_path)?;
        }
        let prepared = (|| {
            self.prepare_files(&state.intent.execution_plan(&git.worktree_root)?)?;
            for plan in &state.intent.plan.crates {
                self.validate_release_notes_size(plan, state.intent.skip_tag, state.intent.remote_repository.as_ref())?;
            }
            self.stage_planned_paths(&state.intent.planned_paths, &state.intent.control_paths)?;
            git.run_git_stdout(&["write-tree"])
        })();
        let tree = match prepared {
            Ok(tree) => tree,
            Err(error) => {
                crate::release::state::restore_preparation(self.ctx.workspace_root(), state_path)?;
                return Err(error);
            }
        };
        if let Preparation::Committing { tree: expected } = &state.preparation
            && expected != &tree
        {
            return Err(RailError::message(
                "release preparation no longer produces the saved commit tree",
            ));
        }
        state.preparation = Preparation::Committing { tree };
        state.save(state_path)?;
        if let Err(error) = git.commit_with_env(&state.preparation_message(), RELEASE_OPERATION_ENV) {
            if git.head_commit()? == state.intent.initial_head {
                crate::release::state::restore_preparation(self.ctx.workspace_root(), state_path)?;
            }
            return Err(error);
        }
        state.reconcile_preparation(git)?;
        state.save(state_path)
    }

    fn prepare_files(&self, plan: &ReleasePlan) -> RailResult<()> {
        for crate_plan in &plan.crates {
            progress!(
                "  version: {} -> {}",
                crate_plan.current_version,
                crate_plan.new_version
            );
            self.bump_crate_version(crate_plan)?;
            if !crate_plan.affected_dependents.is_empty() {
                self.update_dependents(crate_plan)?;
            }
            self.update_changelog(crate_plan)?;
        }
        self.consume_change_files(plan)?;
        self.update_lockfile(plan)?;
        self.write_auxiliary_lockfiles(plan)
    }

    fn reconcile_local_tags(&self, state: &mut ReleaseState, state_path: &Path) -> RailResult<()> {
        super::tags::reconcile(self.ctx.git()?.git(), state, state_path)
    }

    fn reconcile_commit_push(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        let release_commit = state
            .release_commit()
            .map(str::to_owned)
            .ok_or_else(|| RailError::message("prepared release has no exact release commit"))?;
        if !self.release_config.remote_effects.pushes() {
            state.commit_push.status = StepStatus::Complete;
            state.commit_push.object = Some(release_commit);
            state.save(state_path)?;
            return Ok(());
        }
        self.validate_remote_repository(state)?;
        if state.commit_push.is_complete() {
            return Ok(());
        }
        if self.remote_commit_matches(state, &release_commit)? {
            state.commit_push.status = StepStatus::Complete;
            state.commit_push.object = Some(release_commit);
            state.save(state_path)?;
            return Ok(());
        }
        state.commit_push.status = StepStatus::InProgress;
        state.commit_push.object = Some(release_commit.clone());
        state.save(state_path)?;
        self.validate_release_head(state)?;

        self.push_release_commit(
            &state.intent.branch,
            &state.intent.initial_head,
            &release_commit,
            state
                .intent
                .remote_repository
                .as_ref()
                .ok_or_else(|| RailError::message("release push has no repository identity"))?,
        )?;

        if !self.remote_commit_matches(state, &release_commit)? {
            return Err(RailError::message(format!(
                "release commit {} is not observable at origin/{} after push",
                release_commit, state.intent.branch
            )));
        }
        state.commit_push.status = StepStatus::Complete;
        state.commit_push.object = Some(release_commit);
        state.save(state_path)
    }

    fn reconcile_readiness(
        &self,
        state: &mut ReleaseState,
        state_path: &std::path::Path,
        wait_for_checks: bool,
    ) -> RailResult<()> {
        if state.readiness.is_complete() && state.validation.is_empty() {
            return Ok(());
        }
        let release_commit = state
            .release_commit()
            .map(str::to_owned)
            .ok_or_else(|| RailError::message("prepared release has no exact release commit"))?;
        if !self.release_config.remote_effects.pushes() || state.intent.skip_tag && state.intent.skip_publish {
            state.readiness.status = StepStatus::Complete;
            state.readiness.object = Some(format!("not_required:{}", release_commit));
            state.save(state_path)?;
            return Ok(());
        }

        self.validate_remote_repository(state)?;
        let repository = state
            .intent
            .remote_repository
            .as_ref()
            .ok_or_else(|| RailError::message("release readiness has no repository identity"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(7200);
        loop {
            let observation = match self.release_forge(repository)? {
                ReleaseForge::Github => match super::validation::observe(
                    self.ctx.workspace_root(),
                    repository,
                    &release_commit,
                    &state.intent.release_config.validation,
                    &state.validation,
                    &state.validation_dispatches,
                )? {
                    super::validation::Observation::Verified(runs) => {
                        state.validation = runs;
                        CheckReadiness::Green(format!("required workflows verified for {release_commit}"))
                    }
                    super::validation::Observation::Waiting(detail) => CheckReadiness::Waiting(detail),
                    super::validation::Observation::Failed(detail) => CheckReadiness::Failed(detail),
                },
                ReleaseForge::Gitlab => {
                    observe_gitlab_repository_readiness(self.ctx.workspace_root(), repository, &release_commit)?
                }
            };
            match observation {
                CheckReadiness::Green(detail) => {
                    state.readiness.status = StepStatus::Complete;
                    state.readiness.object = Some(detail);
                    return state.save(state_path);
                }
                CheckReadiness::Waiting(detail) => {
                    state.readiness.object = Some(detail.clone());
                    state.save(state_path)?;
                    if !wait_for_checks || std::time::Instant::now() >= deadline {
                        return Err(readiness_wait_error(state_path, &release_commit, &detail));
                    }
                    progress!("release commit {} is awaiting checks: {}", release_commit, detail);
                    std::thread::sleep(release_readiness_poll_interval());
                }
                CheckReadiness::Failed(detail) => {
                    state.readiness.object = Some(detail.clone());
                    state.save(state_path)?;
                    return Err(RailError::with_help(
                        format!("release checks failed for exact commit {}: {}", release_commit, detail),
                        "fix the failing checks without moving or replacing the release commit; then resume the release",
                    ));
                }
            }
        }
    }

    fn reconcile_tag_push(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if !self.release_config.remote_effects.pushes() || state.intent.skip_tag {
            state.tag_push.status = StepStatus::Complete;
            state.tag_push.object = state.release_commit().map(str::to_owned);
            state.save(state_path)?;
            return Ok(());
        }
        self.validate_remote_repository(state)?;
        if state.tag_push.is_complete() {
            return Ok(());
        }
        if self.remote_tags_match(state)? {
            state.tag_push.status = StepStatus::Complete;
            state.tag_push.object = state.release_commit().map(str::to_owned);
            state.save(state_path)?;
            return Ok(());
        }
        state.tag_push.status = StepStatus::InProgress;
        state.tag_push.object = state.release_commit().map(str::to_owned);
        state.save(state_path)?;

        self.push_release_tags(
            &state.intent.plan,
            state
                .intent
                .remote_repository
                .as_ref()
                .ok_or_else(|| RailError::message("release tag push has no repository identity"))?,
        )?;

        if !self.remote_tags_match(state)? {
            return Err(RailError::message(
                "release tags are not observable on origin after push",
            ));
        }
        state.tag_push.status = StepStatus::Complete;
        state.tag_push.object = state.release_commit().map(str::to_owned);
        state.save(state_path)
    }

    fn reconcile_forge_drafts(
        &self,
        state: &mut ReleaseState,
        state_path: &Path,
        assets: &super::artifacts::Assets,
    ) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release() || state.intent.skip_tag {
            for package in &mut state.crates {
                package.forge_draft.status = StepStatus::Complete;
                package.forge_publication.status = StepStatus::Complete;
            }
            return state.save(state_path);
        }
        self.reconcile_forge(state, state_path, assets, false)
    }

    fn reconcile_forge(
        &self,
        state: &mut ReleaseState,
        state_path: &Path,
        assets: &super::artifacts::Assets,
        publish: bool,
    ) -> RailResult<()> {
        self.validate_remote_repository(state)?;
        let repository = state
            .intent
            .remote_repository
            .clone()
            .ok_or_else(|| RailError::message("forge release has no repository identity"))?;
        let forge = self.release_forge(&repository)?;
        let source = state
            .release_commit()
            .ok_or_else(|| RailError::message("forge release has no prepared commit"))?
            .to_owned();
        for package in state.intent.plan.crates.clone() {
            let index = state.crate_index(&package.name)?;
            if forge == ReleaseForge::Gitlab {
                if !state.crates[index].forge_draft.is_complete() {
                    state.crates[index].forge_draft.status = StepStatus::InProgress;
                    state.crates[index].forge_draft.object = Some(package.tag_name.clone());
                    state.save(state_path)?;
                    if !self.forge_release_exists(forge, &repository, &package.tag_name) {
                        self.create_gitlab_release(&repository, &package)?;
                    }
                    state.crates[index].forge_draft.status = StepStatus::Complete;
                    state.crates[index].forge_publication.status = StepStatus::Complete;
                    state.crates[index].forge_publication.object = Some(package.tag_name);
                    state.save(state_path)?;
                }
                continue;
            }
            let retained_id = state.crates[index].forge_draft.object.clone();
            let step = if publish {
                &mut state.crates[index].forge_publication
            } else {
                &mut state.crates[index].forge_draft
            };
            if !step.is_complete() {
                step.status = StepStatus::InProgress;
            }
            state.save(state_path)?;
            let evidence = state
                .artifacts
                .iter()
                .find(|artifact| artifact.package == package.name)
                .cloned();
            let release = super::github_release::GithubRelease {
                root: &self.ctx.git()?.git().worktree_root,
                repository: &repository,
                source: &source,
                package: &package,
                evidence: evidence.as_ref(),
                assets,
            };
            let id = release.reconcile(retained_id.as_deref(), publish, |id| {
                if state.crates[index].forge_draft.object.is_none() {
                    state.crates[index].forge_draft.object = Some(id.to_owned());
                    state.save(state_path)?;
                }
                Ok(())
            })?;
            let step = if publish {
                &mut state.crates[index].forge_publication
            } else {
                &mut state.crates[index].forge_draft
            };
            step.status = StepStatus::Complete;
            step.object = Some(id);
            state.save(state_path)?;
        }
        Ok(())
    }

    fn reconcile_packages(&self, state: &mut ReleaseState, state_path: &Path) -> RailResult<()> {
        if state.intent.skip_publish || !state.intent.plan.crates.iter().any(|package| package.publish) {
            return Ok(());
        }
        let directory = packages::directory(state_path);
        let source = state
            .release_commit()
            .ok_or_else(|| RailError::message("package evidence requires a prepared commit"))?
            .to_owned();
        if let Some(seal) = &state.package_seal {
            seal.validate(&state.intent.plan, &source)?;
            return seal.verify_archives(&directory);
        }
        if state
            .crates
            .iter()
            .any(|package| package.publication.status != StepStatus::Pending && package.publication.object.is_some())
        {
            return Err(RailError::message(
                "original package evidence is missing after publication started; recover the retained release records",
            ));
        }
        self.validate_publish_checkout(state)?;
        let seal = packages::prepare(self.ctx, &state.intent.plan, &source, &directory)?;
        self.validate_publish_checkout(state)?;
        state.package_seal = Some(seal);
        state.save(state_path)
    }

    fn reconcile_publications(&self, state: &mut ReleaseState, state_path: &Path) -> RailResult<()> {
        if state.intent.skip_publish || !state.intent.plan.crates.iter().any(|package| package.publish) {
            for package in state.intent.plan.crates.iter().filter(|package| !package.publish) {
                progress!("  skipped publish (publish = false) for {}", package.name);
            }
            return Ok(());
        }
        let seal = state
            .package_seal
            .clone()
            .ok_or_else(|| RailError::message("sealed package evidence is missing"))?;
        let directory = packages::directory(state_path);
        seal.verify_archives(&directory)?;
        let mut remaining = Vec::new();
        for (archive, observation) in seal.packages.iter().zip(seal.observe()?) {
            let index = state.crate_index(&archive.name)?;
            match observation {
                RegistryObservation::Matching => {
                    state.crates[index].publication.status = StepStatus::Complete;
                    state.crates[index].publication.object = Some(archive.sha256.clone());
                }
                RegistryObservation::Absent
                    if !archive.attempt_path(&directory).try_exists()?
                        && state.crates[index].publication.status == StepStatus::Pending =>
                {
                    remaining.push(archive.name.as_str());
                }
                RegistryObservation::Absent => {
                    return Err(RailError::message(format!(
                        "{}@{} has an uncertain upload; an absent index entry does not authorize retry",
                        archive.name, archive.version
                    )));
                }
                RegistryObservation::Conflicting { .. } => {
                    return Err(RailError::message(format!(
                        "{}@{} conflicts with the sealed checksum or is yanked",
                        archive.name, archive.version
                    )));
                }
                RegistryObservation::Unavailable { reason } => {
                    return Err(RailError::message(format!(
                        "registry observation is unavailable for '{}': {reason}",
                        archive.name
                    )));
                }
            }
            state.save(state_path)?;
        }
        if remaining.is_empty() {
            return Ok(());
        }
        self.validate_publish_checkout(state)?;
        let result = packages::publish(self.ctx, &seal, &directory, &remaining);
        // The credential provider retains each package attempt before returning
        // upload authority, including remote durability when the executor owns it.
        *state = ReleaseState::load_for_recovery(state_path)?;
        let mut incomplete = Vec::new();
        for (archive, observation) in seal.packages.iter().zip(seal.observe()?) {
            let index = state.crate_index(&archive.name)?;
            if observation == RegistryObservation::Matching {
                state.crates[index].publication.status = StepStatus::Complete;
                state.crates[index].publication.object = Some(archive.sha256.clone());
            } else {
                if let Some(attempt) = archive.attempted(&directory)? {
                    state.crates[index].publication.status = StepStatus::InProgress;
                    state.crates[index].publication.object = Some(archive.sha256.clone());
                    state.crates[index].publication_attempt = Some(attempt);
                }
                incomplete.push(format!("{}: {observation:?}", archive.name));
            }
            state.save(state_path)?;
        }
        if !incomplete.is_empty() {
            let detail = match result {
                Ok(output) => format!("Cargo exited with {}", output.status),
                Err(error) => error.to_string(),
            };
            return Err(RailError::with_help(
                format!("release publication is incomplete: {}; {detail}", incomplete.join(", ")),
                "resume this transaction after registry evidence is available; attempted uploads are never repeated while their outcome is uncertain",
            ));
        }
        Ok(())
    }

    fn reconcile_forge_publications(
        &self,
        state: &mut ReleaseState,
        state_path: &Path,
        assets: &super::artifacts::Assets,
    ) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release() || state.intent.skip_tag {
            return Ok(());
        }
        self.reconcile_forge(state, state_path, assets, true)
    }

    /// Bump version in Cargo.toml
    fn bump_crate_version(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        use crate::release::version::BumpType;
        let bump = BumpType::Exact(plan.new_version.clone());
        VersionBumper::bump_version(&self.ctx.workspace_root().join(&plan.manifest_path), bump)?;
        Ok(())
    }

    /// Update dependent crates to use new version
    fn update_dependents(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        // Update [workspace.dependencies] in root Cargo.toml
        let root_manifest = self.ctx.workspace_root().join("Cargo.toml");
        VersionBumper::update_workspace_dependency(&root_manifest, &plan.name, &plan.new_version)?;

        // Update dependent crate manifests
        for dependent_name in &plan.affected_dependents {
            if let Some(pkg) = self.ctx.cargo().get_package(dependent_name) {
                let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
                VersionBumper::update_dependency_version(&manifest_path, &plan.name, &plan.new_version)?;
            }
        }

        Ok(())
    }

    fn update_lockfile(&self, plan: &ReleasePlan) -> RailResult<()> {
        let mut args = vec!["update"];
        for package in &plan.crates {
            args.extend(["--package", package.name.as_str()]);
        }
        let output = process::run("cargo", &args, Some(self.ctx.workspace_root()))?;
        if !output.status.success() {
            return Err(RailError::message(format!(
                "Cargo could not update the selected release packages: {}",
                String::from_utf8_lossy(&output.stderr),
            )));
        }
        Ok(())
    }

    fn write_auxiliary_lockfiles(&self, plan: &ReleasePlan) -> RailResult<()> {
        let workspace_root = canonicalize_existing(self.ctx.workspace_root())?;
        for projection in &plan.auxiliary_lockfiles {
            let path = self.ctx.workspace_root().join(&projection.lockfile_path);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                RailError::message(format!(
                    "failed to inspect planned auxiliary Cargo lockfile '{}': {error}",
                    projection.lockfile_path.display()
                ))
            })?;
            if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
                return Err(RailError::with_help(
                    format!(
                        "planned auxiliary Cargo lockfile '{}' is not a regular file",
                        projection.lockfile_path.display()
                    ),
                    "restore the committed lockfile, regenerate the release plan, and retry",
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| RailError::message("auxiliary Cargo lockfile has no parent directory"))?;
            let parent = canonicalize_existing(parent)?;
            if !parent.starts_with(&workspace_root) {
                return Err(RailError::with_help(
                    format!(
                        "auxiliary Cargo lockfile '{}' resolves outside workspace '{}'",
                        projection.lockfile_path.display(),
                        workspace_root.display()
                    ),
                    "keep release.auxiliary_cargo_manifests and their lockfiles inside the workspace",
                ));
            }

            let current = fs::read(&path)?;
            let current_digest = release_content_digest(&current);
            if current_digest == projection.after_digest {
                continue;
            }
            if current_digest != projection.before_digest {
                return Err(RailError::with_help(
                    format!(
                        "auxiliary Cargo lockfile '{}' drifted (planned {}, current {})",
                        projection.lockfile_path.display(),
                        projection.before_digest,
                        current_digest
                    ),
                    "restore the planned lockfile bytes or regenerate the release plan",
                ));
            }
            if release_content_digest(projection.content.as_bytes()) != projection.after_digest {
                return Err(RailError::with_help(
                    format!(
                        "release plan contains inconsistent post-release bytes for '{}'",
                        projection.lockfile_path.display()
                    ),
                    "discard the release state or plan file and regenerate it with this cargo-rail version",
                ));
            }
            crate::utils::write_file_atomic(&path, projection.content.as_bytes())?;
        }
        Ok(())
    }

    fn consume_change_files(&self, plan: &ReleasePlan) -> RailResult<()> {
        for path in &plan.change_files_to_delete {
            let path = self.ctx.workspace_root().join(path);
            if path.try_exists()? {
                fs::remove_file(&path).map_err(|e| {
                    RailError::message(format!("failed to remove change file {}: {}", path.display(), e))
                })?;
            }
        }
        for update in &plan.change_files_to_update {
            crate::utils::write_file_atomic(&self.ctx.workspace_root().join(&update.path), update.content.as_bytes())?;
        }
        Ok(())
    }

    /// Apply the exact insertion captured before release preparation.
    fn update_changelog(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let presentation = plan.presentation.as_ref().ok_or_else(|| {
            RailError::message("release has no captured presentation; resume through the supported journal reader")
        })?;
        if let Some(write) = &presentation.changelog {
            crate::release::presentation::apply_changelog(self.ctx.workspace_root(), &plan.changelog_path, write)?;
        }
        Ok(())
    }

    fn stage_planned_paths(&self, planned_paths: &[PathBuf], control_paths: &[PathBuf]) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let planned = normalize_release_paths(&git.worktree_root, planned_paths, "planned")?;
        let mut allowed = planned.clone();
        allowed.extend(normalize_release_paths(&git.worktree_root, control_paths, "control")?);
        let mut changed_paths = self.ctx.changed_source_paths()?;
        let git_changed_paths = git.changed_paths()?;
        changed_paths.extend(self.ctx.non_generated_source_paths(&git_changed_paths)?);
        changed_paths.sort();
        changed_paths.dedup();
        let unexpected = changed_paths
            .iter()
            .filter(|path| !allowed.contains(*path))
            .collect::<Vec<_>>();
        if !unexpected.is_empty() {
            return Err(RailError::with_help(
                format!(
                    "release produced unplanned worktree changes: {}",
                    unexpected
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "restore the unexpected paths, regenerate the release plan, and retry",
            ));
        }
        let to_stage = git_changed_paths
            .into_iter()
            .filter(|path| planned.contains(path))
            .collect::<Vec<_>>();
        git.stage_paths(&to_stage)
    }

    fn push_release_commit(
        &self,
        branch: &str,
        expected: &str,
        release_commit: &str,
        repository: &RemoteRepository,
    ) -> RailResult<()> {
        self.validate_expected_repository(repository)?;
        let git = self.ctx.git()?.git();
        let reference = format!("refs/heads/{branch}");
        git.run_git_observable_with_env(
            &[
                "push",
                "--atomic",
                &format!("--force-with-lease={reference}:{expected}"),
                RELEASE_REMOTE,
                &format!("{release_commit}:{reference}"),
            ],
            RELEASE_PUSH_ENV,
        )?;
        Ok(())
    }

    fn push_release_tags(&self, plan: &ReleasePlan, repository: &RemoteRepository) -> RailResult<()> {
        self.validate_expected_repository(repository)?;
        let git = self.ctx.git()?.git();
        let mut args = vec!["push".to_string(), "--atomic".to_string(), RELEASE_REMOTE.to_string()];
        for crate_plan in &plan.crates {
            args.push(format!("refs/tags/{}", crate_plan.tag_name));
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        git.run_git_observable_with_env(&borrowed, RELEASE_PUSH_ENV)?;
        Ok(())
    }

    fn create_gitlab_release(&self, repository: &RemoteRepository, plan: &CrateReleasePlan) -> RailResult<()> {
        let notes_file = self.write_release_notes_temp(plan)?;
        let args = gitlab_release_create_args(
            &plan.tag_name,
            &format!("{} v{}", plan.name, plan.new_version),
            notes_file
                .to_str()
                .ok_or_else(|| RailError::message("release notes path is not valid UTF-8"))?,
            &repository.selector(),
        );
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = process::run("glab", &borrowed, Some(self.ctx.workspace_root()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::message(format!(
                "glab release create failed for {}: {}",
                plan.tag_name,
                stderr.trim()
            )));
        }
        Ok(())
    }

    fn forge_release_exists(&self, forge: ReleaseForge, repository: &RemoteRepository, tag_name: &str) -> bool {
        let selector = repository.selector();
        match forge {
            ReleaseForge::Github => process::succeeds(
                "gh",
                &["release", "view", tag_name, "--repo", &selector],
                Some(self.ctx.workspace_root()),
            ),
            ReleaseForge::Gitlab => process::succeeds(
                "glab",
                &["release", "view", tag_name, "--repo", &selector],
                Some(self.ctx.workspace_root()),
            ),
        }
    }

    fn release_forge(&self, repository: &RemoteRepository) -> RailResult<ReleaseForge> {
        match self.release_config.remote_effects {
            ReleaseRemoteEffects::Github => return Ok(ReleaseForge::Github),
            ReleaseRemoteEffects::Gitlab => return Ok(ReleaseForge::Gitlab),
            ReleaseRemoteEffects::Auto | ReleaseRemoteEffects::Push => {}
            ReleaseRemoteEffects::None => return Err(RailError::message("local-only releases do not have a forge")),
        }
        match repository.host() {
            Some("github.com") => Ok(ReleaseForge::Github),
            Some("gitlab.com") => Ok(ReleaseForge::Gitlab),
            _ => Err(self.unsupported_readiness_error()),
        }
    }

    fn unsupported_readiness_error(&self) -> RailError {
        RailError::with_help(
            "origin does not expose a supported exact-SHA readiness provider",
            "use a GitHub or GitLab origin, or omit --publish and pass --skip-tag for a commit-only push",
        )
    }

    fn validate_forge_repository(&self, forge: ReleaseForge, repository: &RemoteRepository) -> RailResult<()> {
        if matches!(
            (forge, repository.host()),
            (ReleaseForge::Github, Some("gitlab.com")) | (ReleaseForge::Gitlab, Some("github.com"))
        ) {
            return Err(RailError::with_help(
                format!(
                    "release.remote_effects selects {}, but origin identifies '{}'",
                    forge.name(),
                    repository.selector()
                ),
                "make release.remote_effects agree with the exact origin repository provider",
            ));
        }
        if forge == ReleaseForge::Github && repository.github_owner_repo().is_none() {
            return Err(RailError::with_help(
                "the release repository is not an exact GitHub owner/repository identity",
                "configure origin with exactly one owner and repository path before releasing",
            ));
        }
        if forge == ReleaseForge::Gitlab && repository.host().is_some() && repository.path().split('/').count() < 2 {
            return Err(RailError::with_help(
                "the release repository is not an exact GitLab namespace/repository identity",
                "configure origin with a namespace and repository path before releasing",
            ));
        }
        Ok(())
    }

    fn github_auth_succeeds(&self, repository: &RemoteRepository) -> bool {
        let Some(host) = repository.host() else {
            return false;
        };
        process::succeeds(
            "gh",
            &["auth", "status", "--hostname", host],
            Some(self.ctx.workspace_root()),
        )
    }

    fn validate_remote_repository(&self, state: &ReleaseState) -> RailResult<()> {
        if !state.intent.release_config.remote_effects.pushes() {
            return Ok(());
        }
        let expected = state.intent.remote_repository.as_ref().ok_or_else(|| {
            RailError::with_help(
                "release journal predates exact remote repository binding",
                "recover the original cargo-rail version and journal; cargo-rail will not guess an irreversible remote target",
            )
        })?;
        self.validate_expected_repository(expected)
    }

    fn validate_expected_repository(&self, expected: &RemoteRepository) -> RailResult<()> {
        let actual = release_repository(self.ctx.workspace_root())?;
        if &actual != expected {
            return Err(RailError::with_help(
                format!(
                    "release repository changed from '{}' to '{}'",
                    expected.selector(),
                    actual.selector()
                ),
                "restore the exact origin fetch and push repository recorded when the release began",
            ));
        }
        Ok(())
    }

    fn local_tag_target(&self, tag_name: &str) -> RailResult<Option<String>> {
        let git = self.ctx.git()?.git();
        let tag_ref = format!("refs/tags/{}^{{commit}}", tag_name);
        if !git.run_git_check(&["rev-parse", "--verify", "--quiet", &tag_ref]) {
            return Ok(None);
        }
        git.run_git_stdout(&["rev-parse", "--verify", &tag_ref]).map(Some)
    }

    fn remote_commit_matches(&self, state: &ReleaseState, expected_head: &str) -> RailResult<bool> {
        let Some(remote_head) = self.remote_ref_target(&format!("refs/heads/{}", state.intent.branch))? else {
            return Ok(false);
        };
        Ok(remote_head == expected_head)
    }

    fn remote_tags_match(&self, state: &ReleaseState) -> RailResult<bool> {
        for (package, progress) in state.intent.plan.crates.iter().zip(&state.crates) {
            let Some(remote_tag) = self.remote_ref_target(&format!("refs/tags/{}", package.tag_name))? else {
                return Ok(false);
            };
            let expected = progress
                .tag_object
                .as_ref()
                .ok_or_else(|| RailError::message("release tag has no retained object"))?;
            if remote_tag != expected.id {
                return Err(RailError::message(format!(
                    "remote tag '{}' has object {}, expected {}",
                    package.tag_name, remote_tag, expected.id
                )));
            }
        }
        Ok(true)
    }

    fn remote_push_is_absent(&self, state: &ReleaseState) -> RailResult<bool> {
        let remote_head = self.remote_ref_target(&format!("refs/heads/{}", state.intent.branch))?;
        if remote_head.as_deref() != Some(state.intent.initial_head.as_str()) {
            return Ok(false);
        }
        if !state.intent.skip_tag {
            for crate_plan in &state.intent.plan.crates {
                if self
                    .remote_ref_target(&format!("refs/tags/{}", crate_plan.tag_name))?
                    .is_some()
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn remote_ref_target(&self, git_ref: &str) -> RailResult<Option<String>> {
        let output = self.ctx.git()?.git().run_git(&["ls-remote", RELEASE_REMOTE, git_ref])?;
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|_| RailError::message("origin returned a non-UTF-8 Git reference"))?;
        Ok(stdout.split_whitespace().next().map(str::to_string))
    }

    fn ensure_only_release_paths_changed(&self, state: &ReleaseState) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let mut allowed = normalize_release_paths(&git.worktree_root, &state.intent.planned_paths, "planned")?;
        allowed.extend(normalize_release_paths(
            &git.worktree_root,
            &state.intent.control_paths,
            "control",
        )?);
        let unexpected = self
            .ctx
            .changed_source_paths()?
            .into_iter()
            .filter(|path| !allowed.contains(path))
            .collect::<Vec<_>>();
        if unexpected.is_empty() {
            return Ok(());
        }
        Err(RailError::with_help(
            format!(
                "release recovery found unrelated changes: {}",
                unexpected
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "commit or restore unrelated work before resuming or aborting the release",
        ))
    }

    fn restore_local_input_backups(&self, state: &ReleaseState, include_consumed_inputs: bool) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        for backup in &state.intent.local_input_backups {
            if !include_consumed_inputs && !matches!(backup.restore, BackupRestorePolicy::Always) {
                continue;
            }
            let relative = normalize_release_path(&git.worktree_root, &backup.path, "backup")?;
            crate::utils::write_file_atomic(&git.worktree_root.join(relative), backup.content.as_bytes())?;
        }
        Ok(())
    }

    fn clean_untracked_planned_paths(&self, state: &ReleaseState) -> RailResult<()> {
        let git = crate::git::SystemGit::open(&self.ctx.git()?.git().worktree_root)?;
        for path in normalize_release_paths(&git.worktree_root, &state.intent.planned_paths, "planned")? {
            let Some(path) = path.to_str() else {
                return Err(RailError::message(format!(
                    "release path '{}' is not valid UTF-8",
                    path.display()
                )));
            };
            git.run_git(&["clean", "-f", "--", path])?;
        }
        Ok(())
    }

    fn remote_tag_exists(&self, tag_name: &str) -> RailResult<bool> {
        let output = self
            .ctx
            .git()?
            .git()
            .run_git(&["ls-remote", "--tags", RELEASE_REMOTE, tag_name])?;
        Ok(!output.stdout.is_empty())
    }

    fn validate_release_notes_size(
        &self,
        plan: &CrateReleasePlan,
        skip_tag: bool,
        repository: Option<&RemoteRepository>,
    ) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release()
            || skip_tag
            || self.release_forge(
                repository.ok_or_else(|| RailError::message("GitHub release notes have no repository identity"))?,
            )? != ReleaseForge::Github
        {
            return Ok(());
        }

        let notes = self.release_notes(plan)?;
        if notes.len() > GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES {
            return Err(RailError::with_help(
                format!(
                    "release notes for {} v{} are {} bytes, above the {} byte GitHub safety limit",
                    plan.name,
                    plan.new_version,
                    notes.len(),
                    GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES
                ),
                "reduce the generated changelog section before creating the GitHub release",
            ));
        }
        Ok(())
    }

    fn write_release_notes_temp(&self, plan: &CrateReleasePlan) -> RailResult<PathBuf> {
        let dir = crate::workspace::cargo_rail_state_root(self.ctx.workspace_root()).join("forge-release-bodies");
        fs::create_dir_all(&dir)
            .map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
        let path = dir.join(format!("{}.md", sanitize_filename(&plan.tag_name)));
        fs::write(&path, self.release_notes(plan)?)
            .map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
        Ok(path)
    }

    fn release_notes(&self, plan: &CrateReleasePlan) -> RailResult<String> {
        plan.presentation
            .as_ref()
            .map(|presentation| presentation.release_notes.clone())
            .ok_or_else(|| RailError::message("release has no captured forge body"))
    }
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn observe_gitlab_repository_readiness(
    workspace_root: &Path,
    repository: &RemoteRepository,
    release_commit: &str,
) -> RailResult<CheckReadiness> {
    let endpoint = format!(
        "projects/:id/pipelines?sha={}&per_page=1&order_by=id&sort=desc",
        release_commit
    );
    let selector = repository.selector();
    let output = process::run("glab", &["api", &endpoint, "--repo", &selector], Some(workspace_root))?;
    if !output.status.success() {
        return Err(RailError::with_help(
            format!(
                "failed to inspect GitLab pipelines for {}: {}",
                release_commit,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "restore GitLab API access, then resume; cargo-rail will not create tags without exact-SHA evidence",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| RailError::message(format!("invalid GitLab readiness JSON: {}", error)))?;
    Ok(gitlab_pipeline_readiness(&value, release_commit))
}

fn gitlab_pipeline_readiness(value: &serde_json::Value, release_commit: &str) -> CheckReadiness {
    let status = value
        .as_array()
        .and_then(|pipelines| pipelines.first())
        .and_then(|pipeline| pipeline.get("status"))
        .and_then(serde_json::Value::as_str);

    match status {
        Some("success") => CheckReadiness::Green(format!("gitlab:{}:success", release_commit)),
        Some(status @ ("failed" | "canceled")) => CheckReadiness::Failed(format!("GitLab pipeline is {}", status)),
        Some(status) => CheckReadiness::Waiting(format!("GitLab pipeline is {}", status)),
        None => CheckReadiness::Waiting("GitLab has not reported a pipeline for the release commit".to_string()),
    }
}

fn gitlab_release_create_args(tag: &str, title: &str, notes_file: &str, repository: &str) -> Vec<String> {
    vec![
        "release".to_string(),
        "create".to_string(),
        tag.to_string(),
        "--name".to_string(),
        title.to_string(),
        "--notes-file".to_string(),
        notes_file.to_string(),
        "--repo".to_string(),
        repository.to_string(),
    ]
}

fn release_content_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ContentDigest::sha256(bytes))
}

fn step_may_have_side_effect(step: &crate::release::state::Step) -> bool {
    step.status == StepStatus::InProgress || step.object.is_some()
}

fn differing_json_fields(left: &serde_json::Value, right: &serde_json::Value) -> Vec<String> {
    let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
        return vec!["release".to_string()];
    };
    left.keys()
        .chain(right.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| left.get(*key) != right.get(*key))
        .map(|key| format!("release.{}", key))
        .collect()
}

fn advance_phase(state: &mut ReleaseState, state_path: &std::path::Path, phase: ReleasePhase) -> RailResult<()> {
    if state.phase < phase {
        state.phase = phase;
        state.save(state_path)?;
    }
    Ok(())
}

fn readiness_wait_error(state_path: &std::path::Path, release_commit: &str, detail: &str) -> RailError {
    RailError::with_help(
        format!(
            "release commit {} is awaiting exact-SHA checks: {}",
            release_commit, detail
        ),
        format!(
            "stop here and resume after checks settle: cargo rail release resume {}",
            state_path.file_stem().unwrap_or_default().to_string_lossy()
        ),
    )
}

fn release_readiness_poll_interval() -> std::time::Duration {
    std::time::Duration::from_secs(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::presentation::{extract_section, insert_release_at};

    #[test]
    fn test_extract_changelog_section_returns_only_requested_version() {
        let changelog = r#"# Changelog

## [0.2.0] - 2026-06-01

### Features

- new API

## [0.1.0] - 2026-05-01

- old API
"#;

        let section = extract_section(changelog, "0.2.0").unwrap();
        assert!(section.contains("new API"));
        assert!(!section.contains("old API"));
    }

    #[test]
    fn changelog_release_follows_the_preamble() {
        let existing = "# Changelog\n\nThis file records user-visible changes.\n\n## [0.15.0] - 2026-06-01\n\n- old\n";
        let updated = insert_release_at(existing, "0.16.0", "2026-07-11", "- new");

        assert_eq!(
            updated,
            "# Changelog\n\nThis file records user-visible changes.\n\n## [0.16.0] - 2026-07-11\n- new\n## [0.15.0] - 2026-06-01\n\n- old\n"
        );
    }

    #[test]
    fn gitlab_readiness_accepts_only_a_successful_exact_sha_pipeline() {
        assert!(matches!(
          gitlab_pipeline_readiness(&serde_json::json!([{ "status": "success" }]), "abc123"),
          CheckReadiness::Green(detail) if detail == "gitlab:abc123:success"
        ));
        assert!(matches!(
          gitlab_pipeline_readiness(&serde_json::json!([{ "status": "running" }]), "abc123"),
          CheckReadiness::Waiting(detail) if detail == "GitLab pipeline is running"
        ));
        assert!(matches!(
          gitlab_pipeline_readiness(&serde_json::json!([{ "status": "canceled" }]), "abc123"),
          CheckReadiness::Failed(detail) if detail == "GitLab pipeline is canceled"
        ));
        assert!(matches!(
          gitlab_pipeline_readiness(&serde_json::json!([]), "abc123"),
          CheckReadiness::Waiting(detail) if detail.contains("has not reported")
        ));
    }

    #[test]
    fn gitlab_release_create_args_match_glab_cli() {
        assert_eq!(
            gitlab_release_create_args("v1.0.0", "crate v1.0.0", "/tmp/notes.md", "group/repo"),
            vec![
                "release",
                "create",
                "v1.0.0",
                "--name",
                "crate v1.0.0",
                "--notes-file",
                "/tmp/notes.md",
                "--repo",
                "group/repo"
            ]
        );
    }
}
