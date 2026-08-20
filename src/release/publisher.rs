//! Release execution and publishing to crates.io and forge releases.

use crate::config::{ReleaseConfig, ReleaseRemoteEffects};
use crate::error::{RailError, RailResult};
use crate::release::changelog::detect_github_repo;
use crate::release::planner::{CrateReleasePlan, ReleasePlan};
use crate::release::process;
use crate::release::state::{
    BackupRestorePolicy, ReconstructedRelease, ReleaseMode, ReleasePhase, ReleaseState, ReleaseStateCreate,
    ReleaseStatus, StepStatus, validate_state_path,
};
use crate::release::version::VersionBumper;
use crate::utils::canonicalize_existing;
use crate::workspace::WorkspaceContext;
use crate::{progress, warn};
use chrono::Local;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const GITHUB_RELEASE_NOTES_SOFT_LIMIT_BYTES: usize = 120_000;
const RELEASE_REMOTE: &str = "origin";
const RELEASE_OPERATION_ENV: &[(&str, &str)] = &[("CARGO_RAIL_OPERATION", "release")];
const RELEASE_PUSH_ENV: &[(&str, &str)] = &[("CARGO_RAIL_OPERATION", "release"), ("CARGO_RAIL_RELEASE_PUSH", "1")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseForge {
    Github,
    Gitlab,
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
        let mut warnings = Vec::new();
        let git = self.ctx.git()?.git();

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

        if self.release_config.remote_effects.creates_forge_release() && !skip_tag {
            let forge = self.detect_release_forge()?;
            let binary = forge.binary();
            if !process::succeeds(binary, &["--version"], None) {
                return Err(RailError::with_help(
                    format!("{} releases enabled but {} CLI was not found", forge.name(), binary),
                    format!("install {} or set release.remote_effects = \"push\"", binary),
                ));
            }

            if forge == ReleaseForge::Github
                && !process::succeeds("gh", &["auth", "status"], Some(self.ctx.workspace_root()))
            {
                return Err(RailError::with_help(
                    "GitHub CLI is not authenticated",
                    "run 'gh auth login' or provide GITHUB_TOKEN in CI",
                ));
            }

            for crate_plan in &plan.crates {
                if self.forge_release_exists(forge, &crate_plan.tag_name) {
                    warnings.push(format!(
                        "{} release '{}' already exists; cargo-rail will reuse it",
                        forge.name(),
                        crate_plan.tag_name
                    ));
                }
            }
        }

        if self.release_config.remote_effects.pushes() {
            if !git.has_remote(RELEASE_REMOTE)? {
                return Err(RailError::with_help(
                    "release push enabled but remote 'origin' does not exist",
                    "add an origin remote or set [release].remote_effects = \"none\"",
                ));
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
                let forge = self.detect_readiness_forge()?;
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
                if forge == ReleaseForge::Github
                    && !process::succeeds("gh", &["auth", "status"], Some(self.ctx.workspace_root()))
                {
                    return Err(RailError::with_help(
                        "GitHub CLI is not authenticated",
                        "run 'gh auth login' or provide GITHUB_TOKEN in CI",
                    ));
                }
            }
        }

        // Check sign_tags prerequisites if enabled
        if self.release_config.sign_tags && !skip_tag {
            // Check if user has GPG/SSH key configured
            if !git.has_signing_configured() {
                warnings.push(
                    "Tag signing enabled but no signing key configured. \
                    Run 'git config user.signingkey <KEY_ID>'"
                        .to_string(),
                );
            }
        }

        Ok(warnings)
    }

    /// Execute a release plan
    pub fn execute(
        &self,
        transaction_id: &str,
        plan: &ReleasePlan,
        skip_publish: bool,
        skip_tag: bool,
        planned_paths: &[PathBuf],
        control_paths: &[PathBuf],
    ) -> RailResult<()> {
        // Run pre-flight checks
        let warnings = self.preflight_check(plan, skip_publish, skip_tag)?;
        for warning in &warnings {
            warn!("{}", warning);
        }

        let git = self.ctx.git()?.git();
        let (mut state, state_path) = ReleaseState::create(ReleaseStateCreate {
            root: self.ctx.workspace_root(),
            transaction_id: transaction_id.to_string(),
            mode: ReleaseMode::Run,
            plan: plan.clone(),
            release_config: self.release_config.clone(),
            skip_publish,
            skip_tag,
            initial_head: git.head_commit()?,
            branch: git.current_branch()?,
            planned_paths: planned_paths.to_vec(),
            control_paths: control_paths.to_vec(),
            reconstructed: None,
        })?;
        progress!("release state: {}", state_path.display());
        if let Err(error) = self.execute_state(&mut state, &state_path) {
            return Err(error.context(format!(
                "release is recoverable from '{}'\nresume with: cargo rail release resume {}",
                state_path.display(),
                state_path.display()
            )));
        }

        Ok(())
    }

    /// Prepare a release pull request: mutations only, no tags or publish.
    pub fn execute_pr(
        &self,
        transaction_id: &str,
        plan: &ReleasePlan,
        planned_paths: &[PathBuf],
        control_paths: &[PathBuf],
    ) -> RailResult<()> {
        self.preflight_pr()?;
        let branch = release_branch_name(plan)?;
        let git = self.ctx.git()?.git();
        git.run_git_observable_with_env(&["checkout", "-B", &branch], RELEASE_OPERATION_ENV)?;

        let mut consumed_change_files = false;
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
            if !consumed_change_files {
                self.consume_change_files(plan)?;
                consumed_change_files = true;
            }
            self.update_lockfile_for_crate(&crate_plan.name)?;
        }

        self.stage_planned_paths(planned_paths, control_paths)?;
        let mut message = format!(
            "chore(release): prepare {}\n\nRail-Release: {}\nRail-Release-Mode: prepare\nRail-Release-Remote: {}",
            branch,
            transaction_id,
            self.release_config.remote_effects.as_str()
        );
        for crate_plan in &plan.crates {
            message.push_str(&format!(
                "\nRail-Release-Crate: {}@{}",
                crate_plan.name, crate_plan.new_version
            ));
            message.push_str(&format!(
                "\nRail-Release-Tag-Name: {}={}",
                crate_plan.name, crate_plan.tag_name
            ));
            message.push_str(&format!(
                "\nRail-Release-Crate-Publish: {}={}",
                crate_plan.name, crate_plan.publish
            ));
        }
        git.commit_with_env(&message, RELEASE_OPERATION_ENV)?;
        git.run_git_observable_with_env(&["push", "-u", RELEASE_REMOTE, &branch], RELEASE_PUSH_ENV)?;
        self.open_release_pr(plan, &branch)?;
        progress!("release PR ready: {}", branch);
        Ok(())
    }

    /// Finalize an already-merged release PR through checks, publication, tags, and forge releases.
    pub fn execute_finalize(
        &self,
        transaction_id: &str,
        plan: &ReleasePlan,
        skip_publish: bool,
        skip_tag: bool,
    ) -> RailResult<()> {
        let warnings = self.preflight_check(plan, skip_publish, skip_tag)?;
        for warning in &warnings {
            warn!("{}", warning);
        }

        let git = self.ctx.git()?.git();
        let (mut state, state_path) = ReleaseState::create(ReleaseStateCreate {
            root: self.ctx.workspace_root(),
            transaction_id: transaction_id.to_string(),
            mode: ReleaseMode::Finalize,
            plan: plan.clone(),
            release_config: self.release_config.clone(),
            skip_publish,
            skip_tag,
            initial_head: git.head_commit()?,
            branch: git.current_branch()?,
            planned_paths: Vec::new(),
            control_paths: Vec::new(),
            reconstructed: None,
        })?;
        progress!("release state: {}", state_path.display());
        self.execute_state(&mut state, &state_path)
    }

    /// Resume a previously interrupted release without replanning mutated inputs.
    pub fn resume(&self, state_path: &std::path::Path) -> RailResult<()> {
        let state_path = validate_state_path(self.ctx.workspace_root(), state_path)?;
        let mut state = ReleaseState::load(&state_path)?;
        state.validate_recovery_paths(&self.ctx.git()?.git().worktree_root)?;
        if state.status != ReleaseStatus::Active {
            return Err(RailError::message(format!(
                "release state is {:?}, not active",
                state.status
            )));
        }
        let persisted_config = serde_json::to_value(&state.release_config)?;
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
        if self.ctx.git()?.git().current_branch()? != state.branch {
            return Err(RailError::with_help(
                format!("release resume requires branch '{}'", state.branch),
                format!("git switch {}", state.branch),
            ));
        }
        if state.release_commit.is_some() {
            self.validate_release_head(&state)?;
        }
        progress!("resuming release state: {}", state_path.display());
        self.execute_state(&mut state, &state_path)
    }

    /// Rebuild a missing local journal from transaction trailers, then reconcile external truth.
    pub fn reconstruct(
        &self,
        transaction_id: &str,
        plan: &ReleasePlan,
        skip_publish: bool,
        skip_tag: bool,
        release_commit: String,
        commit_targets: std::collections::BTreeMap<String, String>,
    ) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let (mut state, state_path) = ReleaseState::create(ReleaseStateCreate {
            root: self.ctx.workspace_root(),
            transaction_id: transaction_id.to_string(),
            mode: ReleaseMode::Run,
            plan: plan.clone(),
            release_config: self.release_config.clone(),
            skip_publish,
            skip_tag,
            initial_head: release_commit.clone(),
            branch: git.current_branch()?,
            planned_paths: Vec::new(),
            control_paths: Vec::new(),
            reconstructed: Some(ReconstructedRelease {
                release_commit,
                commit_targets,
            }),
        })?;
        progress!("reconstructed release state: {}", state_path.display());
        self.execute_state(&mut state, &state_path)
    }

    /// Abort an active release while it is still entirely local.
    pub fn abort(&self, state_path: &std::path::Path) -> RailResult<()> {
        let state_path = validate_state_path(self.ctx.workspace_root(), state_path)?;
        let mut state = ReleaseState::load(&state_path)?;
        state.validate_recovery_paths(&self.ctx.git()?.git().worktree_root)?;
        if state.status != ReleaseStatus::Active {
            return Err(RailError::message(format!("release state is {:?}", state.status)));
        }
        let push_is_proven_absent =
            state.commit_push.status == StepStatus::InProgress && self.remote_push_is_absent(&state)?;
        let irreversible = (!push_is_proven_absent && step_may_have_side_effect(&state.commit_push))
            || step_may_have_side_effect(&state.tag_push)
            || state.crates.iter().any(|crate_state| {
                step_may_have_side_effect(&crate_state.forge_draft)
                    || step_may_have_side_effect(&crate_state.publication)
                    || step_may_have_side_effect(&crate_state.forge_publication)
            });
        if irreversible {
            return Err(RailError::with_help(
                "release abort refused because a remote or registry side effect may already exist",
                format!(
                    "resume with 'cargo rail release resume {}'; cargo-rail will reconcile the external state",
                    state_path.display()
                ),
            ));
        }

        let git = self.ctx.git()?.git();
        if git.current_branch()? != state.branch {
            return Err(RailError::with_help(
                format!("release abort requires branch '{}'", state.branch),
                format!("git switch {}", state.branch),
            ));
        }
        self.ensure_only_release_paths_changed(&state)?;
        state.abort.status = StepStatus::InProgress;
        state.abort.object = Some(state.initial_head.clone());
        state.save(&state_path, "abort_intent")?;
        fault_before("abort", &state.transaction_id)?;
        for (crate_plan, crate_state) in state.plan.crates.iter().zip(&state.crates) {
            if crate_state.tag.status != StepStatus::Pending && self.local_tag_target(&crate_plan.tag_name)?.is_some() {
                git.run_git(&["tag", "-d", "--", &crate_plan.tag_name])?;
            }
        }
        git.run_git(&["reset", "--hard", &state.initial_head])?;
        self.clean_untracked_planned_paths(&state)?;
        self.restore_local_input_backups(&state, true)?;
        fault_after("abort", &state.transaction_id)?;
        state.abort.status = StepStatus::Complete;
        state.status = ReleaseStatus::Aborted;
        state.save(&state_path, "aborted")?;
        progress!("release aborted and restored to {}", state.initial_head);
        Ok(())
    }

    fn execute_state(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        self.reconcile_local_commits(state, state_path)?;
        self.validate_release_head(state)?;
        advance_phase(state, state_path, ReleasePhase::Prepared)?;
        self.reconcile_commit_push(state, state_path)?;
        advance_phase(state, state_path, ReleasePhase::AwaitingChecks)?;
        self.reconcile_readiness(state, state_path)?;
        advance_phase(state, state_path, ReleasePhase::Ready)?;
        advance_phase(state, state_path, ReleasePhase::Publishing)?;
        self.reconcile_publications(state, state_path)?;
        self.reconcile_local_tags(state, state_path)?;
        self.reconcile_tag_push(state, state_path)?;
        self.reconcile_forge_drafts(state, state_path)?;
        self.reconcile_forge_publications(state, state_path)?;
        state.status = ReleaseStatus::Complete;
        state.phase = ReleasePhase::Released;
        state.save(state_path, "released")?;
        progress!("\nrelease complete");

        Ok(())
    }

    fn validate_release_head(&self, state: &ReleaseState) -> RailResult<()> {
        let expected = state
            .release_commit
            .as_deref()
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
                    expected, state.branch
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

    fn reconcile_local_commits(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if state.mode == ReleaseMode::Finalize {
            return self.reconcile_finalize_commit(state, state_path);
        }
        for crate_plan in state.plan.crates.clone() {
            let index = state.crate_index(&crate_plan.name)?;
            if !state.crates[index].commit.is_complete() {
                let expected_subject = format!("chore(release): {} v{}", crate_plan.name, crate_plan.new_version);
                if state.crates[index].commit.status == StepStatus::InProgress {
                    let expected_parent = state.crates[index]
                        .commit
                        .object
                        .as_deref()
                        .ok_or_else(|| RailError::message("in-progress release commit has no recorded parent"))?;
                    let head = self.ctx.git()?.git().head_commit()?;
                    let subject = self.ctx.git()?.git().run_git_stdout(&["log", "-1", "--format=%s"])?;
                    let parent = self
                        .ctx
                        .git()?
                        .git()
                        .run_git_stdout(&["rev-parse", "HEAD^"])
                        .unwrap_or_default();
                    if head != expected_parent && parent == expected_parent && subject == expected_subject {
                        state.crates[index].commit.status = StepStatus::Complete;
                        state.crates[index].commit.object = Some(head);
                        state.save(state_path, &format!("commit_observed:{}", crate_plan.name))?;
                    } else {
                        self.restore_interrupted_local_step(state)?;
                    }
                }

                if !state.crates[index].commit.is_complete() {
                    state.crates[index].commit.status = StepStatus::InProgress;
                    state.crates[index].commit.object = Some(self.ctx.git()?.git().head_commit()?);
                    state.save(state_path, &format!("commit_intent:{}", crate_plan.name))?;
                    progress!(
                        "  version: {} -> {}",
                        crate_plan.current_version,
                        crate_plan.new_version
                    );
                    let local_result = (|| {
                        self.bump_crate_version(&crate_plan)?;
                        if !crate_plan.affected_dependents.is_empty() {
                            self.update_dependents(&crate_plan)?;
                        }
                        self.update_changelog(&crate_plan)?;
                        self.validate_release_notes_size(&crate_plan, state.skip_tag)?;
                        if !state.crates.iter().any(|crate_state| crate_state.commit.is_complete()) {
                            self.consume_change_files(&state.plan)?;
                        }
                        fault_before("commit", &crate_plan.name)?;
                        self.commit_version_bump(
                            &state.transaction_id,
                            state.skip_publish,
                            state.skip_tag,
                            &crate_plan,
                            &state.planned_paths,
                            &state.control_paths,
                        )
                    })();
                    if let Err(error) = local_result {
                        self.restore_interrupted_local_step(state)?;
                        return Err(error);
                    }
                    let commit = self.ctx.git()?.git().head_commit()?;
                    fault_after("commit", &crate_plan.name)?;
                    state.crates[index].commit.status = StepStatus::Complete;
                    state.crates[index].commit.object = Some(commit);
                    state.save(state_path, &format!("commit_observed:{}", crate_plan.name))?;
                }
            }
        }
        state.release_commit = state
            .crates
            .last()
            .and_then(|crate_state| crate_state.commit.object.clone())
            .or_else(|| Some(state.initial_head.clone()));
        state.save(state_path, "release_commit_observed")
    }

    fn reconcile_finalize_commit(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let expected_subject = format!("chore(release): finalize {}", state.transaction_id);
        let head = git.head_commit()?;
        let message = git.run_git_stdout(&["log", "-1", "--format=%B"])?;
        let trailer = format!("Rail-Release: {}", state.transaction_id);
        let has_transaction = message.lines().any(|line| line.trim() == trailer);
        let is_finalize = message.lines().any(|line| line.trim() == "Rail-Release-Mode: finalize");
        if has_transaction && is_finalize {
            for crate_state in &mut state.crates {
                crate_state.commit.status = StepStatus::Complete;
                crate_state.commit.object = Some(head.clone());
            }
            state.release_commit = Some(head);
            state.save(state_path, "finalize_commit_observed")?;
            return Ok(());
        }

        let first = state
            .crates
            .first()
            .ok_or_else(|| RailError::message("finalize release plan has no crates"))?;
        if first.commit.status == StepStatus::InProgress {
            let expected_parent = first
                .commit
                .object
                .as_deref()
                .ok_or_else(|| RailError::message("in-progress finalize commit has no recorded parent"))?;
            if head != expected_parent {
                let parent = git.run_git_stdout(&["rev-parse", "HEAD^"]).unwrap_or_default();
                let subject = git.run_git_stdout(&["log", "-1", "--format=%s"])?;
                if parent != expected_parent || subject != expected_subject {
                    return Err(RailError::with_help(
                        "HEAD changed while the finalize transaction commit was in progress",
                        "inspect the release status and Git history; do not create tags for an ambiguous release commit",
                    ));
                }
            }
        }

        if !state.crates.iter().all(|crate_state| crate_state.commit.is_complete()) {
            let parent = git.head_commit()?;
            for crate_state in &mut state.crates {
                crate_state.commit.status = StepStatus::InProgress;
                crate_state.commit.object = Some(parent.clone());
            }
            state.save(state_path, "finalize_commit_intent")?;
            fault_before("commit", "finalize")?;
            let mut message = format!(
                "{}\n\n{}\nRail-Release-Mode: finalize\nRail-Release-Publish: {}\nRail-Release-Tag: {}\nRail-Release-Remote: {}",
                expected_subject,
                trailer,
                !state.skip_publish,
                !state.skip_tag,
                self.release_config.remote_effects.as_str()
            );
            for crate_plan in &state.plan.crates {
                message.push_str(&format!(
                    "\nRail-Release-Crate: {}@{}",
                    crate_plan.name, crate_plan.new_version
                ));
                message.push_str(&format!(
                    "\nRail-Release-Tag-Name: {}={}",
                    crate_plan.name, crate_plan.tag_name
                ));
                message.push_str(&format!(
                    "\nRail-Release-Crate-Publish: {}={}",
                    crate_plan.name,
                    !state.skip_publish && crate_plan.publish
                ));
            }
            git.run_git_observable_with_env(&["commit", "--allow-empty", "-m", &message], RELEASE_OPERATION_ENV)?;
            let commit = git.head_commit()?;
            fault_after("commit", "finalize")?;
            for crate_state in &mut state.crates {
                crate_state.commit.status = StepStatus::Complete;
                crate_state.commit.object = Some(commit.clone());
            }
            state.release_commit = Some(commit);
            state.save(state_path, "finalize_commit_observed")?;
        }
        Ok(())
    }

    fn reconcile_local_tags(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        for crate_plan in state.plan.crates.clone() {
            let index = state.crate_index(&crate_plan.name)?;
            if !state.crates[index].tag.is_complete() {
                let expected =
                    state.crates[index].commit.object.clone().ok_or_else(|| {
                        RailError::message(format!("release commit missing for '{}'", crate_plan.name))
                    })?;
                if let Some(existing) = self.local_tag_target(&crate_plan.tag_name)? {
                    if existing != expected {
                        return Err(RailError::message(format!(
                            "tag '{}' points to {}, expected {}",
                            crate_plan.tag_name, existing, expected
                        )));
                    }
                    state.crates[index].tag.status = StepStatus::Complete;
                    state.crates[index].tag.object = Some(existing);
                    state.save(state_path, &format!("tag_observed:{}", crate_plan.tag_name))?;
                } else {
                    state.crates[index].tag.status = StepStatus::InProgress;
                    state.crates[index].tag.object = Some(expected.clone());
                    state.save(state_path, &format!("tag_intent:{}", crate_plan.tag_name))?;
                    fault_before("tag", &crate_plan.tag_name)?;
                    self.create_tag(&crate_plan)?;
                    fault_after("tag", &crate_plan.tag_name)?;
                    state.crates[index].tag.status = StepStatus::Complete;
                    state.crates[index].tag.object = Some(expected);
                    state.save(state_path, &format!("tag_observed:{}", crate_plan.tag_name))?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_commit_push(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        let release_commit = state
            .release_commit
            .clone()
            .ok_or_else(|| RailError::message("prepared release has no exact release commit"))?;
        if !self.release_config.remote_effects.pushes() {
            state.commit_push.status = StepStatus::Complete;
            state.commit_push.object = Some(release_commit);
            state.save(state_path, "commit_push_not_authorized")?;
            return Ok(());
        }
        if state.commit_push.is_complete() {
            return Ok(());
        }
        if self.remote_commit_matches(state, &release_commit)? {
            state.commit_push.status = StepStatus::Complete;
            state.commit_push.object = Some(release_commit);
            state.save(state_path, "commit_push_observed")?;
            return Ok(());
        }
        state.commit_push.status = StepStatus::InProgress;
        state.commit_push.object = Some(release_commit.clone());
        state.save(state_path, "commit_push_intent")?;
        self.validate_release_head(state)?;
        fault_before("push", RELEASE_REMOTE)?;
        self.push_release_commit(&state.branch)?;
        fault_after("push", RELEASE_REMOTE)?;
        if !self.remote_commit_matches(state, &release_commit)? {
            return Err(RailError::message(format!(
                "release commit {} is not observable at origin/{} after push",
                release_commit, state.branch
            )));
        }
        state.commit_push.status = StepStatus::Complete;
        state.commit_push.object = Some(release_commit);
        state.save(state_path, "commit_push_observed")
    }

    fn reconcile_readiness(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if state.readiness.is_complete() {
            return Ok(());
        }
        let release_commit = state
            .release_commit
            .as_deref()
            .ok_or_else(|| RailError::message("prepared release has no exact release commit"))?;
        if !self.release_config.remote_effects.pushes() || state.skip_tag && state.skip_publish {
            state.readiness.status = StepStatus::Complete;
            state.readiness.object = Some(format!("not_required:{}", release_commit));
            state.save(state_path, "readiness_not_required")?;
            return Ok(());
        }

        let observation = self.observe_exact_sha_readiness(release_commit)?;
        match observation {
            CheckReadiness::Green(detail) => {
                state.readiness.status = StepStatus::Complete;
                state.readiness.object = Some(detail);
                state.save(state_path, "readiness_observed")
            }
            CheckReadiness::Waiting(detail) => {
                state.readiness.object = Some(detail.clone());
                state.save(state_path, "readiness_waiting")?;
                Err(readiness_wait_error(state_path, release_commit, &detail))
            }
            CheckReadiness::Failed(detail) => {
                state.readiness.object = Some(detail.clone());
                state.save(state_path, "readiness_failed")?;
                Err(RailError::with_help(
                    format!("release checks failed for exact commit {}: {}", release_commit, detail),
                    "fix the failing checks without moving or replacing the release commit; then resume the release",
                ))
            }
        }
    }

    fn reconcile_tag_push(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if !self.release_config.remote_effects.pushes() || state.skip_tag {
            state.tag_push.status = StepStatus::Complete;
            state.tag_push.object = state.release_commit.clone();
            state.save(state_path, "tag_push_not_required")?;
            return Ok(());
        }
        if state.tag_push.is_complete() {
            return Ok(());
        }
        if self.remote_tags_match(state)? {
            state.tag_push.status = StepStatus::Complete;
            state.tag_push.object = state.release_commit.clone();
            state.save(state_path, "tag_push_observed")?;
            return Ok(());
        }
        state.tag_push.status = StepStatus::InProgress;
        state.tag_push.object = state.release_commit.clone();
        state.save(state_path, "tag_push_intent")?;
        fault_before("tag_push", RELEASE_REMOTE)?;
        self.push_release_tags(&state.plan)?;
        fault_after("tag_push", RELEASE_REMOTE)?;
        if !self.remote_tags_match(state)? {
            return Err(RailError::message(
                "release tags are not observable on origin after push",
            ));
        }
        state.tag_push.status = StepStatus::Complete;
        state.tag_push.object = state.release_commit.clone();
        state.save(state_path, "tag_push_observed")
    }

    fn reconcile_forge_drafts(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release() || state.skip_tag {
            for crate_state in &mut state.crates {
                crate_state.forge_draft.status = StepStatus::Complete;
                crate_state.forge_publication.status = StepStatus::Complete;
            }
            state.save(state_path, "forge_not_required")?;
            return Ok(());
        }
        let forge = self.detect_release_forge()?;
        for crate_plan in state.plan.crates.clone() {
            let index = state.crate_index(&crate_plan.name)?;
            if state.crates[index].forge_draft.is_complete() {
                continue;
            }
            if self.existing_forge_release_matches(forge, &crate_plan)? {
                state.crates[index].forge_draft.status = StepStatus::Complete;
                state.crates[index].forge_draft.object = Some(crate_plan.tag_name.clone());
                state.save(state_path, &format!("forge_observed:{}", crate_plan.tag_name))?;
                continue;
            }
            state.crates[index].forge_draft.status = StepStatus::InProgress;
            state.crates[index].forge_draft.object = Some(crate_plan.tag_name.clone());
            state.save(state_path, &format!("forge_intent:{}", crate_plan.tag_name))?;
            fault_before("forge_draft", &crate_plan.tag_name)?;
            self.create_forge_release(forge, &crate_plan)?;
            fault_after("forge_draft", &crate_plan.tag_name)?;
            state.crates[index].forge_draft.status = StepStatus::Complete;
            state.crates[index].forge_draft.object = Some(crate_plan.tag_name.clone());
            if forge == ReleaseForge::Gitlab {
                state.crates[index].forge_publication.status = StepStatus::Complete;
            }
            state.save(state_path, &format!("forge_observed:{}", crate_plan.tag_name))?;
        }
        Ok(())
    }

    fn reconcile_publications(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        for crate_plan in state.plan.crates.clone() {
            let index = state.crate_index(&crate_plan.name)?;
            if state.crates[index].publication.is_complete() {
                if !crate_plan.publish {
                    progress!("  skipped publish (publish = false) for {}", crate_plan.name);
                }
                continue;
            }
            if self.registry_version_exists(&crate_plan) {
                state.crates[index].publication.status = StepStatus::Complete;
                state.crates[index].publication.object = Some(crate_plan.new_version.to_string());
                state.save(state_path, &format!("publish_observed:{}", crate_plan.name))?;
                continue;
            }
            if state.crates[index].publication.status == StepStatus::Pending {
                state.crates[index].publication.status = StepStatus::InProgress;
                state.crates[index].publication.object = Some(crate_plan.new_version.to_string());
                state.save(state_path, &format!("publish_intent:{}", crate_plan.name))?;
            }
            progress!("  publishing {}...", crate_plan.name);
            self.validate_publish_checkout(state)?;
            fault_before("publish", &crate_plan.name)?;
            let publish = self.publish_crate(&crate_plan);
            fault_after("publish", &crate_plan.name)?;
            let observable = self.registry_version_exists(&crate_plan);
            if let Err(error) = publish
                && !observable
            {
                return Err(error.context(format!(
                    "{} v{} remains unobservable on crates.io; resume to reconcile and retry the immutable version",
                    crate_plan.name, crate_plan.new_version
                )));
            }
            if !observable {
                return Err(registry_wait_error(&crate_plan));
            }
            state.crates[index].publication.status = StepStatus::Complete;
            state.crates[index].publication.object = Some(crate_plan.new_version.to_string());
            state.save(state_path, &format!("publish_observed:{}", crate_plan.name))?;
        }
        Ok(())
    }

    fn reconcile_forge_publications(&self, state: &mut ReleaseState, state_path: &std::path::Path) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release() || state.skip_tag {
            return Ok(());
        }
        let forge = self.detect_release_forge()?;
        for crate_plan in state.plan.crates.clone() {
            let index = state.crate_index(&crate_plan.name)?;
            if state.crates[index].forge_publication.is_complete() {
                continue;
            }
            if forge == ReleaseForge::Github && self.github_release_is_published(&crate_plan.tag_name)? {
                state.crates[index].forge_publication.status = StepStatus::Complete;
                state.crates[index].forge_publication.object = Some(crate_plan.tag_name.clone());
                state.save(state_path, &format!("forge_publish_observed:{}", crate_plan.tag_name))?;
                continue;
            }
            state.crates[index].forge_publication.status = StepStatus::InProgress;
            state.crates[index].forge_publication.object = Some(crate_plan.tag_name.clone());
            state.save(state_path, &format!("forge_publish_intent:{}", crate_plan.tag_name))?;
            fault_before("forge_publish", &crate_plan.tag_name)?;
            self.publish_forge_release(forge, &crate_plan)?;
            fault_after("forge_publish", &crate_plan.tag_name)?;
            state.crates[index].forge_publication.status = StepStatus::Complete;
            state.crates[index].forge_publication.object = Some(crate_plan.tag_name.clone());
            state.save(state_path, &format!("forge_publish_observed:{}", crate_plan.tag_name))?;
        }
        Ok(())
    }

    fn preflight_pr(&self) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        if !git.has_remote(RELEASE_REMOTE)? {
            return Err(RailError::with_help(
                "release PR mode requires remote 'origin'",
                "add an origin remote before running 'cargo rail release run --pr'",
            ));
        }
        if !process::succeeds("gh", &["--version"], None) {
            return Err(RailError::with_help(
                "release PR mode requires gh CLI",
                "install gh from https://cli.github.com/ or run the release without --pr",
            ));
        }
        Ok(())
    }

    fn open_release_pr(&self, plan: &ReleasePlan, branch: &str) -> RailResult<()> {
        let body_path = self.write_release_pr_body(plan, branch)?;
        let output = process::run(
            "gh",
            &[
                "pr",
                "create",
                "--title",
                &format!("Release {}", branch.trim_start_matches("rail/release-")),
                "--body-file",
                body_path
                    .to_str()
                    .ok_or_else(|| RailError::message("release PR body path is not valid UTF-8"))?,
                "--head",
                branch,
            ],
            Some(self.ctx.workspace_root()),
        )?;
        if !output.status.success() {
            return Err(RailError::message(format!(
                "gh pr create failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    fn write_release_pr_body(&self, plan: &ReleasePlan, branch: &str) -> RailResult<PathBuf> {
        let dir = crate::workspace::cargo_rail_state_root(self.ctx.workspace_root()).join("release-pr");
        fs::create_dir_all(&dir)
            .map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
        let path = dir.join(format!("{}.md", sanitize_filename(branch)));
        fs::write(&path, release_pr_body(plan))
            .map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
        Ok(path)
    }

    /// Bump version in Cargo.toml
    fn bump_crate_version(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        use crate::release::version::BumpType;
        let bump = BumpType::Exact(plan.new_version.clone());
        VersionBumper::bump_version(&plan.manifest_path, bump)?;
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

    /// Update Cargo.lock for a specific crate only
    ///
    /// Uses targeted `cargo update --package` to avoid upgrading external dependencies.
    /// This is safer than `cargo update --workspace` which can inadvertently upgrade
    /// pinned external dependencies during a release.
    fn update_lockfile_for_crate(&self, crate_name: &str) -> RailResult<()> {
        let output = process::run(
            "cargo",
            &["update", "--package", crate_name],
            Some(self.ctx.workspace_root()),
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::message(format!(
                "cargo update --package {} failed: {}",
                crate_name, stderr
            )));
        }

        Ok(())
    }

    fn consume_change_files(&self, plan: &ReleasePlan) -> RailResult<()> {
        for path in &plan.change_files_to_delete {
            if path.exists() {
                fs::remove_file(path).map_err(|e| {
                    RailError::message(format!("failed to remove change file {}: {}", path.display(), e))
                })?;
            }
        }
        for update in &plan.change_files_to_update {
            crate::utils::write_file_atomic(&update.path, update.content.as_bytes())?;
        }
        Ok(())
    }

    /// Update or create CHANGELOG.md
    fn update_changelog(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        if !plan.generate_changelog {
            return Ok(());
        }

        let github_repo = detect_github_repo(self.ctx.workspace_root());
        let new_entries = plan.changelog_body.as_str();

        // Read existing changelog or create new
        let existing = if plan.changelog_path.exists() {
            fs::read_to_string(&plan.changelog_path).unwrap_or_default()
        } else {
            format!(
                "# Changelog\n\nAll notable changes to {} will be documented in this file.\n\n",
                plan.name
            )
        };

        // Build the new release section.
        let date = self.get_current_date();
        let mut release = self.format_version_header(plan, plan.previous_tag.as_deref(), &date, github_repo.as_ref());
        release.push_str(new_entries);
        release.push('\n');

        if new_entries.trim().is_empty() {
            if self.release_config.require_changelog_entries {
                return Err(RailError::message(format!(
                    "no changelog entries for {} (enable commits or disable changelog)",
                    plan.name
                )));
            }
            return Ok(());
        }

        let updated = insert_changelog_release(&existing, &release);

        // Auto-create parent directories if they don't exist
        if let Some(parent) = plan.changelog_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)
                .map_err(|e| RailError::message(format!("failed to create directory {}: {}", parent.display(), e)))?;
        }

        fs::write(&plan.changelog_path, updated)
            .map_err(|e| RailError::message(format!("failed to write {}: {}", plan.changelog_path.display(), e)))?;

        Ok(())
    }

    /// Commit version bump and changelog
    fn commit_version_bump(
        &self,
        transaction_id: &str,
        skip_publish: bool,
        skip_tag: bool,
        plan: &CrateReleasePlan,
        planned_paths: &[PathBuf],
        control_paths: &[PathBuf],
    ) -> RailResult<()> {
        let message = format!(
            "chore(release): {} v{}\n\nRail-Release: {}\nRail-Release-Mode: run\nRail-Release-Publish: {}\nRail-Release-Tag: {}\nRail-Release-Remote: {}\nRail-Release-Crate: {}@{}\nRail-Release-Tag-Name: {}={}\nRail-Release-Crate-Publish: {}={}",
            plan.name,
            plan.new_version,
            transaction_id,
            !skip_publish,
            !skip_tag,
            self.release_config.remote_effects.as_str(),
            plan.name,
            plan.new_version,
            plan.name,
            plan.tag_name,
            plan.name,
            !skip_publish && plan.publish
        );

        // Update Cargo.lock to reflect the new version
        // Use targeted update to only update this crate, not external dependencies
        self.update_lockfile_for_crate(&plan.name)?;

        // Refuse any mutation outside the approved path set, then stage only that set.
        self.stage_planned_paths(planned_paths, control_paths)?;
        self.ctx.git()?.git().commit_with_env(&message, RELEASE_OPERATION_ENV)?;

        Ok(())
    }

    fn stage_planned_paths(&self, planned_paths: &[PathBuf], control_paths: &[PathBuf]) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let canonical_git_root = canonicalize_existing(&git.worktree_root)?;
        let planned: BTreeSet<PathBuf> = planned_paths.iter().cloned().collect();
        let mut allowed = planned.clone();
        for path in control_paths {
            let relative = if path.is_absolute() {
                let canonical = canonicalize_existing(path)?;
                let Ok(relative) = canonical.strip_prefix(&canonical_git_root) else {
                    continue;
                };
                relative.to_path_buf()
            } else {
                path.clone()
            };
            allowed.insert(relative);
        }
        let changed_paths = self.ctx.changed_source_paths()?;
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
        let to_stage = changed_paths
            .into_iter()
            .filter(|path| planned.contains(path))
            .collect::<Vec<_>>();
        git.stage_paths(&to_stage)
    }

    /// Create git tag
    fn create_tag(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let message = format!("Release {} v{}", plan.name, plan.new_version);
        self.ctx
            .git()?
            .git()
            .create_tag(&plan.tag_name, Some(&message), self.release_config.sign_tags)
    }

    fn push_release_commit(&self, branch: &str) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let head_refspec = format!("HEAD:{}", branch);
        git.run_git_observable_with_env(&["push", "--atomic", RELEASE_REMOTE, &head_refspec], RELEASE_PUSH_ENV)?;
        Ok(())
    }

    fn push_release_tags(&self, plan: &ReleasePlan) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let mut args = vec!["push".to_string(), "--atomic".to_string(), RELEASE_REMOTE.to_string()];
        for crate_plan in &plan.crates {
            args.push(format!("refs/tags/{}", crate_plan.tag_name));
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        git.run_git_observable_with_env(&borrowed, RELEASE_PUSH_ENV)?;
        Ok(())
    }

    /// Publish crate to crates.io
    fn publish_crate(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let output = process::run(
            "cargo",
            &["publish", "-p", &plan.name, "--locked", "--registry", "crates-io"],
            Some(self.ctx.workspace_root()),
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::message(format!(
                "cargo publish failed for {}: {}",
                plan.name, stderr
            )));
        }

        Ok(())
    }

    fn create_forge_release(&self, forge: ReleaseForge, plan: &CrateReleasePlan) -> RailResult<()> {
        if self.forge_release_exists(forge, &plan.tag_name) {
            progress!(
                "  {} release already exists: {}",
                forge.name().to_lowercase(),
                plan.tag_name
            );
            return Ok(());
        }
        match forge {
            ReleaseForge::Github => self.create_github_release_draft(plan),
            ReleaseForge::Gitlab => self.create_gitlab_release(plan),
        }
    }

    /// Create a draft GitHub release targeting the exact pushed commit.
    fn create_github_release_draft(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let target = self.tag_target_commit(&plan.tag_name)?;
        let notes_file = self.write_release_notes_temp(plan)?;
        let output = process::run(
            "gh",
            &[
                "release",
                "create",
                &plan.tag_name,
                "--target",
                &target,
                "--title",
                &format!("{} v{}", plan.name, plan.new_version),
                "--notes-file",
                notes_file
                    .to_str()
                    .ok_or_else(|| RailError::message("release notes path is not valid UTF-8"))?,
                "--draft",
            ],
            Some(self.ctx.workspace_root()),
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::message(format!(
                "gh release create failed for {}: {}",
                plan.tag_name,
                stderr.trim()
            )));
        }

        Ok(())
    }

    fn create_gitlab_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let notes_file = self.write_release_notes_temp(plan)?;
        let args = gitlab_release_create_args(
            &plan.tag_name,
            &format!("{} v{}", plan.name, plan.new_version),
            notes_file
                .to_str()
                .ok_or_else(|| RailError::message("release notes path is not valid UTF-8"))?,
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

    fn publish_forge_release(&self, forge: ReleaseForge, plan: &CrateReleasePlan) -> RailResult<()> {
        match forge {
            ReleaseForge::Github => self.publish_github_release(plan),
            ReleaseForge::Gitlab => Ok(()),
        }
    }

    fn publish_github_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
        let output = process::run(
            "gh",
            &["release", "edit", &plan.tag_name, "--draft=false", "--latest"],
            Some(self.ctx.workspace_root()),
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RailError::message(format!(
                "gh release edit failed for {}: {}",
                plan.tag_name,
                stderr.trim()
            )));
        }

        Ok(())
    }

    fn forge_release_exists(&self, forge: ReleaseForge, tag_name: &str) -> bool {
        match forge {
            ReleaseForge::Github => {
                process::succeeds("gh", &["release", "view", tag_name], Some(self.ctx.workspace_root()))
            }
            ReleaseForge::Gitlab => {
                process::succeeds("glab", &["release", "view", tag_name], Some(self.ctx.workspace_root()))
            }
        }
    }

    fn detect_release_forge(&self) -> RailResult<ReleaseForge> {
        match self.release_config.remote_effects {
            ReleaseRemoteEffects::Github => return Ok(ReleaseForge::Github),
            ReleaseRemoteEffects::Gitlab => return Ok(ReleaseForge::Gitlab),
            ReleaseRemoteEffects::Auto => {}
            ReleaseRemoteEffects::None | ReleaseRemoteEffects::Push => {
                return Err(RailError::message(
                    "forge release creation is not authorized by release.remote_effects",
                ));
            }
        }

        let output = process::run(
            "git",
            &["config", "--get", "remote.origin.url"],
            Some(self.ctx.workspace_root()),
        )?;
        let remote = String::from_utf8_lossy(&output.stdout);
        detect_release_forge_from_remote(remote.trim()).ok_or_else(|| {
            RailError::with_help(
                "could not detect release forge from origin remote",
                "set [release].remote_effects = \"github\" or \"gitlab\"; Gitea release creation is not supported",
            )
        })
    }

    fn detect_readiness_forge(&self) -> RailResult<ReleaseForge> {
        match self.release_config.remote_effects {
            ReleaseRemoteEffects::Github => return Ok(ReleaseForge::Github),
            ReleaseRemoteEffects::Gitlab => return Ok(ReleaseForge::Gitlab),
            ReleaseRemoteEffects::None => {
                return Err(RailError::message("local-only releases do not have remote readiness"));
            }
            ReleaseRemoteEffects::Auto | ReleaseRemoteEffects::Push => {}
        }

        let output = process::run(
            "git",
            &["config", "--get", "remote.origin.url"],
            Some(self.ctx.workspace_root()),
        )?;
        let remote = String::from_utf8_lossy(&output.stdout);
        detect_release_forge_from_remote(remote.trim()).ok_or_else(|| {
            RailError::with_help(
                "origin does not expose a supported exact-SHA readiness provider",
                "use a GitHub or GitLab origin, or pass both --skip-publish and --skip-tag for a commit-only push",
            )
        })
    }

    fn observe_exact_sha_readiness(&self, release_commit: &str) -> RailResult<CheckReadiness> {
        match self.detect_readiness_forge()? {
            ReleaseForge::Github => self.observe_github_readiness(release_commit),
            ReleaseForge::Gitlab => self.observe_gitlab_readiness(release_commit),
        }
    }

    fn observe_github_readiness(&self, release_commit: &str) -> RailResult<CheckReadiness> {
        observe_github_exact_sha_readiness(self.ctx.workspace_root(), release_commit)
    }

    fn observe_gitlab_readiness(&self, release_commit: &str) -> RailResult<CheckReadiness> {
        observe_gitlab_exact_sha_readiness(self.ctx.workspace_root(), release_commit)
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
        let Some(remote_head) = self.remote_ref_target(&format!("refs/heads/{}", state.branch))? else {
            return Ok(false);
        };
        Ok(remote_head == expected_head)
    }

    fn remote_tags_match(&self, state: &ReleaseState) -> RailResult<bool> {
        for crate_plan in &state.plan.crates {
            let Some(remote_tag) = self.remote_ref_target(&format!("refs/tags/{}^{{}}", crate_plan.tag_name))? else {
                return Ok(false);
            };
            let expected = self
                .local_tag_target(&crate_plan.tag_name)?
                .ok_or_else(|| RailError::message(format!("local tag '{}' is missing", crate_plan.tag_name)))?;
            if remote_tag != expected {
                return Err(RailError::message(format!(
                    "remote tag '{}' points to {}, expected {}",
                    crate_plan.tag_name, remote_tag, expected
                )));
            }
        }
        Ok(true)
    }

    fn remote_push_is_absent(&self, state: &ReleaseState) -> RailResult<bool> {
        let remote_head = self.remote_ref_target(&format!("refs/heads/{}", state.branch))?;
        if remote_head.as_deref() != Some(state.initial_head.as_str()) {
            return Ok(false);
        }
        if !state.skip_tag {
            for crate_plan in &state.plan.crates {
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
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string))
    }

    fn registry_version_exists(&self, plan: &CrateReleasePlan) -> bool {
        let spec = format!("{}@{}", plan.name, plan.new_version);
        process::succeeds(
            "cargo",
            &["info", "--registry", "crates-io", &spec],
            Some(self.ctx.workspace_root()),
        )
    }

    fn github_release_is_published(&self, tag_name: &str) -> RailResult<bool> {
        let output = process::run(
            "gh",
            &["release", "view", tag_name, "--json", "isDraft"],
            Some(self.ctx.workspace_root()),
        )?;
        if !output.status.success() {
            return Ok(false);
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| RailError::message(format!("invalid gh release JSON: {}", error)))?;
        Ok(value.get("isDraft").and_then(serde_json::Value::as_bool) == Some(false))
    }

    fn existing_forge_release_matches(&self, forge: ReleaseForge, plan: &CrateReleasePlan) -> RailResult<bool> {
        if !self.forge_release_exists(forge, &plan.tag_name) {
            return Ok(false);
        }
        if forge == ReleaseForge::Gitlab {
            return Ok(true);
        }
        let output = process::run(
            "gh",
            &["release", "view", &plan.tag_name, "--json", "targetCommitish"],
            Some(self.ctx.workspace_root()),
        )?;
        if !output.status.success() {
            return Err(RailError::message(format!(
                "failed to inspect existing GitHub release '{}'",
                plan.tag_name
            )));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| RailError::message(format!("invalid gh release JSON: {}", error)))?;
        let target = value
            .get("targetCommitish")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let expected = self.tag_target_commit(&plan.tag_name)?;
        if !target.is_empty() && target != expected && target != plan.tag_name {
            return Err(RailError::message(format!(
                "existing GitHub release '{}' targets '{}', expected '{}'",
                plan.tag_name, target, expected
            )));
        }
        Ok(true)
    }

    fn ensure_only_release_paths_changed(&self, state: &ReleaseState) -> RailResult<()> {
        let allowed = state
            .planned_paths
            .iter()
            .chain(&state.control_paths)
            .cloned()
            .collect::<BTreeSet<_>>();
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

    fn restore_interrupted_local_step(&self, state: &ReleaseState) -> RailResult<()> {
        self.ensure_only_release_paths_changed(state)?;
        self.ctx.git()?.git().run_git(&["reset", "--hard", "HEAD"])?;
        self.clean_untracked_planned_paths(state)?;
        let before_first_commit = !state.crates.iter().any(|crate_state| crate_state.commit.is_complete());
        self.restore_local_input_backups(state, before_first_commit)?;
        Ok(())
    }

    fn restore_local_input_backups(&self, state: &ReleaseState, include_consumed_inputs: bool) -> RailResult<()> {
        for backup in &state.local_input_backups {
            if !include_consumed_inputs && !matches!(backup.restore, BackupRestorePolicy::Always) {
                continue;
            }
            crate::utils::write_file_atomic(&backup.path, backup.content.as_bytes())?;
        }
        Ok(())
    }

    fn clean_untracked_planned_paths(&self, state: &ReleaseState) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        for path in &state.planned_paths {
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

    fn tag_target_commit(&self, tag_name: &str) -> RailResult<String> {
        self.ctx
            .git()?
            .git()
            .run_git_stdout(&["rev-parse", "--verify", &format!("refs/tags/{}^{{commit}}", tag_name)])
    }

    fn validate_release_notes_size(&self, plan: &CrateReleasePlan, skip_tag: bool) -> RailResult<()> {
        if !self.release_config.remote_effects.creates_forge_release()
            || skip_tag
            || self.detect_release_forge()? != ReleaseForge::Github
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
                format!(
                    "provide a shorter manual override at {}/v{}.md",
                    self.release_config.release_notes_dir, plan.new_version
                ),
            ));
        }
        Ok(())
    }

    fn write_release_notes_temp(&self, plan: &CrateReleasePlan) -> RailResult<PathBuf> {
        let dir = crate::workspace::cargo_rail_state_root(self.ctx.workspace_root()).join("release-notes");
        fs::create_dir_all(&dir)
            .map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
        let path = dir.join(format!("{}.md", sanitize_filename(&plan.tag_name)));
        fs::write(&path, self.release_notes(plan)?)
            .map_err(|e| RailError::message(format!("failed to write {}: {}", path.display(), e)))?;
        Ok(path)
    }

    fn release_notes(&self, plan: &CrateReleasePlan) -> RailResult<String> {
        if let Some(path) = self.release_notes_override_path(plan) {
            return fs::read_to_string(&path)
                .map_err(|e| RailError::message(format!("failed to read {}: {}", path.display(), e)));
        }

        if plan.changelog_path.exists() {
            let changelog = fs::read_to_string(&plan.changelog_path)
                .map_err(|e| RailError::message(format!("failed to read {}: {}", plan.changelog_path.display(), e)))?;
            if let Some(section) = extract_changelog_section(&changelog, &plan.new_version.to_string()) {
                return Ok(section);
            }
        }

        Ok(format!("Release {} v{}\n", plan.name, plan.new_version))
    }

    fn release_notes_override_path(&self, plan: &CrateReleasePlan) -> Option<PathBuf> {
        let dir = self.ctx.workspace_root().join(&self.release_config.release_notes_dir);
        let version_path = dir.join(format!("v{}.md", plan.new_version));
        if version_path.exists() {
            return Some(version_path);
        }

        let tag_path = dir.join(format!("{}.md", plan.tag_name));
        if tag_path.exists() {
            return Some(tag_path);
        }

        None
    }

    /// Get current date in YYYY-MM-DD format
    fn get_current_date(&self) -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    fn format_version_header(
        &self,
        plan: &CrateReleasePlan,
        previous_tag: Option<&str>,
        date: &str,
        github_repo: Option<&(String, String)>,
    ) -> String {
        if let Some((org, repo)) = github_repo {
            let url = if let Some(prev) = previous_tag {
                format!(
                    "https://github.com/{}/{}/compare/{}...{}",
                    org, repo, prev, plan.tag_name
                )
            } else {
                format!("https://github.com/{}/{}/releases/tag/{}", org, repo, plan.tag_name)
            };

            return format!("## [{}]({}) - {}\n\n", plan.new_version, url, date);
        }

        format!("## [{}] - {}\n\n", plan.new_version, date)
    }
}

