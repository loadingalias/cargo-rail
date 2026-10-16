//! Release planning and dry-run analysis

use crate::change_detection::changed_code_crate_names;
use crate::config::{
    ChangelogConfig, ChangelogFilters, ChangelogRelativeTo, CommitPolicy, ReleaseConfig, ReleaseSource,
};
use crate::error::{RailError, RailResult};
use crate::release::attribution::{AttributedCommit, AttributedHistory, CommitAttributor};
use crate::release::change_files::{ChangeBump, ChangeIntent, ChangePresentation, PendingChangeSet};
use crate::release::changelog::{ChangelogSpec, CommitDiagnostic, CommitRef, detect_github_repo, resolve_links};
use crate::release::presentation::{self, PlannedPresentation};
use crate::release::process;
use crate::release::version::{BumpLevel, BumpRequest, commit_bump_level};
use crate::workspace::WorkspaceContext;
use cargo_metadata::PackageId;
use rustc_hash::{FxHashMap, FxHashSet};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) const RELEASE_PLAN_CONTRACT_VERSION: u32 = 10;

/// A plan for releasing one or more crates
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePlan {
    /// Contract version for release plan schema.
    pub plan_contract_version: u32,
    /// Workspace snapshot shared with planner, split, and sync ownership.
    pub snapshot_id: String,
    /// Selected release intent source.
    #[serde(default)]
    pub source: ReleaseSource,
    /// Canonical crate order (dependency order).
    pub canonical_crate_order: Vec<String>,
    /// Crates to release in dependency order
    pub crates: Vec<CrateReleasePlan>,
    /// Summary statistics
    pub summary: ReleaseSummary,
    /// Pending change files consumed by this release plan.
    #[serde(serialize_with = "super::path_serde::serialize_vec")]
    pub change_files_to_delete: Vec<PathBuf>,
    /// Pending change files rewritten to retain unreleased no-release intent.
    pub change_files_to_update: Vec<PlannedChangeFileUpdate>,
    /// Exact Cargo manifest and lockfile writes validated in isolated captured source.
    ///
    /// Empty until [`ReleasePlanner::project_writes`] runs; the projection resolves Cargo
    /// in an isolated copy, so the preview does not wait for it.
    pub cargo_writes: Vec<PlannedCargoWrite>,
    /// Target crates excluded from the plan, with the reason (auto mode only).
    pub skipped: Vec<SkippedCrate>,
}

/// A target crate excluded from an auto-bump release plan
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedCrate {
    /// Crate name
    pub name: String,
    /// Why the crate was skipped
    pub reason: String,
}

/// Outcome of planning one crate
enum CratePlanOutcome {
    /// The crate is part of the release
    Planned(Box<CrateReleasePlan>),
    /// The crate was excluded, with a trace reason
    Skipped(SkippedCrate),
}

struct VersionGroupForce {
    name: String,
    level: BumpLevel,
}

/// Release plan for a single crate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrateReleasePlan {
    /// Crate name
    pub name: String,
    /// Current version
    pub current_version: Version,
    /// New version after bump
    pub new_version: Version,
    /// Path to Cargo.toml
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub manifest_path: PathBuf,
    /// Path to CHANGELOG.md
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub changelog_path: PathBuf,
    /// Git tag name for this release
    pub tag_name: String,
    /// Previous matching release tag (if any)
    pub previous_tag: Option<String>,
    /// Changelog range start ref (if any)
    pub changelog_range_start: Option<String>,
    /// Changelog range end ref
    pub changelog_range_end: String,
    /// Whether the release publishes this crate to the configured registry
    pub publish: bool,
    /// The strongest bump that reviewed intent or conventional commits request, which
    /// API checks compare against.
    #[serde(skip)]
    pub(crate) reviewed_level: Option<BumpLevel>,
    /// Whether to generate changelog
    pub generate_changelog: bool,
    /// Canonical bump expression `old -> new`.
    pub bump: String,
    /// Why this crate is in the plan.
    pub bump_reason: String,
    /// Rendered changelog body for the planned version, excluding the version header.
    pub changelog_body: String,
    /// Exact insertion and forge prose, required in serialized plans; filled after intent resolution.
    #[serde(deserialize_with = "required_presentation")]
    pub presentation: Option<PlannedPresentation>,
    /// Attributed commits supplying intent in commit workflows.
    #[serde(default)]
    pub commits: Vec<PlannedCommit>,
    /// Selected commit-policy diagnostics, before presentation filters.
    #[serde(default)]
    pub commit_diagnostics: Vec<CommitDiagnostic>,
    #[serde(skip)]
    presentation_policy: Option<ChangelogSpec>,
    /// Dependency update entries synthesized for dependent-closure releases.
    pub dependency_updates: Vec<DependencyUpdate>,
    /// Intent-file entries consumed for this crate.
    pub change_entries: Vec<PlannedChangeEntry>,
    /// Version group that affected this crate, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_group: Option<String>,
    /// Dependents that will need version updates
    pub affected_dependents: Vec<String>,
    /// Internal auto-bump level used for version-group propagation.
    #[serde(skip)]
    auto_bump_level: Option<BumpLevel>,
    /// Synthesized changelog entry for group-only releases.
    #[serde(skip)]
    version_group_entry: Option<String>,
}

