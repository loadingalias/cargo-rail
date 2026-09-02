//! Release planning and dry-run analysis

use crate::change_detection::changed_code_crate_names;
use crate::config::{ChangelogConfig, ChangelogRelativeTo, ReleaseConfig, SemverCheckPolicy};
use crate::error::{RailError, RailResult};
use crate::release::change_files::{ChangeBump, ChangeIntent, PendingChangeSet, render_change_body};
use crate::release::process;
use crate::release::semver_checks::{self, SemverCheck};
use crate::release::version::{BumpLevel, BumpRequest};
use crate::workspace::WorkspaceContext;
use cargo_metadata::PackageId;
use rustc_hash::{FxHashMap, FxHashSet};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) const RELEASE_REGISTRY: &str = "crates-io";
pub(crate) const RELEASE_PLAN_CONTRACT_VERSION: u32 = 6;

/// A plan for releasing one or more crates
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePlan {
    /// Contract version for release plan schema.
    pub plan_contract_version: u32,
    /// Workspace snapshot shared with planner, split, and sync ownership.
    pub snapshot_id: String,
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
    /// Exact standalone Cargo lockfile projections produced by this release.
    pub auxiliary_lockfiles: Vec<PlannedAuxiliaryLockfile>,
    /// Target crates excluded from the plan, with the reason (auto mode only).
    pub skipped: Vec<SkippedCrate>,
}

/// A target crate excluded from an auto-bump release plan
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Shared-history diagnostics for `release check`
#[derive(Debug, Clone, Default)]
pub struct ReleaseCheckInsights {
    /// Crates with code changes but no covering change file (sorted)
    pub missing_change_files: Vec<String>,
    /// Whether the repository is shallow and may not contain release tags.
    pub shallow_repository: bool,
}

/// Release plan for a single crate
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Whether to publish to crates.io
    pub publish: bool,
    /// Human-readable publish intent
    pub publish_intent: String,
    /// Whether to generate changelog
    pub generate_changelog: bool,
    /// Canonical bump expression `old -> new`.
    pub bump: String,
    /// Why this crate is in the plan.
    pub bump_reason: String,
    /// Rendered changelog body for the planned version, excluding the version header.
    pub changelog_body: String,
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

/// A synthesized dependency update entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyUpdate {
    /// Dependency crate name.
    pub name: String,
    /// Dependency version after this release plan.
    pub version: Version,
}

/// One change-file entry included in a crate release plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedChangeEntry {
    /// Source change file path.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    /// Requested bump level.
    pub bump: ChangeBump,
    /// User-facing changelog body.
    pub body: String,
}

/// Exact retained content for a partially consumed change file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedChangeFileUpdate {
    /// Change file rewritten by the release commit.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    /// Canonical content containing only retained no-release intents.
    pub content: String,
}

/// One exact committed standalone Cargo lockfile update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAuxiliaryLockfile {
    /// Configured workspace-relative manifest that owns this projection.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub manifest_path: PathBuf,
    /// Resolved workspace-relative committed lockfile.
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub lockfile_path: PathBuf,
    /// SHA-256 of the committed lockfile bytes used to plan.
    pub before_digest: String,
    /// SHA-256 of the exact post-release bytes.
    pub after_digest: String,
    /// Exact UTF-8 Cargo.lock content to write during apply and recovery.
    pub content: String,
}

/// Summary statistics for a release plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSummary {
    /// Total number of crates in the plan
    pub total_crates: usize,
    /// Number of crates that will be published
    pub crates_to_publish: usize,
    /// Number of crates that will be tagged
    pub crates_to_tag: usize,
}

/// How explicit crate selections should handle dependent closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependentPolicy {
    /// Reject partial selections that would leave dependents out of sync.
    RejectPartialClosure,
    /// Expand explicit selection to the full dependent closure.
    IncludeDependents,
}

/// Release planner
#[derive(Debug)]
pub struct ReleasePlanner<'a> {
    /// Workspace context
    ctx: &'a WorkspaceContext,
    /// Release configuration
    release_config: &'a ReleaseConfig,
    /// Lazily cached availability of the optional cargo-semver-checks binary.
    semver_available: Cell<Option<bool>>,
}

