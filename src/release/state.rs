//! Durable, idempotent release execution state.

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::release::planner::{RELEASE_PLAN_CONTRACT_VERSION, RELEASE_REGISTRY, ReleasePlan};
use crate::release::remote::RemoteRepository;
use crate::utils::canonicalize_existing;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) const RELEASE_STATE_SCHEMA_VERSION: u32 = 10;

#[derive(Deserialize)]
struct ReleaseStateSchema {
    schema_version: u32,
    transaction_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseState {
    pub schema_version: u32,
    pub transaction_id: String,
    pub status: ReleaseStatus,
    pub phase: ReleasePhase,
    pub remote_storage: bool,
    pub review: Option<super::review::ReviewState>,
    pub executor: Option<u64>,
    pub validation_dispatches: std::collections::BTreeMap<String, u64>,
    pub intent: ReleaseIntent,
    pub crates: Vec<CrateReleaseState>,
    pub preparation: Preparation,
    #[serde(deserialize_with = "Option::deserialize")]
    pub package_seal: Option<super::packages::PackageSeal>,
    pub commit_push: Step,
    pub readiness: Step,
    pub validation: Vec<super::validation::WorkflowRun>,
    pub artifacts: Vec<super::artifacts::ArtifactEvidence>,
    pub tag_push: Step,
    pub abort: Step,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseIntent {
    pub identity: String,
    pub source_tree: String,
    pub plan: ReleasePlan,
    pub release_config: ReleaseConfig,
    #[serde(default)]
    pub remote_repository: Option<RemoteRepository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_registry: Option<String>,
    pub review: bool,
    pub hosted: bool,
    pub alias_previous: std::collections::BTreeMap<String, Option<String>>,
    pub skip_publish: bool,
    pub skip_tag: bool,
    pub initial_head: String,
    pub branch: String,
    #[serde(serialize_with = "super::path_serde::serialize_vec")]
    pub planned_paths: Vec<PathBuf>,
    #[serde(serialize_with = "super::path_serde::serialize_vec")]
    pub control_paths: Vec<PathBuf>,
    pub local_input_backups: Vec<LocalInputBackup>,
}

impl ReleaseIntent {
    fn identity_for(&self, transaction_id: &str) -> RailResult<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .ok_or_else(|| RailError::message("invalid release intent"))?
            .remove("identity");
        super::contract::identity(
            "cargo-rail-release-intent-v1",
            serde_json::json!({"transaction_id": transaction_id, "intent": value}),
        )
    }

    fn visit_paths(&mut self, mut visit: impl FnMut(&mut PathBuf) -> RailResult<()>) -> RailResult<()> {
        for path in self.planned_paths.iter_mut().chain(&mut self.control_paths) {
            visit(path)?;
        }
        for backup in &mut self.local_input_backups {
            visit(&mut backup.path)?;
        }
        visit_plan_paths(&mut self.plan, &mut visit)?;
        Ok(())
    }

    pub(crate) fn execution_plan(&self, git_root: &Path) -> RailResult<ReleasePlan> {
        let mut plan = self.plan.clone();
        visit_plan_paths(&mut plan, &mut |path| {
            *path = git_root.join(&*path);
            Ok(())
        })?;
        Ok(plan)
    }

    fn make_portable(&mut self, root: &Path) -> RailResult<()> {
        self.visit_paths(|path| {
            let relative = normalize_release_path(root, path, "intent")?;
            *path = crate::source::RepositoryPath::new(&relative)?.as_path().to_path_buf();
            Ok(())
        })?;
        self.planned_paths.sort();
        self.control_paths.sort();
        self.validate_portable()
    }

    fn validate_portable(&self) -> RailResult<()> {
        self.clone().visit_paths(|path| {
            let text = path
                .to_str()
                .ok_or_else(|| RailError::message("release intent path is not UTF-8"))?;
            let normalized = crate::source::RepositoryPath::new(path)?;
            if normalized.as_str() != text || text.contains(['\\', ':']) {
                return Err(RailError::message(
                    "release intent paths must be normalized repository-relative paths",
                ));
            }
            Ok(())
        })
    }
}

fn visit_plan_paths(plan: &mut ReleasePlan, visit: &mut impl FnMut(&mut PathBuf) -> RailResult<()>) -> RailResult<()> {
    for path in &mut plan.change_files_to_delete {
        visit(path)?;
    }
    for update in &mut plan.change_files_to_update {
        visit(&mut update.path)?;
    }
    for auxiliary in &mut plan.auxiliary_lockfiles {
        visit(&mut auxiliary.manifest_path)?;
        visit(&mut auxiliary.lockfile_path)?;
    }
    for package in &mut plan.crates {
        visit(&mut package.manifest_path)?;
        visit(&mut package.changelog_path)?;
        for change in &mut package.change_entries {
            visit(&mut change.path)?;
        }
        if let Some(presentation) = &mut package.presentation {
            if let Some(changelog) = &mut presentation.changelog {
                visit(&mut changelog.path)?;
            }
            for input in &mut presentation.note_inputs {
                visit(&mut input.path)?;
            }
        }
    }
    Ok(())
}

pub(crate) struct ReleaseStateCreate<'a> {
    pub(crate) root: &'a Path,
    pub(crate) transaction_id: String,
    pub(crate) review: bool,
    pub(crate) hosted: bool,
    pub(crate) remote_storage: bool,
    pub(crate) plan: ReleasePlan,
    pub(crate) release_config: ReleaseConfig,
    pub(crate) remote_repository: Option<RemoteRepository>,
    pub(crate) skip_publish: bool,
    pub(crate) skip_tag: bool,
    pub(crate) initial_head: String,
    pub(crate) branch: String,
    pub(crate) planned_paths: Vec<PathBuf>,
    pub(crate) control_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalInputBackup {
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    pub content: String,
    #[serde(default)]
    pub restore: BackupRestorePolicy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupRestorePolicy {
    #[default]
    BeforePreparation,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Preparation {
    Pending,
    Writing,
    Committing { tree: String },
    Complete { commit: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleaseStatus {
    Active,
    Complete,
    Aborted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleasePhase {
    #[default]
    Planned,
    Prepared,
    AwaitingReview,
    AwaitingChecks,
    Ready,
    Publishing,
    Released,
}

impl ReleasePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Prepared => "prepared",
            Self::AwaitingReview => "awaiting_review",
            Self::AwaitingChecks => "awaiting_checks",
            Self::Ready => "ready",
            Self::Publishing => "publishing",
            Self::Released => "released",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CrateReleaseState {
    pub name: String,
    pub tag: Step,
    pub tag_object: Option<TagObject>,
    pub forge_draft: Step,
    pub publication: Step,
    pub publication_attempt: Option<String>,
    pub forge_publication: Step,
    pub alias: Step,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TagObject {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Step {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Complete,
}

impl Step {
    pub fn is_complete(&self) -> bool {
        self.status == StepStatus::Complete
    }
}

impl ReleaseState {
    pub fn create(request: ReleaseStateCreate<'_>) -> RailResult<(Self, PathBuf)> {
        let ReleaseStateCreate {
            root,
            transaction_id,
            review,
            hosted,
            remote_storage,
            plan,
            release_config,
            remote_repository,
            skip_publish,
            skip_tag,
            initial_head,
            branch,
            planned_paths,
            control_paths,
        } = request;
        let directory = state_dir(root);
        if directory.try_exists()? {
            for entry in std::fs::read_dir(&directory)? {
                let path = entry?.path();
                if path.extension().is_some_and(|extension| extension == "json") {
                    let existing = Self::load(&path)?;
                    if existing.status == ReleaseStatus::Active {
                        return Err(RailError::with_help(
                            format!("release transaction '{}' is already active", existing.transaction_id),
                            format!(
                                "inspect it with 'cargo rail release status {}'",
                                existing.transaction_id
                            ),
                        ));
                    }
                }
            }
        }
        let git = SystemGit::open(root)?;
        for planned in &plan.crates {
            crate::release::presentation::validate_inputs(root, planned)?;
        }
        let publish_registry = if skip_publish {
            None
        } else {
            let registry = release_config
                .registry_publication
                .registry()
                .ok_or_else(|| RailError::message("release state has no configured registry publication authority"))?;
            if registry != RELEASE_REGISTRY {
                return Err(RailError::message(format!(
                    "release state selected unsupported registry '{registry}'"
                )));
            }
            Some(registry.to_string())
        };
        let mut local_input_paths = plan
            .change_files_to_delete
            .iter()
            .cloned()
            .chain(plan.change_files_to_update.iter().map(|update| update.path.clone()))
            .map(|path| (path, BackupRestorePolicy::BeforePreparation))
            .collect::<BTreeMap<_, _>>();
        for (path, restore) in control_paths.iter().filter_map(|path| {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                git.worktree_root.join(path)
            };
            (absolute.is_file() && crate::utils::path_relative_to(&git.worktree_root, &absolute).is_ok())
                .then_some((absolute, BackupRestorePolicy::Always))
        }) {
            local_input_paths.entry(path).or_insert(restore);
        }
        let local_input_backups = local_input_paths
            .into_iter()
            .map(|(path, restore)| {
                let content = std::fs::read_to_string(&path)
                    .map_err(|error| RailError::message(format!("failed to preserve {}: {}", path.display(), error)))?;
                Ok(LocalInputBackup { path, content, restore })
            })
            .collect::<RailResult<Vec<_>>>()?;
        let crates = plan
            .crates
            .iter()
            .map(|crate_plan| CrateReleaseState {
                name: crate_plan.name.clone(),
                tag: if skip_tag { complete_step(None) } else { Step::default() },
                tag_object: None,
                forge_draft: Step::default(),
                publication: if skip_publish || !crate_plan.publish {
                    complete_step(None)
                } else {
                    Step::default()
                },
                publication_attempt: None,
                forge_publication: Step::default(),
                alias: Step::default(),
            })
            .collect();
        let mut alias_previous = std::collections::BTreeMap::new();
        let mut alias_names = BTreeSet::new();
        for (package, alias) in &release_config.aliases {
            if !plan.crates.iter().any(|planned| &planned.name == package) {
                continue;
            }
            if skip_tag
                || !release_config.remote_effects.creates_forge_release()
                || !git.run_git_check(&["check-ref-format", &format!("refs/tags/{alias}")])
                || plan.crates.iter().any(|planned| &planned.tag_name == alias)
                || !alias_names.insert(alias)
            {
                return Err(RailError::message(
                    "release aliases require unique tags and immutable forge publication",
                ));
            }
            alias_previous.insert(
                package.clone(),
                super::review::remote_head(&git, &format!("refs/tags/{alias}"))?,
            );
        }
        let mut state = Self {
            schema_version: RELEASE_STATE_SCHEMA_VERSION,
            transaction_id: transaction_id.clone(),
            status: ReleaseStatus::Active,
            phase: ReleasePhase::Planned,
            remote_storage,
            review: review.then(super::review::ReviewState::default),
            executor: None,
            validation_dispatches: std::collections::BTreeMap::new(),
            intent: ReleaseIntent {
                identity: String::new(),
                source_tree: git.run_git_stdout(&["show", "-s", "--format=%T", &initial_head])?,
                plan,
                release_config,
                remote_repository,
                publish_registry,
                review,
                hosted,
                alias_previous,
                skip_publish,
                skip_tag,
                initial_head,
                branch,
                planned_paths,
                control_paths,
                local_input_backups,
            },
            crates,
            preparation: Preparation::Pending,
            package_seal: None,
            commit_push: Step::default(),
            readiness: Step::default(),
            validation: Vec::new(),
            artifacts: Vec::new(),
            tag_push: Step::default(),
            abort: Step::default(),
        };
        for projection in &mut state.intent.plan.auxiliary_lockfiles {
            projection.manifest_path = root.join(&projection.manifest_path);
            projection.lockfile_path = root.join(&projection.lockfile_path);
        }
        state.intent.make_portable(&git.worktree_root)?;
        state.intent.identity = state.intent.identity_for(&state.transaction_id)?;
        state.validate_contract()?;
        state.validate_recovery_paths(&git.worktree_root)?;
        let path = state_dir(root).join(format!("{}.json", transaction_id));
        if path.exists() {
            let existing = Self::load(&path)?;
            let help = match existing.status {
                ReleaseStatus::Active => {
                    format!("resume it with 'cargo rail release resume {}'", existing.transaction_id)
                }
                ReleaseStatus::Complete | ReleaseStatus::Aborted => {
                    format!(
                        "delete only this terminal journal with 'cargo rail clean --release-journal {}'",
                        state.transaction_id
                    )
                }
            };
            return Err(RailError::with_help(
                format!(
                    "release transaction '{}' already exists at '{}'",
                    transaction_id,
                    path.display()
                ),
                help,
            ));
        }
        if let Err(error) = state.save(&path) {
            if path.exists() {
                return Err(error.context(format!(
                    "release journal may have been persisted at '{}'; inspect it with: cargo rail release status {}",
                    path.display(),
                    path.display()
                )));
            }
            return Err(error);
        }
        Ok((state, path))
    }

    pub fn load(path: &Path) -> RailResult<Self> {
        use std::io::Read as _;
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || metadata.len() > super::contract::MAX_RECORD_BYTES as u64
        {
            return Err(RailError::message("release record must be a bounded regular file"));
        }
        let file = std::fs::File::open(path)?;
        if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
            return Err(RailError::message("release record changed while opening"));
        }
        let mut bytes = Vec::new();
        file.take(super::contract::MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        let state = Self::decode(&bytes, path)?;
        state.validate_journal_path(path)?;
        Ok(state)
    }

    /// Validate current recovery authority without rewriting the journal.
    pub(crate) fn load_for_recovery(path: &Path) -> RailResult<Self> {
        let state = Self::load(path)?;
        if state.status == ReleaseStatus::Active {
            state.validate_recovery_paths(release_root(path))?;
        }
        Ok(state)
    }

    fn decode(bytes: &[u8], path: &Path) -> RailResult<Self> {
        let schema: ReleaseStateSchema = serde_json::from_slice(bytes)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
        validate_transaction_id(&schema.transaction_id)?;
        validate_journal_path(&schema.transaction_id, path)?;
        if schema.schema_version != RELEASE_STATE_SCHEMA_VERSION {
            return Err(RailError::with_help(
                format!(
                    "unsupported release state version {} in '{}'",
                    schema.schema_version,
                    path.display()
                ),
                "preserve this journal and use the executable that created it to finish or safely abort/reconcile the transaction; its release version is unknown and publication may already have happened",
            ));
        }
        let state: Self = super::contract::decode(bytes)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
        state.validate_contract()?;
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> RailResult<()> {
        let bytes = self.bytes_for_save(path)?;
        if self.remote_storage {
            super::storage::store(release_root(path), self)?;
        }
        crate::utils::write_file_atomic(path, &bytes)
    }

    pub(crate) fn retain_local(&self, path: &Path) -> RailResult<()> {
        let bytes = self.bytes_for_save(path)?;
        crate::utils::write_file_atomic(path, &bytes)
    }

    fn bytes_for_save(&self, path: &Path) -> RailResult<Vec<u8>> {
        self.validate_contract()?;
        self.validate_journal_path(path)?;
        self.validate_recovery_paths(release_root(path))?;
        let parent = path
            .parent()
            .ok_or_else(|| RailError::message("release state path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        if path.try_exists()? {
            let existing = Self::load(path)?;
            require_successor(&existing, self)?;
        }

        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| RailError::message(format!("failed to serialize release state: {}", error)))?;
        Ok(bytes)
    }

    pub fn crate_index(&self, name: &str) -> RailResult<usize> {
        self.crates
            .iter()
            .position(|state| state.name == name)
            .ok_or_else(|| RailError::message(format!("release state has no crate '{}'", name)))
    }

    pub(crate) fn release_commit(&self) -> Option<&str> {
        if let Some(merge) = self.review.as_ref().and_then(|review| review.merge.as_ref()) {
            return Some(&merge.commit);
        }
        match &self.preparation {
            Preparation::Complete { commit } => Some(commit),
            _ => None,
        }
    }

    pub(crate) fn preparation_message(&self) -> String {
        let packages = self
            .intent
            .plan
            .crates
            .iter()
            .map(|plan| format!("{} v{}", plan.name, plan.new_version))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "chore(release): {packages}\n\nRail-Release: {}\nRail-Release-Contract: {RELEASE_STATE_SCHEMA_VERSION}\nRail-Release-Intent: {}",
            self.transaction_id, self.intent.identity,
        )
    }

    pub(crate) fn validate_preparation_binding(&self, git: &SystemGit) -> RailResult<()> {
        if let Preparation::Complete { commit } = &self.preparation {
            let message = git.run_git_stdout(&["show", "-s", "--format=%B", commit])?;
            let parent = git.run_git_stdout(&["show", "-s", "--format=%P", commit])?;
            if message != self.preparation_message() || parent != self.intent.initial_head {
                return Err(RailError::message(
                    "release is not bound by its original preparation commit",
                ));
            }
            if let Some(merge) = self.review.as_ref().and_then(|review| review.merge.as_ref()) {
                let tree = git.run_git_stdout(&["show", "-s", "--format=%T", &merge.commit])?;
                if tree != merge.tree || tree != git.run_git_stdout(&["show", "-s", "--format=%T", commit])? {
                    return Err(RailError::message(
                        "release merge differs from its retained prepared tree",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Reconcile a possibly completed Git commit using the saved tree and exact message.
    pub(crate) fn reconcile_preparation(&mut self, git: &SystemGit) -> RailResult<bool> {
        let head = git.head_commit()?;
        if let Preparation::Complete { commit } = &self.preparation {
            if &head != commit {
                return Err(RailError::message("release checkout moved from its prepared commit"));
            }
            return Ok(true);
        }
        if head == self.intent.initial_head {
            return Ok(false);
        }
        if let Preparation::Committing { tree } = &self.preparation {
            let parent = git.run_git_stdout(&["show", "-s", "--format=%P", "HEAD"])?;
            let actual_tree = git.run_git_stdout(&["show", "-s", "--format=%T", "HEAD"])?;
            let message = git.run_git_stdout(&["log", "-1", "--format=%B"])?;
            if parent == self.intent.initial_head && &actual_tree == tree && message == self.preparation_message() {
                self.preparation = Preparation::Complete { commit: head };
                return Ok(true);
            }
        }
        Err(RailError::with_help(
            "release preparation found a commit that does not match its saved parent, tree, and message",
            "preserve the journal and inspect the unexpected commit before resuming; no commit was adopted or reset",
        ))
    }

    pub fn validate_recovery_paths(&self, worktree_root: &Path) -> RailResult<()> {
        normalize_release_paths(worktree_root, &self.intent.planned_paths, "planned")?;
        normalize_release_paths(worktree_root, &self.intent.control_paths, "control")?;
        for backup in &self.intent.local_input_backups {
            normalize_release_path(worktree_root, &backup.path, "backup")?;
        }
        Ok(())
    }

    pub(crate) fn validate_contract(&self) -> RailResult<()> {
        self.intent
            .release_config
            .validate_policy()
            .map_err(RailError::Config)?;
        if self.intent.skip_publish != self.intent.publish_registry.is_none()
            || self.intent.publish_registry.as_deref().is_some_and(|registry| {
                registry != RELEASE_REGISTRY
                    || self.intent.release_config.registry_publication.registry() != Some(registry)
            })
            || (self.remote_storage || self.intent.release_config.remote_effects.pushes())
                && self.intent.remote_repository.is_none()
        {
            return Err(RailError::message(
                "release state contains inconsistent registry or repository authority",
            ));
        }
        if self.schema_version != RELEASE_STATE_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "cannot persist unsupported release state version {}",
                self.schema_version
            )));
        }
        if self.intent.plan.plan_contract_version != RELEASE_PLAN_CONTRACT_VERSION {
            return Err(RailError::with_help(
                format!(
                    "release state version {} requires embedded release plan contract {}, found {}",
                    RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION, self.intent.plan.plan_contract_version
                ),
                "resume with the cargo-rail version that created this state, or safely abort and replan",
            ));
        }
        validate_transaction_id(&self.transaction_id)?;
        if self.intent.identity != self.intent.identity_for(&self.transaction_id)? {
            return Err(RailError::message(
                "immutable release intent identity does not match its contents",
            ));
        }
        self.intent.validate_portable()?;
        super::artifacts::validate_record(self)?;
        super::review::validate_record(self)?;
        super::hosted::validate_record(self)?;
        let selected_aliases = self
            .intent
            .release_config
            .aliases
            .keys()
            .filter(|package| self.intent.plan.crates.iter().any(|planned| &planned.name == *package))
            .collect::<BTreeSet<_>>();
        if self.intent.alias_previous.keys().collect::<BTreeSet<_>>() != selected_aliases
            || !selected_aliases.is_empty()
                && (self.intent.skip_tag || !self.intent.release_config.remote_effects.creates_forge_release())
            || self
                .intent
                .alias_previous
                .values()
                .flatten()
                .any(|oid| !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(RailError::message("release alias authority is incomplete"));
        }
        for package in &self.crates {
            if package.alias.status == StepStatus::Pending && package.alias.object.is_some()
                || package.alias.status != StepStatus::Pending
                    && (!selected_aliases.contains(&package.name)
                        || !package.forge_publication.is_complete()
                        || package.alias.object.as_deref() != package.tag_object.as_ref().map(|tag| tag.id.as_str()))
                || self.status == ReleaseStatus::Complete
                    && selected_aliases.contains(&package.name)
                    && !package.alias.is_complete()
            {
                return Err(RailError::message(
                    "release alias progress precedes verified publication",
                ));
            }
        }
        super::validation::validate_records(&self.intent.release_config.validation, &self.validation)?;
        let github_validation_required = self.intent.release_config.remote_effects.pushes()
            && (!self.intent.skip_tag || !self.intent.skip_publish)
            && (self.intent.release_config.remote_effects == crate::config::ReleaseRemoteEffects::Github
                || self
                    .intent
                    .remote_repository
                    .as_ref()
                    .is_some_and(|repository| repository.host() == Some("github.com")));
        if github_validation_required
            && (self.intent.release_config.validation.is_empty()
                || self.readiness.is_complete() && self.validation.is_empty())
        {
            return Err(RailError::message(
                "release readiness has no required workflow evidence",
            ));
        }
        let plan = &self.intent.plan;
        let names = plan.crates.iter().map(|package| &package.name).collect::<BTreeSet<_>>();
        if names.len() != plan.crates.len()
            || plan
                .canonical_crate_order
                .iter()
                .ne(plan.crates.iter().map(|package| &package.name))
            || plan.summary.total_crates != plan.crates.len()
            || plan.summary.crates_to_tag != plan.crates.len()
            || plan.summary.crates_to_publish != plan.crates.iter().filter(|package| package.publish).count()
            || plan.source != self.intent.release_config.source
        {
            return Err(RailError::message(
                "release plan has inconsistent package selection or summary",
            ));
        }
        for paths in [&self.intent.planned_paths, &self.intent.control_paths] {
            if paths.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(RailError::message("release paths must be unique and sorted"));
            }
        }
        let mut tags = BTreeSet::new();
        for package in &plan.crates {
            if !super::registry::valid_name(&package.name) || !tags.insert(&package.tag_name) {
                return Err(RailError::message(
                    "release plan has invalid package names or duplicate tags",
                ));
            }
            package.api_evidence.require_ready(&package.name)?;
            if package.api_evidence.required
                != (self.intent.release_config.semver_check == crate::config::SemverCheckPolicy::Deny)
            {
                return Err(RailError::message(
                    "release API evidence does not match its required policy",
                ));
            }
            let presentation = package
                .presentation
                .as_ref()
                .ok_or_else(|| RailError::message("release package has no captured presentation"))?;
            if package.generate_changelog != presentation.changelog.is_some() {
                return Err(RailError::message(
                    "release changelog policy does not match its captured write",
                ));
            }
            if let Some(write) = &presentation.changelog
                && (write.path != package.changelog_path
                    || crate::source::ContentDigest::sha256(write.content.as_bytes()).to_string() != write.after_digest
                    || write
                        .before_digest
                        .as_deref()
                        .is_some_and(|digest| !super::registry::valid_checksum(digest)))
            {
                return Err(RailError::message(
                    "release changelog bytes do not match their bound path and digest",
                ));
            }
        }
        for projection in &plan.auxiliary_lockfiles {
            if !projection
                .before_digest
                .strip_prefix("sha256:")
                .is_some_and(super::registry::valid_checksum)
                || format!(
                    "sha256:{}",
                    crate::source::ContentDigest::sha256(projection.content.as_bytes())
                ) != projection.after_digest
            {
                return Err(RailError::message(
                    "release auxiliary lockfile bytes do not match their digest",
                ));
            }
        }
        for object in [&self.intent.initial_head, &self.intent.source_tree] {
            if !matches!(object.len(), 40 | 64)
                || !object
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(RailError::message(
                    "release intent contains an invalid Git object identity",
                ));
            }
        }
        if self.crates.len() != self.intent.plan.crates.len()
            || self
                .crates
                .iter()
                .zip(&self.intent.plan.crates)
                .any(|(state, planned)| state.name != planned.name)
        {
            return Err(RailError::message(
                "release progress does not match the immutable package selection",
            ));
        }

        if let Some(seal) = &self.package_seal {
            let source = self
                .release_commit()
                .ok_or_else(|| RailError::message("package evidence has no preparation commit"))?;
            seal.validate(&self.intent.plan, source)?;
        }

        if self.phase >= ReleasePhase::Prepared && self.release_commit().is_none()
            || self.phase == ReleasePhase::Released && self.status != ReleaseStatus::Complete
            || self.status == ReleaseStatus::Aborted && !self.abort.is_complete()
            || self.status == ReleaseStatus::Complete
                && (self.phase != ReleasePhase::Released
                    || !self.commit_push.is_complete()
                    || !self.readiness.is_complete()
                    || !self.tag_push.is_complete()
                    || self.crates.iter().any(|package| {
                        !package.tag.is_complete()
                            || !package.publication.is_complete()
                            || self.intent.release_config.remote_effects.creates_forge_release()
                                && !self.intent.skip_tag
                                && (!package.forge_draft.is_complete() || !package.forge_publication.is_complete())
                    }))
        {
            return Err(RailError::message(
                "release phase or terminal status contradicts its retained effects",
            ));
        }
        for (progress, package) in self.crates.iter().zip(&plan.crates) {
            if let Some(tag) = &progress.tag_object {
                let source = self.release_commit().unwrap_or_default();
                if self.intent.skip_tag
                    || progress.tag.status != StepStatus::Complete
                    || progress.tag.object.as_deref() != Some(tag.id.as_str())
                    || !matches!(tag.id.len(), 40 | 64)
                    || !tag
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || tag.content.len() > 64 * 1024
                    || !tag.content.starts_with(&format!(
                        "object {source}\ntype commit\ntag {}\ntagger ",
                        package.tag_name
                    ))
                    || !tag.content.split_once("\n\n").is_some_and(|(_, body)| {
                        body.starts_with(&format!("Release {} v{}\n", package.name, package.new_version))
                    })
                {
                    return Err(RailError::message(
                        "release tag object does not match its prepared identity",
                    ));
                }
            } else if !self.intent.skip_tag && progress.tag.is_complete() {
                return Err(RailError::message("completed release tag has no retained object"));
            }
            if progress
                .publication_attempt
                .as_deref()
                .is_some_and(|id| !super::registry::valid_checksum(id))
                || progress.publication.status == StepStatus::Pending && progress.publication_attempt.is_some()
                || progress.publication.status == StepStatus::InProgress && progress.publication_attempt.is_none()
            {
                return Err(RailError::message(
                    "release publication has inconsistent upload attempt identity",
                ));
            }
            if !self.intent.skip_publish && package.publish && progress.publication.status != StepStatus::Pending {
                if self.phase < ReleasePhase::Publishing
                    || !self.readiness.is_complete()
                    || !self.commit_push.is_complete()
                    || !self.intent.skip_tag && self.crates.iter().any(|package| !package.tag.is_complete())
                {
                    return Err(RailError::message(
                        "release upload precedes required preparation and validation",
                    ));
                }
                let archive = self
                    .package_seal
                    .as_ref()
                    .and_then(|seal| seal.packages.iter().find(|archive| archive.name == package.name));
                if archive.is_none_or(|archive| progress.publication.object.as_deref() != Some(archive.sha256.as_str()))
                {
                    return Err(RailError::message(
                        "release publication progress has no matching sealed archive",
                    ));
                }
            }
        }

        let object = match &self.preparation {
            Preparation::Committing { tree } => Some(tree),
            Preparation::Complete { commit } => Some(commit),
            Preparation::Pending | Preparation::Writing => None,
        };
        if object.is_some_and(|object| {
            !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(RailError::message(
                "release preparation contains an invalid Git object identity",
            ));
        }
        Ok(())
    }

    fn validate_journal_path(&self, path: &Path) -> RailResult<()> {
        validate_journal_path(&self.transaction_id, path)
    }
}

pub(crate) fn require_successor(previous: &ReleaseState, next: &ReleaseState) -> RailResult<()> {
    super::review::require_successor(previous, next)?;
    super::hosted::require_successor(previous, next)?;
    if previous.intent.identity != next.intent.identity {
        return Err(RailError::message("cannot replace a persisted release intent"));
    }
    fn step(previous: &Step, next: &Step) -> bool {
        match previous.status {
            StepStatus::Pending => true,
            StepStatus::InProgress => next.status != StepStatus::Pending,
            StepStatus::Complete => next.status == StepStatus::Complete && previous.object == next.object,
        }
    }
    if previous.transaction_id != next.transaction_id
        || previous.remote_storage && !next.remote_storage
        || previous.phase > next.phase
        || previous.status != ReleaseStatus::Active && previous.status != next.status
        || serde_json::to_value(&previous.preparation)? != serde_json::to_value(&next.preparation)?
            && matches!(previous.preparation, Preparation::Complete { .. })
        || previous.package_seal.is_some()
            && serde_json::to_value(&previous.package_seal)? != serde_json::to_value(&next.package_seal)?
        || !previous.validation.is_empty() && previous.validation != next.validation
        || !previous.artifacts.is_empty() && previous.artifacts != next.artifacts
        || !step(&previous.commit_push, &next.commit_push)
        || !step(&previous.readiness, &next.readiness)
        || !step(&previous.tag_push, &next.tag_push)
        || !step(&previous.abort, &next.abort)
        || previous.crates.iter().zip(&next.crates).any(|(previous, next)| {
            previous.tag_object.is_some() && previous.tag_object != next.tag_object
                || previous.forge_draft.object.is_some() && previous.forge_draft.object != next.forge_draft.object
                || previous
                    .publication_attempt
                    .as_ref()
                    .is_some_and(|id| next.publication_attempt.as_ref() != Some(id))
                || !step(&previous.tag, &next.tag)
                || !step(&previous.publication, &next.publication)
                || !step(&previous.forge_draft, &next.forge_draft)
                || !step(&previous.forge_publication, &next.forge_publication)
                || !step(&previous.alias, &next.alias)
                || previous.alias.object.is_some() && previous.alias.object != next.alias.object
        })
    {
        return Err(RailError::message(
            "release progress conflicts with this executor; recover the latest retained record",
        ));
    }
    Ok(())
}

fn validate_journal_path(transaction_id: &str, path: &Path) -> RailResult<()> {
    let expected = format!("{transaction_id}.json");
    if path.file_name().is_none_or(|name| name != expected.as_str()) {
        return Err(RailError::message(format!(
            "release journal '{}' does not match transaction identity '{}'",
            path.display(),
            transaction_id
        )));
    }
    Ok(())
}

fn validate_transaction_id(transaction_id: &str) -> RailResult<()> {
    if transaction_id.len() > "release-".len()
        && transaction_id.len() <= 128
        && transaction_id.starts_with("release-")
        && transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Ok(());
    }
    Err(RailError::message(format!(
        "release state contains invalid transaction identity '{transaction_id}'"
    )))
}

pub(crate) fn normalize_release_paths(
    worktree_root: &Path,
    paths: &[PathBuf],
    kind: &str,
) -> RailResult<BTreeSet<PathBuf>> {
    paths
        .iter()
        .map(|path| normalize_release_path(worktree_root, path, kind))
        .collect()
}

pub(crate) fn normalize_release_path(worktree_root: &Path, path: &Path, kind: &str) -> RailResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(RailError::message(format!(
            "release state contains an empty {kind} path"
        )));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree_root.join(path)
    };
    let relative = crate::utils::path_relative_to(worktree_root, &absolute).map_err(|error| {
        RailError::message(format!(
            "release {kind} path '{}' escapes Git worktree '{}': {error}",
            path.display(),
            worktree_root.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Err(RailError::message(format!(
            "release {kind} path '{}' names the Git worktree root",
            path.display()
        )));
    }
    Ok(relative)
}

pub(crate) fn validate_state_path(root: &Path, path: &Path) -> RailResult<PathBuf> {
    let canonical = canonicalize_existing(path)?;
    let dir = canonicalize_existing(&state_dir(root))?;
    if canonical.parent().is_none_or(|parent| parent != dir) {
        return Err(RailError::message(format!(
            "release state '{}' is outside the workspace release-state directory",
            path.display()
        )));
    }
    Ok(canonical)
}

pub(crate) fn prepare_recovery(root: &Path, path: &Path) -> RailResult<()> {
    let _lock = lock(root)?;
    restore_preparation(root, path)
}

pub(crate) fn restore_preparation(root: &Path, path: &Path) -> RailResult<()> {
    let path = validate_state_path(root, path)?;
    let mut state = ReleaseState::load_for_recovery(&path)?;
    if state.status != ReleaseStatus::Active
        || !matches!(state.preparation, Preparation::Writing | Preparation::Committing { .. })
    {
        return Ok(());
    }
    let git = SystemGit::open(root)?;
    let git = SystemGit::open(&git.worktree_root)?;
    let expected_branch = if state.intent.review {
        super::review::branch(&state)
    } else {
        state.intent.branch.clone()
    };
    if git.current_branch()? != expected_branch {
        return Err(RailError::message(
            "release recovery requires its recorded branch before restoring files",
        ));
    }
    state.validate_recovery_paths(&git.worktree_root)?;
    if state.reconcile_preparation(&git)? {
        return state.save(&path);
    }

    let mut allowed = normalize_release_paths(&git.worktree_root, &state.intent.planned_paths, "planned")?;
    allowed.extend(normalize_release_paths(
        &git.worktree_root,
        &state.intent.control_paths,
        "control",
    )?);
    let unexpected = git
        .changed_paths()?
        .into_iter()
        .filter(|changed| !allowed.contains(changed))
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(RailError::with_help(
            format!(
                "release recovery found unrelated changes: {}",
                unexpected
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "restore unrelated work before resuming or aborting the release",
        ));
    }
    git.run_git(&["reset", "--hard", "HEAD"])?;
    for planned in normalize_release_paths(&git.worktree_root, &state.intent.planned_paths, "planned")? {
        let planned = planned
            .to_str()
            .ok_or_else(|| RailError::message(format!("release path '{}' is not valid UTF-8", planned.display())))?;
        git.run_git(&["clean", "-f", "--", planned])?;
    }
    for backup in &state.intent.local_input_backups {
        let relative = normalize_release_path(&git.worktree_root, &backup.path, "backup")?;
        crate::utils::write_file_atomic(&git.worktree_root.join(relative), backup.content.as_bytes())?;
    }
    state.save(&path)
}

pub(crate) fn lock(root: &Path) -> RailResult<std::fs::File> {
    let root = canonicalize_existing(root)?;
    let mut directory = root;
    for component in ["target", "cargo-rail", "releases"] {
        directory.push(component);
        match std::fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || canonicalize_existing(&directory)? != directory
        {
            return Err(RailError::message(
                "release state directory is not a contained real directory",
            ));
        }
    }
    let path = directory.join("execution.lock");
    let file = crate::utils::open_cache_lock_file(&path, true)?;
    if !crate::utils::private_file_matches_path(&file, &path, 0)? {
        return Err(RailError::message(
            "release execution lock is not a private regular file",
        ));
    }
    file.try_lock()
        .map_err(|_| RailError::message("another release operation is active in this checkout"))?;
    if !crate::utils::private_file_matches_path(&file, &path, 0)? {
        return Err(RailError::message(
            "release execution lock changed while acquiring authority",
        ));
    }
    Ok(file)
}

pub(crate) fn resolve_active(root: &Path, transaction: Option<&str>) -> RailResult<PathBuf> {
    if let Some(transaction) = transaction {
        validate_transaction_id(transaction)?;
        let path = state_dir(root).join(format!("{transaction}.json"));
        if !path.try_exists()? {
            return Err(RailError::with_help(
                format!("original release record for '{transaction}' is unavailable"),
                "recover the original records from the release executor; Git trailers cannot reconstruct release authority",
            ));
        }
        ReleaseState::load(&path)?;
        return Ok(path);
    }
    let directory = state_dir(root);
    let mut active = Vec::new();
    if directory.try_exists()? {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "json")
                && ReleaseState::load(&path)?.status == ReleaseStatus::Active
            {
                active.push(path);
            }
        }
    }
    match active.len() {
        1 => Ok(active.remove(0)),
        0 => Err(RailError::message("no active release transaction was found")),
        _ => Err(RailError::with_help(
            "multiple active release transactions were found",
            "pass the exact transaction ID shown by 'cargo rail release status'",
        )),
    }
}

pub(crate) fn state_dir(root: &Path) -> PathBuf {
    crate::workspace::cargo_rail_state_root(root).join("releases")
}

fn release_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| path.parent().unwrap_or(path))
}

fn complete_step(object: Option<String>) -> Step {
    Step {
        status: StepStatus::Complete,
        object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::planner::ReleaseSummary;

    fn fixture(schema_version: u32, plan_contract_version: u32) -> ReleaseState {
        let mut state = ReleaseState {
            schema_version,
            transaction_id: "release-test".to_string(),
            status: ReleaseStatus::Active,
            phase: ReleasePhase::Planned,
            remote_storage: false,
            review: None,
            executor: None,
            validation_dispatches: std::collections::BTreeMap::new(),
            intent: ReleaseIntent {
                identity: String::new(),
                source_tree: "a".repeat(40),
                plan: ReleasePlan {
                    plan_contract_version,
                    artifacts: Vec::new(),
                    snapshot_id: String::new(),
                    source: Default::default(),
                    canonical_crate_order: Vec::new(),
                    crates: Vec::new(),
                    summary: ReleaseSummary {
                        total_crates: 0,
                        crates_to_publish: 0,
                        crates_to_tag: 0,
                    },
                    change_files_to_delete: Vec::new(),
                    change_files_to_update: Vec::new(),
                    auxiliary_lockfiles: Vec::new(),
                    skipped: Vec::new(),
                },
                release_config: ReleaseConfig::default(),
                remote_repository: None,
                publish_registry: None,
                review: false,
                hosted: false,
                alias_previous: std::collections::BTreeMap::new(),
                skip_publish: true,
                skip_tag: true,
                initial_head: "b".repeat(40),
                branch: "main".to_string(),
                planned_paths: Vec::new(),
                control_paths: Vec::new(),
                local_input_backups: Vec::new(),
            },
            crates: Vec::new(),
            preparation: Preparation::Pending,
            package_seal: None,
            commit_push: Step::default(),
            readiness: Step::default(),
            validation: Vec::new(),
            artifacts: Vec::new(),
            tag_push: Step::default(),
            abort: Step::default(),
        };
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        state
    }

    fn load_fixture(state: &ReleaseState) -> RailResult<ReleaseState> {
        load_value(serde_json::to_value(state)?)
    }

    fn load_value(value: serde_json::Value) -> RailResult<ReleaseState> {
        let directory = tempfile::tempdir()?;
        let transaction_id = value["transaction_id"].as_str().unwrap_or("release-test");
        let path = directory.path().join(format!("{transaction_id}.json"));
        std::fs::write(&path, serde_json::to_vec(&value)?)?;
        ReleaseState::load(&path)
    }

    #[test]
    fn current_state_requires_exact_embedded_plan_contract() {
        load_fixture(&fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        for plan_contract in [4, RELEASE_PLAN_CONTRACT_VERSION + 1] {
            let error = load_fixture(&fixture(RELEASE_STATE_SCHEMA_VERSION, plan_contract)).unwrap_err();
            assert!(
                error.to_string().contains(&format!(
                    "requires embedded release plan contract {}",
                    RELEASE_PLAN_CONTRACT_VERSION
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn current_state_requires_auxiliary_projection_field() {
        let mut state =
            serde_json::to_value(fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        state["intent"]["plan"]
            .as_object_mut()
            .unwrap()
            .remove("auxiliary_lockfiles");
        let error = load_value(state).unwrap_err();
        assert!(error.to_string().contains("auxiliary_lockfiles"), "{error}");
    }

    #[test]
    fn preparation_rejects_removed_fields_and_incomplete_commit_bindings() {
        let base = serde_json::to_value(fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        for field in ["mode", "release_commit"] {
            let mut state = base.clone();
            state[field] = serde_json::json!("run");
            let error = load_value(state).unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
        }
        for preparation in [
            serde_json::json!({"status":"committing"}),
            serde_json::json!({"status":"complete"}),
            serde_json::json!({"status":"committing", "tree":"HEAD"}),
            serde_json::json!({"status":"complete", "commit":"HEAD"}),
            serde_json::json!({"status":"complete", "commit":"a".repeat(40), "tree":"b".repeat(40)}),
        ] {
            let mut state = base.clone();
            state["preparation"] = preparation.clone();
            assert!(load_value(state).is_err(), "accepted {preparation}");
        }
        let mut state = base;
        state["crates"] = serde_json::json!([{
            "name":"fixture", "commit":{}, "tag":{}, "forge_draft":{}, "publication":{}, "forge_publication":{}
        }]);
        let error = load_value(state).unwrap_err();
        assert!(error.to_string().contains("unknown field `commit`"), "{error}");
    }

    #[test]
    fn release_intent_normalizes_native_paths_before_validation() {
        let root = tempfile::tempdir().unwrap();
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        let native_path = root.path().join("crates").join("fixture").join("Cargo.toml");
        state.intent.planned_paths = vec![native_path.clone()];
        state.intent.plan.change_files_to_delete = vec![native_path];

        state.intent.make_portable(root.path()).unwrap();

        assert_eq!(
            state.intent.planned_paths[0].to_str().unwrap(),
            "crates/fixture/Cargo.toml"
        );
        assert_eq!(
            state.intent.plan.change_files_to_delete[0].to_str().unwrap(),
            "crates/fixture/Cargo.toml"
        );
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        load_fixture(&state).unwrap();
    }

    #[test]
    fn current_state_serializes_release_paths_portably() {
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.intent.planned_paths = vec![PathBuf::from(r"crates\fixture\Cargo.toml")];
        state.intent.control_paths = vec![PathBuf::from(r"release-notes\fixture-v0.1.1.md")];
        state.intent.local_input_backups = vec![LocalInputBackup {
            path: PathBuf::from(r".changes\fixture.md"),
            content: String::new(),
            restore: BackupRestorePolicy::BeforePreparation,
        }];
        state.intent.plan.change_files_to_delete = vec![PathBuf::from(r".changes\fixture.md")];

        let document = serde_json::to_value(state).unwrap();
        assert_eq!(
            document["intent"]["planned_paths"],
            serde_json::json!(["crates/fixture/Cargo.toml"])
        );
        assert_eq!(
            document["intent"]["control_paths"],
            serde_json::json!(["release-notes/fixture-v0.1.1.md"])
        );
        assert_eq!(
            document["intent"]["local_input_backups"][0]["path"],
            ".changes/fixture.md"
        );
        assert_eq!(
            document["intent"]["plan"]["change_files_to_delete"],
            serde_json::json!([".changes/fixture.md"])
        );
    }

    #[test]
    fn future_state_schema_is_rejected() {
        let error = load_fixture(&fixture(
            RELEASE_STATE_SCHEMA_VERSION + 1,
            RELEASE_PLAN_CONTRACT_VERSION,
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains(&format!(
                "unsupported release state version {}",
                RELEASE_STATE_SCHEMA_VERSION + 1
            )),
            "{error}"
        );
    }

    #[test]
    fn current_state_binds_transaction_identity_to_the_journal_filename() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        let mismatched = root.path().join("release-renamed.json");
        std::fs::write(&mismatched, serde_json::to_vec(&state).unwrap()).unwrap();

        let error = ReleaseState::load(&mismatched).unwrap_err();
        assert!(
            error.to_string().contains("does not match transaction identity"),
            "{error}"
        );
        let save_path = root.path().join("release-also-renamed.json");
        let error = state.save(&save_path).unwrap_err();
        assert!(
            error.to_string().contains("does not match transaction identity"),
            "{error}"
        );
        assert!(!save_path.exists());
    }

    #[test]
    fn current_state_rejects_invalid_transaction_identity_spelling() {
        for transaction in ["release_bad".to_owned(), format!("release-{}", "a".repeat(121))] {
            let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
            state.transaction_id = transaction;
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(format!("{}.json", state.transaction_id));
            let error = state.save(&path).unwrap_err();
            assert!(error.to_string().contains("invalid transaction identity"), "{error}");
            assert!(!path.exists());
        }
    }

    #[test]
    fn persisted_intent_cannot_be_replaced_even_with_a_valid_new_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = state_dir(root.path()).join("release-test.json");
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        state.intent.skip_tag = !state.intent.skip_tag;
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        let error = state.save(&path).unwrap_err();
        assert!(
            error.to_string().contains("cannot replace a persisted release intent"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn local_save_preserves_completed_effects_against_stale_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = state_dir(root.path()).join("release-test.json");
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.save(&path).unwrap();
        let mut stale = state.clone();
        state.preparation = Preparation::Complete { commit: "c".repeat(40) };
        state.phase = ReleasePhase::Prepared;
        state.commit_push = complete_step(Some("c".repeat(40)));
        state.save(&path).unwrap();
        let retained = std::fs::read(&path).unwrap();
        let error = stale.save(&path).unwrap_err();
        assert!(error.to_string().contains("progress conflicts"), "{error}");
        stale = state;
        stale.commit_push.object = Some("d".repeat(40));
        let error = stale.save(&path).unwrap_err();
        assert!(error.to_string().contains("progress conflicts"), "{error}");
        assert_eq!(std::fs::read(path).unwrap(), retained);
    }

    #[test]
    fn record_reader_rejects_omitted_defaults_and_unknown_nested_fields() {
        let base = serde_json::to_value(fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        for field in ["sign_tags", "changelog", "semver_check"] {
            let mut value = base.clone();
            value["intent"]["release_config"].as_object_mut().unwrap().remove(field);
            let error = load_value(value).unwrap_err();
            assert!(
                error.to_string().contains("complete current contract"),
                "{field}: {error}"
            );
        }
        let mut value = base;
        value["intent"]["remote_repository"] = serde_json::json!({
            "host": "github.com", "path": "example/repository", "authority": "publish"
        });
        let error = load_value(value).unwrap_err();
        assert!(error.to_string().contains("complete current contract"), "{error}");
    }

    #[test]
    fn record_validation_rejects_rehashed_inconsistent_registry_and_summary() {
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.intent.skip_publish = false;
        state.intent.publish_registry = Some("crates-io".to_owned());
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        let error = state.validate_contract().unwrap_err();
        assert!(
            error.to_string().contains("registry or repository authority"),
            "{error}"
        );
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.intent.plan.summary.total_crates = 1;
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        let error = state.validate_contract().unwrap_err();
        assert!(error.to_string().contains("selection or summary"), "{error}");
    }

    #[test]
    fn record_cannot_claim_completion_without_retained_effects() {
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.status = ReleaseStatus::Complete;
        state.phase = ReleasePhase::Released;
        state.preparation = Preparation::Complete { commit: "c".repeat(40) };
        let error = load_fixture(&state).unwrap_err();
        assert!(error.to_string().contains("terminal status contradicts"), "{error}");
    }

    #[test]
    fn execution_lock_rejects_a_second_owner_until_the_first_releases_it() {
        let root = tempfile::tempdir().unwrap();
        let owner = lock(root.path()).unwrap();
        let error = lock(root.path()).unwrap_err();
        assert!(
            error.to_string().contains("another release operation is active"),
            "{error}"
        );
        drop(owner);
        let next = lock(root.path()).unwrap();
        assert_eq!(next.metadata().unwrap().len(), 0);
    }

    #[test]
    fn current_recovery_rejects_an_escaping_control_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.intent.control_paths = vec![PathBuf::from("../outside-plan.json")];
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        let path = directory.join("release-test.json");
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("path"), "{error}");
    }

    #[test]
    fn current_recovery_rejects_an_absolute_in_worktree_control_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let control = root.path().join("release-plan.json");
        std::fs::write(&control, "{}\n").unwrap();
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.intent.control_paths = vec![control];
        state.intent.identity = state.intent.identity_for(&state.transaction_id).unwrap();
        let path = directory.join("release-test.json");
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("path"), "{error}");
    }
}