fn required_presentation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PlannedPresentation>, D::Error> {
    PlannedPresentation::deserialize(deserializer).map(Some)
}

/// One commit included in a crate release plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedCommit {
    /// Full commit SHA.
    pub sha: String,
    /// Commit subject line.
    pub subject: String,
    /// Commit body, if present.
    pub body: Option<String>,
    /// Parsed conventional commit type.
    pub commit_type: Option<String>,
    /// Parsed conventional commit scope.
    pub scope: Option<String>,
    /// Whether the commit declares a breaking change.
    pub breaking: bool,
    /// Semver bump contribution from this commit.
    pub bump: Option<String>,
}

/// A synthesized dependency update entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyUpdate {
    /// Dependency crate name.
    pub name: String,
    /// Dependency version after this release plan.
    pub version: Version,
}

/// One change-file entry included in a crate release plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedChangeEntry {
    /// Source change file path.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    /// Requested bump level.
    pub bump: ChangeBump,
    /// User-facing changelog body.
    pub body: String,
    /// Explicit authored section and Markdown rendering rule.
    #[serde(default)]
    pub presentation: ChangePresentation,
}

/// Exact retained content for a partially consumed change file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedChangeFileUpdate {
    /// Change file rewritten by the release commit.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    /// Canonical content containing only retained no-release intents.
    pub content: String,
}

/// One exact Cargo manifest or lockfile write.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedCargoWrite {
    /// Workspace-relative Cargo.toml or Cargo.lock path.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    /// SHA-256 of captured bytes; absent only when creating Cargo.lock.
    pub before_digest: Option<String>,
    /// SHA-256 of the exact post-release bytes.
    pub after_digest: String,
    /// Exact UTF-8 content applied without resolving Cargo again.
    pub content: String,
}

/// Summary statistics for a release plan
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSummary {
    /// Total number of crates in the plan
    pub total_crates: usize,
    /// Number of crates that will be published
    pub crates_to_publish: usize,
    /// Number of crates that will be tagged
    pub crates_to_tag: usize,
}

#[derive(Default)]
struct PlanningInputs {
    pending: PendingChangeSet,
    histories: HashMap<HistoryKey, AttributedHistory>,
    changed_code: HashMap<Option<String>, FxHashSet<String>>,
    head: String,
}

/// Release planner
#[derive(Debug)]
pub struct ReleasePlanner<'a> {
    /// Workspace context
    ctx: &'a WorkspaceContext,
    /// Release configuration
    release_config: &'a ReleaseConfig,
}