impl<'a> ReleasePlanner<'a> {
    /// Create a new release planner
    pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
        Self {
            ctx,
            release_config,
            semver_available: Cell::new(None),
        }
    }

    /// Build a release plan
    ///
    /// Uses dependency order for deterministic crate sequencing and computes
    /// per-crate publish/tag/changelog intent.
    pub fn plan(
        &self,
        crate_names: Option<Vec<String>>,
        bump_request: &BumpRequest,
        dependent_policy: DependentPolicy,
    ) -> RailResult<ReleasePlan> {
        let ordered_targets = self.resolve_targets(crate_names, dependent_policy)?;
        self.validate_touched_version_groups(&ordered_targets)?;

        // Build plan for each crate
        let mut crate_plans = Vec::with_capacity(ordered_targets.len());
        let mut version_map: FxHashMap<String, Version> = FxHashMap::default();
        let mut changed_code_by_base: HashMap<Option<String>, FxHashSet<String>> = HashMap::new();
        let workspace_members = self.ctx.graph().workspace_members();
        let pending_changes = PendingChangeSet::load(
            self.ctx.workspace_root(),
            &self.release_config.change_dir,
            workspace_members,
        )?;

        let mut skipped = Vec::new();
        for crate_name in &ordered_targets {
            match self.plan_crate(
                crate_name,
                bump_request,
                &version_map,
                &mut changed_code_by_base,
                &pending_changes,
                None,
            )? {
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
                &mut changed_code_by_base,
                &pending_changes,
            )?;
        }

        let planned_crates: HashSet<String> = crate_plans.iter().map(|plan| plan.name.clone()).collect();
        for plan in &mut crate_plans {
            let package_id = self.workspace_package_id(&plan.name)?;
            plan.affected_dependents = self
                .ctx
                .graph()
                .direct_dependents_by_id(package_id)?
                .into_iter()
                .filter(|dependent| planned_crates.contains(dependent))
                .collect();
        }

        // Build summary
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
        let consumption = pending_changes.plan_consumption(&selected_change_files, &planned_names)?;
        let change_files_to_update = consumption
            .update
            .into_iter()
            .map(|(path, content)| PlannedChangeFileUpdate { path, content })
            .collect();
        let auxiliary_lockfiles = crate::release::auxiliary::plan_lockfiles(
            self.ctx,
            &crate_plans,
            &self.release_config.auxiliary_cargo_manifests,
        )?;

        Ok(ReleasePlan {
            plan_contract_version: RELEASE_PLAN_CONTRACT_VERSION,
            snapshot_id: self.ctx.snapshot()?.id().to_string(),
            canonical_crate_order,
            crates: crate_plans,
            summary,
            change_files_to_delete: consumption.delete,
            change_files_to_update,
            auxiliary_lockfiles,
            skipped,
        })
    }

    /// Gather change-file coverage diagnostics for `release check`.
    pub fn release_check_insights(&self, crate_names: &[String]) -> RailResult<ReleaseCheckInsights> {
        let shallow_repository = is_shallow_repository(self.ctx.workspace_root())?;
        let pending_changes = PendingChangeSet::load(
            self.ctx.workspace_root(),
            &self.release_config.change_dir,
            self.ctx.graph().workspace_members(),
        )?;
        let mut changed_code_by_base: HashMap<Option<String>, FxHashSet<String>> = HashMap::new();
        let mut insights = ReleaseCheckInsights::default();

        for crate_name in crate_names {
            let previous_tag = self.find_previous_tag(crate_name)?;
            if !pending_changes.covers(crate_name)
                && self
                    .changed_code_crates(previous_tag, &mut changed_code_by_base)?
                    .contains(crate_name)
            {
                insights.missing_change_files.push(crate_name.clone());
            }
        }

        insights.missing_change_files.sort();
        insights.shallow_repository = shallow_repository;
        Ok(insights)
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

    /// Build a finalize-only plan from current manifest versions.
    ///
    /// This is used after a release PR has merged. It does not bump versions or
    /// edit changelogs; it verifies the expected changelog sections already
    /// exist and then drives tag/publish steps.
    pub fn finalize_plan(
        &self,
        crate_names: Option<Vec<String>>,
        dependent_policy: DependentPolicy,
    ) -> RailResult<ReleasePlan> {
        self.current_version_plan(crate_names, dependent_policy, true)
    }

    /// Rebuild post-commit release effects from an exact release checkout.
    pub(crate) fn recovery_plan(
        &self,
        crate_names: Option<Vec<String>>,
        dependent_policy: DependentPolicy,
    ) -> RailResult<ReleasePlan> {
        self.current_version_plan(crate_names, dependent_policy, false)
    }

    fn current_version_plan(
        &self,
        crate_names: Option<Vec<String>>,
        dependent_policy: DependentPolicy,
        require_changelog_entry: bool,
    ) -> RailResult<ReleasePlan> {
        let ordered_targets = self.resolve_targets(crate_names, dependent_policy)?;
        let mut crate_plans = Vec::with_capacity(ordered_targets.len());

        for crate_name in &ordered_targets {
            let package = self
                .ctx
                .cargo()
                .get_package(crate_name)
                .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;
            let manifest_path = package.manifest_path.clone().into_std_path_buf();
            let current_version = package.version.clone();
            let previous_tag = self.find_previous_tag(crate_name)?;
            let crate_config = self
                .ctx
                .config()
                .as_ref()
                .and_then(|config| config.crates.get(crate_name));
            let changelog_config = crate_config.and_then(|config| config.changelog.as_ref());
            let changelog_path = self.resolve_changelog_path(&manifest_path, changelog_config)?;
            let generate_changelog = !changelog_config.is_some_and(|config| config.skip);
            if require_changelog_entry
                && generate_changelog
                && !changelog_contains_version_entry(&changelog_path, &current_version.to_string())
            {
                return Err(RailError::with_help(
                    format!(
                        "release finalize expected {} v{} in {}",
                        crate_name,
                        current_version,
                        changelog_path.display()
                    ),
                    "merge the release PR first, or run release run --pr to create it",
                ));
            }

            let publish_from_cargo = crate::workspace::CargoState::package_allows_registry(package, RELEASE_REGISTRY);
            let publish = publish_from_cargo
                && crate_config
                    .and_then(|config| config.release.as_ref())
                    .map(|release| release.publish)
                    .unwrap_or(true);
            let tag_name = self.format_tag(crate_name, &current_version);
            crate_plans.push(CrateReleasePlan {
                name: crate_name.clone(),
                current_version: current_version.clone(),
                new_version: current_version.clone(),
                manifest_path,
                changelog_path,
                tag_name,
                previous_tag: previous_tag.clone(),
                changelog_range_start: previous_tag,
                changelog_range_end: "HEAD".to_string(),
                publish,
                publish_intent: if publish {
                    "publish_to_crates_io".to_string()
                } else {
                    "skip_publish".to_string()
                },
                generate_changelog,
                bump: format!("{} -> {}", current_version, current_version),
                bump_reason: "finalize merged release PR".to_string(),
                changelog_body: String::new(),
                dependency_updates: Vec::new(),
                change_entries: Vec::new(),
                version_group: self.version_group_for(crate_name).map(|(group, _)| group.to_string()),
                affected_dependents: Vec::new(),
                auto_bump_level: None,
                version_group_entry: None,
            });
        }

        let planned_crates: HashSet<String> = crate_plans.iter().map(|plan| plan.name.clone()).collect();
        for plan in &mut crate_plans {
            let package_id = self.workspace_package_id(&plan.name)?;
            plan.affected_dependents = self
                .ctx
                .graph()
                .direct_dependents_by_id(package_id)?
                .into_iter()
                .filter(|dependent| planned_crates.contains(dependent))
                .collect();
        }

        let crates_to_publish = crate_plans.iter().filter(|plan| plan.publish).count();
        Ok(ReleasePlan {
            plan_contract_version: RELEASE_PLAN_CONTRACT_VERSION,
            snapshot_id: self.ctx.snapshot()?.id().to_string(),
            canonical_crate_order: crate_plans.iter().map(|plan| plan.name.clone()).collect(),
            summary: ReleaseSummary {
                total_crates: crate_plans.len(),
                crates_to_publish,
                crates_to_tag: crate_plans.len(),
            },
            crates: crate_plans,
            change_files_to_delete: Vec::new(),
            change_files_to_update: Vec::new(),
            auxiliary_lockfiles: Vec::new(),
            skipped: Vec::new(),
        })
    }

    /// Plan release for a single crate
    fn plan_crate(
        &self,
        crate_name: &str,
        bump_request: &BumpRequest,
        version_map: &FxHashMap<String, Version>,
        changed_code_by_base: &mut HashMap<Option<String>, FxHashSet<String>>,
        pending_changes: &PendingChangeSet,
        version_group_force: Option<&VersionGroupForce>,
    ) -> RailResult<CratePlanOutcome> {
        // Get crate metadata
        let package = self
            .ctx
            .cargo()
            .get_package(crate_name)
            .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

        let manifest_path = package.manifest_path.clone().into_std_path_buf();

        // Get current version from cargo_metadata (already resolves workspace inheritance)
        let current_version = package.version.clone();

        let previous_tag = self.find_previous_tag(crate_name)?;

        // Get per-crate config (if any)
        let crate_config = self.ctx.config().and_then(|c| c.crates.get(crate_name));

        let changelog_config = crate_config.and_then(|c| c.changelog.as_ref());
        let changelog_path = self.resolve_changelog_path(&manifest_path, changelog_config)?;

        let generate_changelog = !changelog_config.is_some_and(|changelog_cfg| changelog_cfg.skip);

        if !pending_changes.covers(crate_name)
            && self
                .changed_code_crates(previous_tag.clone(), changed_code_by_base)?
                .contains(crate_name)
        {
            return Err(RailError::with_help(
                format!(
                    "missing change files for {crate_name}: code changes require {} coverage",
                    self.release_config.change_dir
                ),
                format!("add reviewed release intent under {}", self.release_config.change_dir),
            ));
        }
        let dependency_updates = self.dependency_updates(package, version_map);
        let change_entries = planned_change_entries(pending_changes.for_crate(crate_name));
        let change_bump = change_entries
            .iter()
            .filter_map(|entry| entry.bump.release_level())
            .max();

        // Cargo's registry allowlist is the hard ceiling. Repository policy
        // may narrow that authority but can never widen it.
        let publish_from_cargo = crate::workspace::CargoState::package_allows_registry(package, RELEASE_REGISTRY);
        let publish = publish_from_cargo
            && crate_config
                .and_then(|c| c.release.as_ref())
                .map(|r| r.publish)
                .unwrap_or(true);

        let explicit_signal = change_bump;
        let natural_level = explicit_signal.or_else(|| (!dependency_updates.is_empty()).then_some(BumpLevel::Patch));

        // cargo-semver-checks validates reviewed intent; it never invents or
        // escalates a release. A confirmed mismatch returns to the author.
        if publish {
            self.validate_semver_intent(crate_name, explicit_signal)?;
        }

        let (bump_type, bump_reason, planned_auto_level) = match bump_request {
            BumpRequest::Explicit(bump_type) => (bump_type.clone(), "explicit".to_string(), None),
            BumpRequest::Auto => {
                let (level, mut reason) = if let Some(force) = version_group_force {
                    (force.level, format!("version group {} -> {}", force.name, force.level))
                } else if let Some(level) = natural_level {
                    (
                        level,
                        auto_bump_reason(level, change_bump, !dependency_updates.is_empty()),
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
                            since, "no reviewed release intent or dependency updates"
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

        // Calculate new version
        let new_version = bump_type.apply(&current_version);

        // Determine tag name
        let tag_name = self.format_tag(crate_name, &new_version);

        let version_group_entry = version_group_force.and_then(|force| {
            natural_level
                .is_none()
                .then(|| format!("- synchronized version with version group {}.", force.name))
        });
        let changelog_body = if generate_changelog {
            render_changelog_body(&change_entries, version_group_entry.as_deref(), &dependency_updates)
        } else {
            String::new()
        };
        let bump = format!("{} -> {}", current_version, new_version);
        let publish_intent = if publish {
            "publish_to_crates_io".to_string()
        } else {
            "skip_publish".to_string()
        };

        Ok(CratePlanOutcome::Planned(Box::new(CrateReleasePlan {
            name: crate_name.to_string(),
            current_version,
            new_version,
            manifest_path,
            changelog_path,
            tag_name,
            previous_tag: previous_tag.clone(),
            changelog_range_start: previous_tag,
            changelog_range_end: "HEAD".to_string(),
            publish,
            publish_intent,
            generate_changelog,
            bump,
            bump_reason,
            changelog_body,
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

        match relative_to {
            ChangelogRelativeTo::Crate => Ok(manifest_path
                .parent()
                .ok_or_else(|| RailError::message("Invalid manifest path"))?
                .join(&changelog_relative_path)),
            ChangelogRelativeTo::Workspace => Ok(self.ctx.workspace_root().join(&changelog_relative_path)),
        }
    }

    fn apply_version_groups(
        &self,
        ordered_targets: &[String],
        crate_plans: &mut Vec<CrateReleasePlan>,
        skipped: &mut Vec<SkippedCrate>,
        version_map: &mut FxHashMap<String, Version>,
        changed_code_by_base: &mut HashMap<Option<String>, FxHashSet<String>>,
        pending_changes: &PendingChangeSet,
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
            match self.plan_crate(
                &crate_name,
                &BumpRequest::Auto,
                version_map,
                changed_code_by_base,
                pending_changes,
                Some(&force),
            )? {
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
            Self::refresh_changelog_body(plan);
        }
        Ok(())
    }

    fn refresh_changelog_body(plan: &mut CrateReleasePlan) {
        if !plan.generate_changelog {
            return;
        }
        plan.changelog_body = render_changelog_body(
            &plan.change_entries,
            plan.version_group_entry.as_deref(),
            &plan.dependency_updates,
        );
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

    fn semver_breaking_signal(&self, crate_name: &str) -> Option<String> {
        if !self.semver_available() || !semver_checks::has_library_target(self.ctx, crate_name) {
            return None;
        }

        // Inconclusive checks (missing baseline, network or build failure) never
        // escalate a bump; `release check --extended` surfaces them as advisory.
        match semver_checks::check_release(self.ctx, crate_name) {
            Ok(SemverCheck::Breaking { message }) => Some(message),
            Ok(SemverCheck::Pass | SemverCheck::Inconclusive { .. }) | Err(_) => None,
        }
    }

    fn validate_semver_intent(&self, crate_name: &str, declared_level: Option<BumpLevel>) -> RailResult<()> {
        if declared_level >= Some(BumpLevel::Major) {
            return Ok(());
        }
        let Some(message) = self.semver_breaking_signal(crate_name) else {
            return Ok(());
        };

        let help = format!(
            "revise the reviewed change entry for '{}' to use bump \"major\", then rerun the release plan",
            crate_name
        );
        Err(RailError::with_help(
            format!(
                "cargo-semver-checks requires a major release for '{}', but the declared release intent is {}: {}",
                crate_name,
                declared_level.map_or("none", |level| level.as_str()),
                message
            ),
            help,
        ))
    }

    fn semver_available(&self) -> bool {
        if self.release_config.semver_check == SemverCheckPolicy::Off {
            return false;
        }
        if let Some(available) = self.semver_available.get() {
            return available;
        }
        let available = semver_checks::is_available(self.ctx.workspace_root());
        self.semver_available.set(Some(available));
        available
    }

    /// Format git tag name for a crate
    ///
    /// Supports placeholders:
    /// - {prefix} - the tag_prefix config value (default: "v")
    /// - {crate} - the crate name
    /// - {version} - the version number
    fn format_tag(&self, crate_name: &str, version: &Version) -> String {
        let workspace_members = self.ctx.graph().workspace_members();
        let is_single_crate = workspace_members.len() == 1;

        // For single-crate repos, use simple "{prefix}{version}" format
        // For monorepos, use tag_format with all placeholders
        if is_single_crate {
            format!("{}{}", self.release_config.tag_prefix, version)
        } else {
            // Apply all placeholders including {prefix}
            const VERSION_PLACEHOLDER: &str = "\u{7b}version\u{7d}";
            self.release_config
                .tag_format
                .replace("{prefix}", &self.release_config.tag_prefix)
                .replace("{crate}", crate_name)
                .replace(VERSION_PLACEHOLDER, &version.to_string())
        }
    }

    fn find_previous_tag(&self, crate_name: &str) -> RailResult<Option<String>> {
        let workspace_members = self.ctx.graph().workspace_members();
        let is_single_crate = workspace_members.len() == 1;
        let pattern = if is_single_crate {
            format!("{}*", self.release_config.tag_prefix)
        } else {
            self.release_config
                .tag_format
                .replace("{crate}", crate_name)
                .replace("{version}", "*")
        };

        self.ctx.git()?.git().find_latest_tag(&pattern)
    }

    pub(crate) fn resolve_targets(
        &self,
        crate_names: Option<Vec<String>>,
        dependent_policy: DependentPolicy,
    ) -> RailResult<Vec<String>> {
        let all_ordered = self.ctx.graph().publish_order()?;
        let Some(targets) = crate_names else {
            return Ok(all_ordered);
        };
        for crate_name in &targets {
            self.workspace_package_id(crate_name)?;
        }

        let requested: HashSet<String> = targets.into_iter().collect();

        if dependent_policy == DependentPolicy::RejectPartialClosure {
            self.reject_partial_version_groups(&requested)?;
        }

        let requested_ids = self.workspace_package_ids(&requested)?;
        let dependents = self.ctx.graph().transitive_dependents_of_ids(&requested_ids)?;
        if !dependents.is_empty() && dependent_policy == DependentPolicy::RejectPartialClosure {
            let mut missing: Vec<String> = dependents.into_iter().collect();
            missing.sort();
            return Err(RailError::with_help(
                format!(
                    "partial release would leave dependent crate(s) out of sync: {}",
                    missing.join(", ")
                ),
                "re-run with --include-dependents or release the full dependent closure",
            ));
        }

        if dependent_policy == DependentPolicy::IncludeDependents {
            let mut selected = requested;
            loop {
                let before = selected.len();
                self.expand_selected_version_groups(&mut selected);
                let selected_ids = self.workspace_package_ids(&selected)?;
                let dependents = self.ctx.graph().transitive_dependents_of_ids(&selected_ids)?;
                selected.extend(dependents);
                self.expand_selected_version_groups(&mut selected);
                if selected.len() == before {
                    break;
                }
            }
            return Ok(all_ordered.into_iter().filter(|name| selected.contains(name)).collect());
        }

        Ok(all_ordered
            .into_iter()
            .filter(|name| requested.contains(name))
            .collect())
    }

    fn workspace_package_id(&self, crate_name: &str) -> RailResult<&PackageId> {
        self.ctx
            .graph()
            .workspace_package_by_name(crate_name)
            .map(|package| &package.id)
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

    fn reject_partial_version_groups(&self, selected: &HashSet<String>) -> RailResult<()> {
        for (group_name, members) in &self.release_config.version_groups {
            let selected_count = members.iter().filter(|member| selected.contains(*member)).count();
            if selected_count == 0 || selected_count == members.len() {
                continue;
            }
            let mut missing: Vec<_> = members
                .iter()
                .filter(|member| !selected.contains(*member))
                .cloned()
                .collect();
            missing.sort();
            return Err(RailError::with_help(
                format!(
                    "partial release would leave version group '{}' out of sync: missing {}",
                    group_name,
                    missing.join(", ")
                ),
                "re-run with --include-dependents or release every member of the version group",
            ));
        }
        Ok(())
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

/// Determine whether git history is shallow using git's own repository view.
pub(crate) fn is_shallow_repository(workspace_root: &Path) -> RailResult<bool> {
    let output = process::run("git", &["rev-parse", "--is-shallow-repository"], Some(workspace_root))?;
    if !output.status.success() {
        return Err(RailError::message("failed to determine whether repository is shallow"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

impl ReleasePlan {
    /// Format summary for display
    ///
    /// Applies skip flags to presentation only; it does not mutate the plan.
    pub fn format_summary_with_flags(&self, skip_publish: bool, skip_tag: bool) -> String {
        let mut output = String::new();

        output.push_str("📦 Release Plan\n\n");

        for (i, crate_plan) in self.crates.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", i + 1, crate_plan.name));
            output.push_str(&format!(
                "   Version: {} → {}\n",
                crate_plan.current_version, crate_plan.new_version
            ));
            output.push_str(&format!("   Bump: {} ({})\n", crate_plan.bump, crate_plan.bump_reason));

            // Show tag status
            let tag_status = if skip_tag {
                "✗ (--skip-tag)"
            } else {
                &crate_plan.tag_name
            };
            output.push_str(&format!("   Tag: {}\n", tag_status));

            // Show publish status
            let publish_status = if skip_publish {
                "✗ (not authorized; pass --publish)".to_string()
            } else if crate_plan.publish {
                "✓".to_string()
            } else {
                "✗ (publish = false)".to_string()
            };
            output.push_str(&format!("   Publish: {}\n", publish_status));

            if !crate_plan.affected_dependents.is_empty() {
                output.push_str(&format!("   Affects: {}\n", crate_plan.affected_dependents.join(", ")));
            }
            output.push('\n');
        }

        if !self.auxiliary_lockfiles.is_empty() {
            output.push_str("Auxiliary Cargo lockfiles:\n");
            for projection in &self.auxiliary_lockfiles {
                output.push_str(&format!(
                    "  {} — {}\n",
                    projection.lockfile_path.display(),
                    projection.manifest_path.display()
                ));
            }
            output.push('\n');
        }

        if !self.skipped.is_empty() {
            output.push_str("Skipped:\n");
            for skip in &self.skipped {
                output.push_str(&format!("  {} — {}\n", skip.name, skip.reason));
            }
            output.push('\n');
        }

        // Adjust summary counts based on flags
        let effective_publish_count = if skip_publish {
            0
        } else {
            self.summary.crates_to_publish
        };
        let effective_tag_count = if skip_tag { 0 } else { self.summary.crates_to_tag };

        output.push_str(&format!(
            "Summary: {} crate(s), {} to publish, {} tag(s), {} skipped\n",
            self.summary.total_crates,
            effective_publish_count,
            effective_tag_count,
            self.skipped.len()
        ));

        output
    }

    /// Format summary for display (default: no skip flags)
    pub fn format_summary(&self) -> String {
        self.format_summary_with_flags(false, false)
    }
}

fn planned_change_entries(intents: &[ChangeIntent]) -> Vec<PlannedChangeEntry> {
    intents
        .iter()
        .map(|intent| PlannedChangeEntry {
            path: intent.path.clone(),
            bump: intent.bump,
            body: intent.body.clone(),
        })
        .collect()
}

fn render_changelog_body(
    change_entries: &[PlannedChangeEntry],
    version_group_entry: Option<&str>,
    dependency_updates: &[DependencyUpdate],
) -> String {
    let mut output = String::new();
    for entry in change_entries
        .iter()
        .filter(|entry| entry.bump.release_level().is_some())
    {
        let highlight = render_change_body(&entry.body);
        let highlight = highlight.trim();
        if !highlight.is_empty() {
            output.push_str(highlight);
            output.push_str("\n\n");
        }
    }
    if let Some(highlight) = version_group_entry.map(str::trim)
        && !highlight.is_empty()
    {
        output.push_str(highlight);
        output.push_str("\n\n");
    }
    if !dependency_updates.is_empty() {
        output.push_str("### Dependencies\n\n");
        for update in dependency_updates {
            output.push_str("- updated ");
            output.push_str(&update.name);
            output.push_str(" to ");
            output.push_str(&update.version.to_string());
            output.push('\n');
        }
        output.push('\n');
    }
    output
}

fn auto_bump_reason(level: BumpLevel, change_bump: Option<BumpLevel>, has_dependency_updates: bool) -> String {
    if change_bump.is_some() {
        format!("auto: reviewed change files -> {}", level)
    } else if has_dependency_updates {
        "auto: dependent closure -> patch".to_string()
    } else {
        format!("auto: {}", level)
    }
}

fn changelog_contains_version_entry(path: &Path, version: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let needle = format!("## [{}]", version);
    content.lines().any(|line| line.trim_start().starts_with(&needle))
}