fn insert_changelog_release(existing: &str, release: &str) -> String {
    let lines: Vec<&str> = existing.lines().collect();
    let first_release = lines
        .iter()
        .position(|line| line.starts_with("## ["))
        .unwrap_or(lines.len());
    let mut updated = String::new();

    if let Some(header) = lines.first() {
        updated.push_str(header);
        updated.push_str("\n\n");
    }
    for line in lines.iter().take(first_release).skip(1) {
        updated.push_str(line);
        updated.push('\n');
    }
    if !updated.is_empty() && !updated.ends_with("\n\n") {
        updated.push('\n');
    }
    updated.push_str(release.trim_start_matches('\n'));
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    for line in lines.iter().skip(first_release) {
        updated.push_str(line);
        updated.push('\n');
    }

    updated
}

fn extract_changelog_section(changelog: &str, version: &str) -> Option<String> {
    let needle = format!("## [{}]", version);
    let mut section = String::new();
    let mut in_section = false;

    for line in changelog.lines() {
        if line.trim_start().starts_with("## ") {
            if in_section {
                break;
            }
            in_section = line.trim_start().starts_with(&needle);
        }

        if in_section {
            section.push_str(line);
            section.push('\n');
        }
    }

    if section.trim().is_empty() { None } else { Some(section) }
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

fn release_branch_name(plan: &ReleasePlan) -> RailResult<String> {
    let json = serde_json::to_string(plan)
        .map_err(|e| RailError::message(format!("failed to serialize release plan for branch hash: {}", e)))?;
    Ok(format!("rail/release-{}", short_hash(&json)))
}

fn detect_release_forge_from_remote(remote: &str) -> Option<ReleaseForge> {
    let lower = remote.to_ascii_lowercase();
    if lower.contains("github.com") {
        Some(ReleaseForge::Github)
    } else if lower.contains("gitlab.com") {
        Some(ReleaseForge::Gitlab)
    } else {
        None
    }
}

pub(crate) fn observe_github_exact_sha_readiness(
    workspace_root: &Path,
    release_commit: &str,
) -> RailResult<CheckReadiness> {
    let (owner, repository) = detect_github_repo(workspace_root)
        .ok_or_else(|| RailError::message("could not derive the GitHub repository identity from origin"))?;
    const QUERY: &str = "query($owner:String!,$repository:String!,$oid:GitObjectID!){repository(owner:$owner,name:$repository){object(oid:$oid){... on Commit{statusCheckRollup{state contexts{totalCount checkRunCount checkRunCountsByState{state count} statusContextCount statusContextCountsByState{state count}}}}}}}";
    let query = format!("query={}", QUERY);
    let owner = format!("owner={}", owner);
    let repository = format!("repository={}", repository);
    let oid = format!("oid={}", release_commit);
    let output = process::run(
        "gh",
        &[
            "api",
            "graphql",
            "-f",
            &query,
            "-F",
            &owner,
            "-F",
            &repository,
            "-F",
            &oid,
        ],
        Some(workspace_root),
    )?;
    if !output.status.success() {
        return Err(RailError::with_help(
            format!(
                "failed to inspect GitHub checks for {}: {}",
                release_commit,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "restore GitHub API access, then resume; cargo-rail will not create tags without exact-SHA evidence",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| RailError::message(format!("invalid GitHub readiness JSON: {}", error)))?;
    Ok(github_check_readiness(&value, release_commit))
}

pub(crate) fn observe_gitlab_exact_sha_readiness(
    workspace_root: &Path,
    release_commit: &str,
) -> RailResult<CheckReadiness> {
    let endpoint = format!(
        "projects/:id/pipelines?sha={}&per_page=1&order_by=id&sort=desc",
        release_commit
    );
    let output = process::run("glab", &["api", &endpoint], Some(workspace_root))?;
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

#[derive(Debug, Default)]
struct GithubContextSummary {
    successful: u64,
    non_authorizing: u64,
    waiting: u64,
    failed: u64,
    observed: u64,
}

impl GithubContextSummary {
    fn add_check_run(&mut self, state: &str, count: u64) {
        self.observed += count;
        match state {
            "SUCCESS" => self.successful += count,
            "NEUTRAL" | "SKIPPED" => self.non_authorizing += count,
            "COMPLETED" | "IN_PROGRESS" | "PENDING" | "QUEUED" | "WAITING" => self.waiting += count,
            "ACTION_REQUIRED" | "CANCELLED" | "FAILURE" | "STALE" | "STARTUP_FAILURE" | "TIMED_OUT" => {
                self.failed += count;
            }
            _ => self.waiting += count,
        }
    }

    fn add_status_context(&mut self, state: &str, count: u64) {
        self.observed += count;
        match state {
            "SUCCESS" => self.successful += count,
            "EXPECTED" | "PENDING" => self.waiting += count,
            "ERROR" | "FAILURE" => self.failed += count,
            _ => self.waiting += count,
        }
    }
}

fn github_context_summary(contexts: &serde_json::Value) -> Option<GithubContextSummary> {
    let total = contexts.get("totalCount")?.as_u64()?;
    let check_runs = contexts.get("checkRunCount")?.as_u64()?;
    let status_contexts = contexts.get("statusContextCount")?.as_u64()?;
    if check_runs + status_contexts != total {
        return None;
    }

    let mut summary = GithubContextSummary::default();
    for item in contexts.get("checkRunCountsByState")?.as_array()? {
        summary.add_check_run(item.get("state")?.as_str()?, item.get("count")?.as_u64()?);
    }
    for item in contexts.get("statusContextCountsByState")?.as_array()? {
        summary.add_status_context(item.get("state")?.as_str()?, item.get("count")?.as_u64()?);
    }

    (summary.observed == total).then_some(summary)
}

fn github_check_readiness(value: &serde_json::Value, release_commit: &str) -> CheckReadiness {
    let rollup = value.pointer("/data/repository/object/statusCheckRollup");
    let Some(rollup) = rollup else {
        return CheckReadiness::Waiting("GitHub has not reported any checks for the release commit".to_string());
    };
    let Some(summary) = rollup.get("contexts").and_then(github_context_summary) else {
        return CheckReadiness::Waiting("GitHub check context counts are incomplete or malformed".to_string());
    };

    if summary.failed > 0 {
        CheckReadiness::Failed(format!("GitHub reports {} failed check context(s)", summary.failed))
    } else if summary.waiting > 0 {
        CheckReadiness::Waiting(format!("GitHub reports {} pending check context(s)", summary.waiting))
    } else if summary.successful == 0 {
        CheckReadiness::Waiting(format!(
            "GitHub has no completed successful checks ({} skipped or neutral)",
            summary.non_authorizing
        ))
    } else {
        CheckReadiness::Green(format!(
            "github:{}:{}_successful_checks",
            release_commit, summary.successful
        ))
    }
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

fn gitlab_release_create_args(tag: &str, title: &str, notes_file: &str) -> Vec<String> {
    vec![
        "release".to_string(),
        "create".to_string(),
        tag.to_string(),
        "--name".to_string(),
        title.to_string(),
        "--notes-file".to_string(),
        notes_file.to_string(),
    ]
}

fn short_hash(value: &str) -> String {
    let hash = crate::utils::fnv1a64(value.as_bytes());
    format!("{:08x}", hash & 0xffff_ffff)
}

fn release_pr_body(plan: &ReleasePlan) -> String {
    let mut out = plan.format_summary_with_flags(true, true);
    out.push_str("\n## Changelog Bodies\n\n");
    for crate_plan in &plan.crates {
        out.push_str(&format!("### {} v{}\n\n", crate_plan.name, crate_plan.new_version));
        if crate_plan.changelog_body.trim().is_empty() {
            out.push_str("_No generated changelog entries._\n\n");
        } else {
            out.push_str(crate_plan.changelog_body.trim());
            out.push_str("\n\n");
        }
    }
    out
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

fn fault_after(step: &str, subject: &str) -> RailResult<()> {
    let Ok(requested) = std::env::var("CARGO_RAIL_RELEASE_FAIL_AFTER") else {
        return Ok(());
    };
    let point = format!("{}:{}", step, subject);
    if requested == step || requested == point {
        return Err(RailError::message(format!("injected release failure after {}", point)));
    }
    Ok(())
}

fn fault_before(step: &str, subject: &str) -> RailResult<()> {
    let Ok(requested) = std::env::var("CARGO_RAIL_RELEASE_FAIL_BEFORE") else {
        return Ok(());
    };
    let point = format!("{}:{}", step, subject);
    if requested == step || requested == point {
        return Err(RailError::message(format!("injected release failure before {}", point)));
    }
    Ok(())
}

fn advance_phase(state: &mut ReleaseState, state_path: &std::path::Path, phase: ReleasePhase) -> RailResult<()> {
    if state.phase < phase {
        state.phase = phase;
        state.save(state_path, phase.as_str())?;
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
            state_path.display()
        ),
    )
}

fn registry_wait_error(plan: &CrateReleasePlan) -> RailError {
    RailError::with_help(
        format!(
            "{} v{} publication is not yet observable on crates.io",
            plan.name, plan.new_version
        ),
        "stop here and resume later; cargo-rail will reconcile registry truth before issuing another publish",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_changelog_section_returns_only_requested_version() {
        let changelog = r#"# Changelog

## [0.2.0] - 2026-06-01

### Features

- new API

## [0.1.0] - 2026-05-01

- old API
"#;

        let section = extract_changelog_section(changelog, "0.2.0").unwrap();
        assert!(section.contains("new API"));
        assert!(!section.contains("old API"));
    }

    #[test]
    fn changelog_release_follows_the_preamble() {
        let existing = "# Changelog\n\nThis file records user-visible changes.\n\n## [0.15.0] - 2026-06-01\n\n- old\n";
        let release = "## [0.16.0] - 2026-07-11\n\n- new\n\n";

        let updated = insert_changelog_release(existing, release);

        let preamble = updated.find("This file records user-visible changes.").unwrap();
        let current = updated.find("## [0.16.0]").unwrap();
        let previous = updated.find("## [0.15.0]").unwrap();
        assert!(preamble < current && current < previous, "{}", updated);
    }

    #[test]
    fn release_branch_name_is_stable() {
        let plan = ReleasePlan {
            plan_contract_version: 4,
            snapshot_id: String::new(),
            source: crate::config::ReleaseSource::Changes,
            canonical_crate_order: Vec::new(),
            crates: Vec::new(),
            summary: crate::release::planner::ReleaseSummary {
                total_crates: 0,
                crates_to_publish: 0,
                crates_to_tag: 0,
            },
            change_files_to_delete: Vec::new(),
            change_files_to_update: Vec::new(),
            skipped: Vec::new(),
        };
        assert_eq!(release_branch_name(&plan).unwrap(), release_branch_name(&plan).unwrap());
        assert!(release_branch_name(&plan).unwrap().starts_with("rail/release-"));
    }

    #[test]
    fn detects_release_forge_from_common_remotes() {
        assert_eq!(
            detect_release_forge_from_remote("git@github.com:org/repo.git"),
            Some(ReleaseForge::Github)
        );
        assert_eq!(
            detect_release_forge_from_remote("https://gitlab.com/org/repo.git"),
            Some(ReleaseForge::Gitlab)
        );
        assert_eq!(
            detect_release_forge_from_remote("https://git.example/org/repo.git"),
            None
        );
    }

    #[test]
    fn github_readiness_requires_an_executed_successful_context() {
        let success = serde_json::json!({
          "data": { "repository": { "object": { "statusCheckRollup": {
            "state": "SUCCESS",
            "contexts": {
              "totalCount": 3,
              "checkRunCount": 2,
              "checkRunCountsByState": [
                { "state": "SUCCESS", "count": 1 },
                { "state": "SKIPPED", "count": 1 }
              ],
              "statusContextCount": 1,
              "statusContextCountsByState": [{ "state": "SUCCESS", "count": 1 }]
            }
          } } } }
        });
        assert!(matches!(
          github_check_readiness(&success, "abc123"),
          CheckReadiness::Green(detail) if detail == "github:abc123:2_successful_checks"
        ));

        let skipped = serde_json::json!({
          "data": { "repository": { "object": { "statusCheckRollup": {
            "state": "SUCCESS",
            "contexts": {
              "totalCount": 2,
              "checkRunCount": 2,
              "checkRunCountsByState": [
                { "state": "SKIPPED", "count": 1 },
                { "state": "NEUTRAL", "count": 1 }
              ],
              "statusContextCount": 0,
              "statusContextCountsByState": []
            }
          } } } }
        });
        assert!(matches!(
          github_check_readiness(&skipped, "abc123"),
          CheckReadiness::Waiting(detail) if detail.contains("no completed successful checks")
        ));

        let pending = serde_json::json!({
          "data": { "repository": { "object": { "statusCheckRollup": {
            "state": "PENDING",
            "contexts": {
              "totalCount": 2,
              "checkRunCount": 2,
              "checkRunCountsByState": [
                { "state": "SUCCESS", "count": 1 },
                { "state": "IN_PROGRESS", "count": 1 }
              ],
              "statusContextCount": 0,
              "statusContextCountsByState": []
            }
          } } } }
        });
        assert!(matches!(
          github_check_readiness(&pending, "abc123"),
          CheckReadiness::Waiting(detail) if detail == "GitHub reports 1 pending check context(s)"
        ));

        let failure = serde_json::json!({
          "data": { "repository": { "object": { "statusCheckRollup": {
            "state": "FAILURE",
            "contexts": {
              "totalCount": 2,
              "checkRunCount": 1,
              "checkRunCountsByState": [{ "state": "SUCCESS", "count": 1 }],
              "statusContextCount": 1,
              "statusContextCountsByState": [{ "state": "ERROR", "count": 1 }]
            }
          } } } }
        });
        assert!(matches!(
          github_check_readiness(&failure, "abc123"),
          CheckReadiness::Failed(detail) if detail == "GitHub reports 1 failed check context(s)"
        ));

        let malformed = serde_json::json!({
          "data": { "repository": { "object": { "statusCheckRollup": {
            "state": "SUCCESS",
            "contexts": {
              "totalCount": 2,
              "checkRunCount": 2,
              "checkRunCountsByState": [{ "state": "SUCCESS", "count": 1 }],
              "statusContextCount": 0,
              "statusContextCountsByState": []
            }
          } } } }
        });
        assert!(matches!(
          github_check_readiness(&malformed, "abc123"),
          CheckReadiness::Waiting(detail) if detail.contains("incomplete or malformed")
        ));
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
            gitlab_release_create_args("v1.0.0", "crate v1.0.0", "/tmp/notes.md"),
            vec![
                "release",
                "create",
                "v1.0.0",
                "--name",
                "crate v1.0.0",
                "--notes-file",
                "/tmp/notes.md"
            ]
        );
    }
}