impl<'a> ReleasePlanner<'a> {
    /// Create a new release planner
    pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
        Self { ctx, release_config }
    }

    /// Build a release plan
    ///
    /// Uses dependency order for deterministic crate sequencing and computes
    /// per-crate publish/tag/changelog intent.
    pub fn plan(&self, crate_names: Option<Vec<String>>, bump_request: &BumpRequest) -> RailResult<ReleasePlan> {
        if matches!(bump_request, BumpRequest::Auto)
            && self.release_config.source.uses_commits()
            && is_shallow_repository(self.ctx.workspace_root())?
        {
            return Err(RailError::with_help(
                "automatic commit releases require complete history",
                "fetch full history and tags before selecting an automatic commit bump",
            ));
        }
        let ordered_targets = self.resolve_targets(crate_names)?;
        self.validate_touched_version_groups(&ordered_targets)?;

        let mut crate_plans = Vec::with_capacity(ordered_targets.len());
        let mut version_map: FxHashMap<String, Version> = FxHashMap::default();
        let mut inputs = self.planning_inputs()?;

        let mut skipped = Vec::new();
        for crate_name in &ordered_targets {
            match self.plan_crate(crate_name, bump_request, &version_map, &mut inputs, None)? {
                CratePlanOutcome::Planned(plan) => {
                    version_map.insert(crate_name.clone(), plan.new_version.clone());
                    crate_plans.push(*plan);
                }
                CratePlanOutcome::Skipped(skip) => skipped.push(skip),
            }
        }

        if matches!(bump_request, BumpRequest::Auto) {
            self.apply_version_groups(
                &ordered_targets,
                &mut crate_plans,
                &mut skipped,
                &mut version_map,
                &mut inputs,
            )?;
        }

        for plan in &mut crate_plans {
            let package_id = self.workspace_package_id(&plan.name)?;
            plan.affected_dependents = self
                .ctx
                .graph()
                .direct_dependents_by_id(package_id)?
                .into_iter()
                .collect();
        }

        let crates_to_publish = crate_plans.iter().filter(|p| p.publish).count();
        let summary = ReleaseSummary {
            total_crates: crate_plans.len(),
            crates_to_publish,
            crates_to_tag: crate_plans.len(),
        };
        let canonical_crate_order = crate_plans.iter().map(|plan| plan.name.clone()).collect();
        let mut selected_change_files: Vec<PathBuf> = crate_plans
            .iter()
            .flat_map(|plan| plan.change_entries.iter().map(|entry| entry.path.clone()))
            .collect();
        selected_change_files.sort();
        selected_change_files.dedup();

        // Release-worthy entries are consumed atomically. No-release entries for
        // crates outside the plan remain as reviewed coverage for their next
        // actual release boundary.
        let planned_names: HashSet<&str> = crate_plans.iter().map(|plan| plan.name.as_str()).collect();
        let consumption = inputs
            .pending
            .plan_consumption(&selected_change_files, &planned_names)?;
        let change_files_to_update = consumption
            .update
            .into_iter()
            .map(|(path, content)| PlannedChangeFileUpdate { path, content })
            .collect();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let github = detect_github_repo(self.ctx.workspace_root());
        for plan in &mut crate_plans {
            let overrides = self
                .ctx
                .config()
                .and_then(|config| config.crates.get(&plan.name))
                .and_then(|config| config.changelog.as_ref());
            if plan.generate_changelog {
                let spec = plan
                    .presentation_policy
                    .take()
                    .ok_or_else(|| RailError::message("release plan has no resolved presentation policy"))?;
                let links = if self.release_config.source.uses_commits() {
                    resolve_links(&self.release_config.changelog, overrides, self.ctx.workspace_root())?
                } else {
                    Default::default()
                };
                let commits = plan.commits.iter().map(PlannedCommit::as_ref).collect::<Vec<_>>();
                let highlights = plan.version_group_entry.iter().cloned().collect::<Vec<_>>();
                let dependencies = plan
                    .dependency_updates
                    .iter()
                    .map(|update| (update.name.clone(), update.version.clone()))
                    .collect::<Vec<_>>();
                // Breaking entries lead, so the notes show upgrade steps first. The sort is stable,
                // so file order holds within one bump level.
                let mut change_entries = plan.change_entries.iter().collect::<Vec<_>>();
                change_entries.sort_by_key(|entry| std::cmp::Reverse(entry.bump.release_level()));
                let mut rendered_highlights = highlights;
                rendered_highlights.extend(
                    change_entries
                        .into_iter()
                        .map(|entry| entry.presentation.render(&entry.body)),
                );
                plan.changelog_body = spec.render(&links, &rendered_highlights, &commits, &dependencies);
            }
            plan.presentation = Some(presentation::capture(
                self.ctx.workspace_root(),
                plan,
                &date,
                github.as_ref(),
            )?);
        }
        Ok(ReleasePlan {
            plan_contract_version: RELEASE_PLAN_CONTRACT_VERSION,
            snapshot_id: self.ctx.snapshot()?.id().to_string(),
            source: self.release_config.source,
            canonical_crate_order,
            crates: crate_plans,
            summary,
            change_files_to_delete: consumption.delete,
            change_files_to_update,
            cargo_writes: Vec::new(),
            skipped,
        })
    }

    /// Fill `plan.cargo_writes` with the exact manifest and lockfile bytes after the release.
    pub fn project_writes(&self, plan: &mut ReleasePlan) -> RailResult<()> {
        plan.cargo_writes = crate::release::cargo_projection::plan_writes(
            self.ctx,
            &plan.crates,
            &self.release_config.auxiliary_cargo_manifests,
        )?;
        Ok(())
    }

    fn planning_inputs(&self) -> RailResult<PlanningInputs> {
        let pending = if self.release_config.source.uses_changes() {
            PendingChangeSet::load(
                self.ctx.workspace_root(),
                &self.release_config.change_dir,
                self.ctx.graph().workspace_members(),
            )?
        } else {
            PendingChangeSet::default()
        };
        Ok(PlanningInputs {
            pending,
            head: self.ctx.git()?.git().head_commit()?,
            ..Default::default()
        })
    }

    fn changed_code_crates<'b>(
        &self,
        base: Option<String>,
        cache: &'b mut HashMap<Option<String>, FxHashSet<String>>,
    ) -> RailResult<&'b FxHashSet<String>> {
        if !cache.contains_key(&base) {
            let crates = changed_code_crate_names(self.ctx, base.as_deref())?;
            cache.insert(base.clone(), crates);
        }
        cache
            .get(&base)
            .ok_or_else(|| RailError::message("internal error: missing cached release coverage"))
    }

    /// Plan release for a single crate
    fn plan_crate(
        &self,
        crate_name: &str,
        bump_request: &BumpRequest,
        version_map: &FxHashMap<String, Version>,
        inputs: &mut PlanningInputs,
        version_group_force: Option<&VersionGroupForce>,
    ) -> RailResult<CratePlanOutcome> {
        let package = self
            .ctx
            .cargo()
            .get_package(crate_name)
            .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

        let manifest_path = package.manifest_path.clone().into_std_path_buf();

        let current_version = package.version.clone();

        let previous_tag = self.find_previous_tag(crate_name)?;

        let crate_config = self.ctx.config().and_then(|c| c.crates.get(crate_name));

        let changelog_config = crate_config.and_then(|c| c.changelog.as_ref());
        let changelog_path = self.resolve_changelog_path(&manifest_path, changelog_config)?;

        let generate_changelog = !changelog_config.is_some_and(|changelog_cfg| changelog_cfg.skip);

        let spec = ChangelogSpec::resolve(&self.release_config.changelog, changelog_config)?;
        let (commits, commit_diagnostics, has_code_changes) = if self.release_config.source.uses_commits() {
            let filters = changelog_config
                .and_then(|config| config.filters.as_ref())
                .unwrap_or(&self.release_config.changelog.filters);
            let history = history_for_range(
                &mut inputs.histories,
                &CommitAttributor::new(self.ctx),
                previous_tag.clone(),
                &inputs.head,
                filters,
            )?;
            let commits = planned_commits(&history.for_crate(crate_name).collect::<Vec<_>>());
            let diagnostics = if self.release_config.unconventional_commits == CommitPolicy::Allow {
                Vec::new()
            } else {
                spec.diagnose(&history.commit_refs_for_crate(crate_name))
            };
            (commits, diagnostics, history.has_code_changes(crate_name))
        } else {
            (
                Vec::new(),
                Vec::new(),
                self.changed_code_crates(previous_tag.clone(), &mut inputs.changed_code)?
                    .contains(crate_name),
            )
        };
        if self.release_config.unconventional_commits == CommitPolicy::Deny && !commit_diagnostics.is_empty() {
            return Err(RailError::with_help(
                format!(
                    "commit diagnostics failed for {crate_name}: {}",
                    commit_diagnostics
                        .iter()
                        .map(CommitDiagnostic::describe)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                "rewrite the commit subjects or select release.unconventional_commits = 'warn' or 'allow'",
            ));
        }
        if self.release_config.requires_change_file(crate_name)
            && !inputs.pending.covers(crate_name)
            && has_code_changes
        {
            return Err(RailError::with_help(
                format!(
                    "missing change files for {crate_name}: code changes require {} coverage",
                    self.release_config.change_dir
                ),
                format!("add reviewed release intent under {}", self.release_config.change_dir),
            ));
        }
        let inferred_bump = commits
            .iter()
            .filter_map(|commit| commit.bump.as_deref().and_then(|bump| bump.parse::<BumpLevel>().ok()))
            .max();
        let dependency_updates = self.dependency_updates(package, version_map);
        let change_entries = if self.release_config.source.uses_changes() {
            planned_change_entries(inputs.pending.for_crate(crate_name))
        } else {
            Vec::new()
        };
        let change_bump = change_entries
            .iter()
            .filter_map(|entry| entry.bump.release_level())
            .max();

        // Cargo's registry allowlist is the hard ceiling. Repository policy
        // may narrow that authority but can never widen it.
        let publish = self
            .release_config
            .publish
            .registry()
            .is_some_and(|registry| crate::workspace::CargoState::package_allows_registry(package, registry))
            && crate_config
                .and_then(|c| c.release.as_ref())
                .map(|r| r.publish)
                .unwrap_or(true);

        let explicit_signal = [change_bump, inferred_bump].into_iter().flatten().max();
        let natural_level = explicit_signal.or_else(|| (!dependency_updates.is_empty()).then_some(BumpLevel::Patch));

        let (bump_type, bump_reason, planned_auto_level) = match bump_request {
            BumpRequest::Explicit(bump_type) => (bump_type.clone(), "explicit".to_string(), None),
            BumpRequest::Auto => {
                let (level, mut reason) = if let Some(force) = version_group_force {
                    (force.level, format!("version group {} -> {}", force.name, force.level))
                } else if let Some(level) = natural_level {
                    (
                        level,
                        auto_bump_reason(level, change_bump, inferred_bump, !dependency_updates.is_empty()),
                    )
                } else {
                    let since = previous_tag
                        .as_deref()
                        .map(|tag| format!("since {}", tag))
                        .unwrap_or_else(|| "with no previous tag: full history".to_string());
                    return Ok(CratePlanOutcome::Skipped(SkippedCrate {
                        name: crate_name.to_string(),
                        reason: format!(
                            "auto: no release-worthy changes {} ({})",
                            since,
                            no_release_signal_reason(self.release_config.source)
                        ),
                    }));
                };
                if previous_tag.is_none() && version_group_force.is_none() {
                    reason = format!("{} (no previous tag: full history)", reason);
                }
                (
                    level.to_bump_type(&current_version, self.release_config.pre_1_breaking_bump),
                    reason,
                    Some(level),
                )
            }
        };

        let new_version = bump_type.apply(&current_version);

        let tag_name = self.format_tag(crate_name, &new_version);

        let version_group_entry = version_group_force.and_then(|force| {
            natural_level
                .is_none()
                .then(|| format!("- synchronized version with version group {}.", force.name))
        });
        let bump = format!("{} -> {}", current_version, new_version);
        Ok(CratePlanOutcome::Planned(Box::new(CrateReleasePlan {
            name: crate_name.to_string(),
            current_version,
            new_version,
            manifest_path,
            changelog_path,
            tag_name,
            previous_tag: previous_tag.clone(),
            changelog_range_start: previous_tag,
            changelog_range_end: inputs.head.clone(),
            publish,
            reviewed_level: explicit_signal,
            generate_changelog,
            bump,
            bump_reason,
            changelog_body: String::new(),
            presentation: None,
            commits,
            commit_diagnostics,
            presentation_policy: Some(spec),
            dependency_updates,
            change_entries,
            version_group: version_group_force.map(|force| force.name.clone()),
            affected_dependents: Vec::new(),
            auto_bump_level: planned_auto_level,
            version_group_entry,
        })))
    }

    fn resolve_changelog_path(
        &self,
        manifest_path: &std::path::Path,
        changelog_config: Option<&ChangelogConfig>,
    ) -> RailResult<PathBuf> {
        let changelog_relative_path = changelog_config
            .and_then(|ch| ch.path.as_ref())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.release_config.changelog.path.clone());
        let relative_to = changelog_config
            .and_then(|ch| ch.relative_to)
            .unwrap_or(self.release_config.changelog.relative_to);

        let base = match relative_to {
            ChangelogRelativeTo::Crate => manifest_path
                .parent()
                .ok_or_else(|| RailError::message("Invalid manifest path"))?,
            ChangelogRelativeTo::Workspace => self.ctx.workspace_root(),
        };
        changelog_path(base, Path::new(&changelog_relative_path))
    }

    fn apply_version_groups(
        &self,
        ordered_targets: &[String],
        crate_plans: &mut Vec<CrateReleasePlan>,
        skipped: &mut Vec<SkippedCrate>,
        version_map: &mut FxHashMap<String, Version>,
        inputs: &mut PlanningInputs,
    ) -> RailResult<()> {
        if self.release_config.version_groups.is_empty() {
            return Ok(());
        }

        let mut group_levels: HashMap<String, BumpLevel> = HashMap::new();
        for plan in crate_plans.iter() {
            if let Some((group_name, _)) = self.version_group_for(&plan.name)
                && let Some(level) = plan.auto_bump_level
            {
                group_levels
                    .entry(group_name.to_string())
                    .and_modify(|current| *current = (*current).max(level))
                    .or_insert(level);
            }
        }

        if group_levels.is_empty() {
            return Ok(());
        }

        let forced: Vec<(String, VersionGroupForce)> = skipped
            .iter()
            .filter_map(|skip| {
                let (group_name, _) = self.version_group_for(&skip.name)?;
                let level = *group_levels.get(group_name)?;
                Some((
                    skip.name.clone(),
                    VersionGroupForce {
                        name: group_name.to_string(),
                        level,
                    },
                ))
            })
            .collect();

        let forced_names: HashSet<&str> = forced.iter().map(|(name, _)| name.as_str()).collect();
        skipped.retain(|skip| !forced_names.contains(skip.name.as_str()));

        for (crate_name, force) in forced {
            match self.plan_crate(&crate_name, &BumpRequest::Auto, version_map, inputs, Some(&force))? {
                CratePlanOutcome::Planned(plan) => {
                    version_map.insert(crate_name, plan.new_version.clone());
                    crate_plans.push(*plan);
                }
                CratePlanOutcome::Skipped(_) => {}
            }
        }

        for plan in crate_plans.iter_mut() {
            if plan.version_group.is_some() {
                continue;
            }
            let Some((group_name, _)) = self.version_group_for(&plan.name) else {
                continue;
            };
            let Some(level) = group_levels.get(group_name).copied() else {
                continue;
            };
            let current_level = plan.auto_bump_level.unwrap_or(level);
            if level > current_level {
                let bump_type = level.to_bump_type(&plan.current_version, self.release_config.pre_1_breaking_bump);
                plan.new_version = bump_type.apply(&plan.current_version);
                plan.bump = format!("{} -> {}", plan.current_version, plan.new_version);
                plan.tag_name = self.format_tag(&plan.name, &plan.new_version);
                plan.auto_bump_level = Some(level);
            }
            plan.bump_reason = format!("{}; version group {} -> {}", plan.bump_reason, group_name, level);
            plan.version_group = Some(group_name.to_string());
        }

        let order: HashMap<&str, usize> = ordered_targets
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.as_str(), idx))
            .collect();
        crate_plans.sort_by_key(|plan| order.get(plan.name.as_str()).copied().unwrap_or(usize::MAX));

        version_map.clear();
        for plan in crate_plans.iter() {
            version_map.insert(plan.name.clone(), plan.new_version.clone());
        }
        self.refresh_dependency_updates(crate_plans, version_map)
    }

    fn refresh_dependency_updates(
        &self,
        crate_plans: &mut [CrateReleasePlan],
        version_map: &FxHashMap<String, Version>,
    ) -> RailResult<()> {
        for plan in crate_plans {
            let package = self
                .ctx
                .cargo()
                .get_package(&plan.name)
                .ok_or_else(|| RailError::message(format!("Crate '{}' not found", plan.name)))?;
            let dependency_updates = self.dependency_updates(package, version_map);
            if dependency_updates == plan.dependency_updates {
                continue;
            }
            plan.dependency_updates = dependency_updates;
        }
        Ok(())
    }

    fn dependency_updates(
        &self,
        package: &cargo_metadata::Package,
        version_map: &FxHashMap<String, Version>,
    ) -> Vec<DependencyUpdate> {
        let mut updates: Vec<_> = package
            .dependencies
            .iter()
            .filter_map(|dep| {
                version_map.get(dep.name.as_str()).map(|version| DependencyUpdate {
                    name: dep.name.clone(),
                    version: version.clone(),
                })
            })
            .collect();
        updates.sort_by(|a, b| a.name.cmp(&b.name));
        updates.dedup_by(|a, b| a.name == b.name);
        updates
    }

    /// Format git tag name for a crate from its `{crate}` and `{version}` template.
    pub(crate) fn format_tag(&self, crate_name: &str, version: &Version) -> String {
        self.format_tag_value(crate_name, &version.to_string())
    }

    fn format_tag_value(&self, crate_name: &str, version: &str) -> String {
        let format = self
            .ctx
            .config()
            .and_then(|config| config.release_tag_format_override(crate_name))
            .unwrap_or(&self.release_config.tag_format);
        crate::config::render_tag_template(format, crate_name, version)
    }

    fn find_previous_tag(&self, crate_name: &str) -> RailResult<Option<String>> {
        let pattern = self.format_tag_value(crate_name, "*");

        self.ctx.git()?.git().find_latest_tag(&pattern)
    }

    /// The selected crates and every published dependent and version-group member they need,
    /// in publication order. No selection means every crate the release manages.
    pub(crate) fn resolve_targets(&self, crate_names: Option<Vec<String>>) -> RailResult<Vec<String>> {
        let all_ordered = self
            .ctx
            .graph()
            .publish_order()?
            .into_iter()
            .filter(|name| self.release_config.is_release_crate(name))
            .collect::<Vec<_>>();
        let Some(targets) = crate_names else {
            return Ok(all_ordered);
        };
        for crate_name in &targets {
            self.workspace_package_id(crate_name)?;
            if !self.release_config.is_release_crate(crate_name) {
                return Err(RailError::with_help(
                    format!("'{crate_name}' is not a crate this release manages"),
                    "add it to release.crates, or select a listed crate",
                ));
            }
        }
        let mut selected: HashSet<String> = targets.into_iter().collect();
        loop {
            let before = selected.len();
            self.expand_selected_version_groups(&mut selected);
            let selected_ids = self.workspace_package_ids(&selected)?;
            selected.extend(self.published_dependents(&selected_ids)?);
            if selected.len() == before {
                break;
            }
        }
        Ok(all_ordered.into_iter().filter(|name| selected.contains(name)).collect())
    }

    fn workspace_package_id(&self, crate_name: &str) -> RailResult<&PackageId> {
        self.ctx
            .graph()
            .workspace_package_by_name(crate_name)
            .map(|package| &package.id)
    }

    fn published_dependents(&self, selected: &HashSet<PackageId>) -> RailResult<HashSet<String>> {
        Ok(self
            .ctx
            .graph()
            .transitive_dependents_of_ids(selected)?
            .into_iter()
            .filter(|name| {
                self.release_config.is_release_crate(name)
                    && self
                        .ctx
                        .cargo()
                        .get_package(name)
                        .is_some_and(crate::workspace::CargoState::is_package_publishable)
            })
            .collect())
    }

    fn workspace_package_ids(&self, crate_names: &HashSet<String>) -> RailResult<HashSet<PackageId>> {
        crate_names
            .iter()
            .map(|crate_name| self.workspace_package_id(crate_name).cloned())
            .collect()
    }

    fn version_group_for(&self, crate_name: &str) -> Option<(&str, &[String])> {
        self.release_config
            .version_groups
            .iter()
            .find(|(_, members)| members.iter().any(|member| member == crate_name))
            .map(|(name, members)| (name.as_str(), members.as_slice()))
    }

    fn expand_selected_version_groups(&self, selected: &mut HashSet<String>) {
        let touched_groups: Vec<_> = self
            .release_config
            .version_groups
            .iter()
            .filter(|(_, members)| members.iter().any(|member| selected.contains(member)))
            .map(|(_, members)| members.clone())
            .collect();
        for members in touched_groups {
            selected.extend(members);
        }
    }

    fn validate_touched_version_groups(&self, ordered_targets: &[String]) -> RailResult<()> {
        let selected: HashSet<&str> = ordered_targets.iter().map(String::as_str).collect();
        for (group_name, members) in &self.release_config.version_groups {
            if !members.iter().any(|member| selected.contains(member.as_str())) {
                continue;
            }
            let mut versions: Vec<_> = members
                .iter()
                .filter_map(|member| {
                    self.ctx
                        .cargo()
                        .get_package(member)
                        .map(|package| (member.as_str(), package.version.clone()))
                })
                .collect();
            versions.sort_by(|a, b| a.0.cmp(b.0));
            let Some((_, first_version)) = versions.first() else {
                continue;
            };
            if versions.iter().all(|(_, version)| version == first_version) {
                continue;
            }
            let detail = versions
                .iter()
                .map(|(name, version)| format!("{}={}", name, version))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(RailError::with_help(
                format!(
                    "version group '{}' has mismatched current versions: {}",
                    group_name, detail
                ),
                "align member versions before enabling lockstep releases",
            ));
        }
        Ok(())
    }
}

/// Join a configured changelog path to its base directory.
///
/// The release reads and rewrites this exact file. A link at the file, or at a directory between
/// `base` and the file, would redirect both to a path that configuration does not name.
pub(crate) fn changelog_path(base: &Path, configured: &Path) -> RailResult<PathBuf> {
    let path = base.join(configured);
    let link = crate::utils::first_linked_entry(base, configured).map_err(|error| {
        RailError::message(format!(
            "failed to inspect changelog path '{}': {error}",
            path.display()
        ))
    })?;
    if let Some(link) = link {
        return Err(RailError::with_help(
            format!("release changelog path '{}' is a symbolic link", link.display()),
            "name the changelog file itself in release.changelog.path",
        ));
    }
    Ok(path)
}

/// Determine whether git history is shallow using git's own repository view.
pub(crate) fn is_shallow_repository(workspace_root: &Path) -> RailResult<bool> {
    let output = process::run("git", &["rev-parse", "--is-shallow-repository"], Some(workspace_root))?;
    if !output.status.success() {
        return Err(RailError::message("failed to determine whether repository is shallow"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn planned_change_entries(intents: &[ChangeIntent]) -> Vec<PlannedChangeEntry> {
    intents
        .iter()
        .map(|intent| PlannedChangeEntry {
            path: intent.path.clone(),
            bump: intent.bump,
            body: intent.body.clone(),
            presentation: intent.presentation.clone(),
        })
        .collect()
}

fn auto_bump_reason(
    level: BumpLevel,
    change_bump: Option<BumpLevel>,
    inferred_bump: Option<BumpLevel>,
    has_dependency_updates: bool,
) -> String {
    if change_bump.is_some() {
        format!("auto: reviewed change files -> {}", level)
    } else if inferred_bump.is_some() {
        format!("auto: conventional commits -> {}", level)
    } else if has_dependency_updates {
        "auto: dependent closure -> patch".to_string()
    } else {
        format!("auto: {}", level)
    }
}

fn no_release_signal_reason(source: ReleaseSource) -> &'static str {
    match source {
        ReleaseSource::Changes => "no reviewed release intent or dependency updates",
        ReleaseSource::Commits => "no qualifying conventional commits or dependency updates",
        ReleaseSource::Both => "no qualifying reviewed intent, conventional commits, or dependency updates",
    }
}

fn history_for_range<'a>(
    histories: &'a mut HashMap<HistoryKey, AttributedHistory>,
    attributor: &CommitAttributor<'_>,
    from: Option<String>,
    to: &str,
    filters: &ChangelogFilters,
) -> RailResult<&'a AttributedHistory> {
    let key = HistoryKey::new(from, filters);
    if !histories.contains_key(&key) {
        let history = attributor.history_with_filters(key.from.as_deref(), to, Some(filters))?;
        histories.insert(key.clone(), history);
    }
    histories
        .get(&key)
        .ok_or_else(|| RailError::message("missing captured release history"))
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HistoryKey {
    from: Option<String>,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
}

impl HistoryKey {
    fn new(from: Option<String>, filters: &ChangelogFilters) -> Self {
        let (include_paths, exclude_paths) = (filters.include_paths.clone(), filters.exclude_paths.clone());
        Self {
            from,
            include_paths,
            exclude_paths,
        }
    }
}

fn planned_commits(commits: &[&AttributedCommit]) -> Vec<PlannedCommit> {
    commits
        .iter()
        .map(|commit| {
            let parsed = crate::release::changelog::parse_subject(&commit.subject, commit.body.as_deref());
            let bump = commit_bump_level(&parsed).map(|level| level.as_str().to_string());
            PlannedCommit {
                sha: commit.sha.clone(),
                subject: commit.subject.clone(),
                body: commit.body.clone(),
                commit_type: parsed.commit_type.map(str::to_string),
                scope: parsed.scope.map(str::to_string),
                breaking: parsed.breaking,
                bump,
            }
        })
        .collect()
}

impl PlannedCommit {
    fn as_ref(&self) -> CommitRef<'_> {
        CommitRef {
            sha: &self.sha,
            subject: &self.subject,
            body: self.body.as_deref(),
        }
    }
}
