//! Release planning and dry-run analysis

use crate::config::{ChangelogConfig, ChangelogFilters, ChangelogRelativeTo, ReleaseConfig, SemverCheckPolicy};
use crate::error::{RailError, RailResult};
use crate::release::attribution::{AttributedCommit, AttributedHistory, CommitAttributor};
use crate::release::change_files::{ChangeIntent, PendingChangeSet, render_change_body};
use crate::release::changelog::{ChangelogSpec, CommitDiagnostic, CommitRef, resolve_links};
use crate::release::semver_checks;
use crate::release::version::{BumpLevel, BumpRequest, commit_bump_level};
use crate::workspace::WorkspaceContext;
use rustc_hash::FxHashMap;
use semver::Version;
use serde::Serialize;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// A plan for releasing one or more crates
#[derive(Debug, Clone, Serialize)]
pub struct ReleasePlan {
  /// Contract version for release plan schema.
  pub plan_contract_version: u32,
  /// Canonical crate order (dependency order).
  pub canonical_crate_order: Vec<String>,
  /// Crates to release in dependency order
  pub crates: Vec<CrateReleasePlan>,
  /// Summary statistics
  pub summary: ReleaseSummary,
  /// Pending change files consumed by this release plan.
  pub change_files_to_delete: Vec<PathBuf>,
}

/// Release plan for a single crate
#[derive(Debug, Clone, Serialize)]
pub struct CrateReleasePlan {
  /// Crate name
  pub name: String,
  /// Current version
  pub current_version: Version,
  /// New version after bump
  pub new_version: Version,
  /// Path to Cargo.toml
  pub manifest_path: PathBuf,
  /// Path to CHANGELOG.md
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
  /// Conventional commits attributed to this crate and included in changelog planning.
  pub commits: Vec<PlannedCommit>,
  /// Non-conventional or unknown-type diagnostics for this crate's release range.
  pub commit_diagnostics: Vec<CommitDiagnostic>,
  /// Rendered changelog body for the planned version, excluding the version header.
  pub changelog_body: String,
  /// Structured changelog entries for machine-readable release plan output.
  pub changelog_entries: Vec<serde_json::Value>,
  /// Dependency update entries synthesized for dependent-closure releases.
  pub dependency_updates: Vec<DependencyUpdate>,
  /// Intent-file entries consumed for this crate.
  pub change_entries: Vec<PlannedChangeEntry>,
  /// Dependents that will need version updates
  pub affected_dependents: Vec<String>,
}

/// One commit included in a crate release plan.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct DependencyUpdate {
  /// Dependency crate name.
  pub name: String,
  /// Dependency version after this release plan.
  pub version: Version,
}

/// One change-file entry included in a crate release plan.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedChangeEntry {
  /// Source change file path.
  pub path: PathBuf,
  /// Requested bump level.
  pub bump: BumpLevel,
  /// User-facing changelog body.
  pub body: String,
}

/// Summary statistics for a release plan
#[derive(Debug, Clone, Serialize)]
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

    // Build plan for each crate
    let mut crate_plans = Vec::with_capacity(ordered_targets.len());
    let mut version_map: FxHashMap<String, Version> = FxHashMap::default();
    let mut histories: HashMap<HistoryKey, AttributedHistory> = HashMap::new();
    let attributor = CommitAttributor::new(self.ctx);
    let workspace_members = self.ctx.graph.workspace_members();
    let pending_changes = PendingChangeSet::load(self.ctx.workspace_root(), workspace_members)?;

    for crate_name in &ordered_targets {
      if let Some(plan) = self.plan_crate(
        crate_name,
        bump_request,
        &version_map,
        &mut histories,
        &attributor,
        &pending_changes,
      )? {
        version_map.insert(crate_name.clone(), plan.new_version.clone());
        crate_plans.push(plan);
      }
    }

    let planned_crates: HashSet<String> = crate_plans.iter().map(|plan| plan.name.clone()).collect();
    for plan in &mut crate_plans {
      plan.affected_dependents = self
        .ctx
        .graph
        .direct_dependents(&plan.name)?
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
    let mut change_files_to_delete: Vec<PathBuf> = crate_plans
      .iter()
      .flat_map(|plan| plan.change_entries.iter().map(|entry| entry.path.clone()))
      .collect();
    change_files_to_delete.sort();
    change_files_to_delete.dedup();

    Ok(ReleasePlan {
      plan_contract_version: 1,
      canonical_crate_order,
      crates: crate_plans,
      summary,
      change_files_to_delete,
    })
  }

  /// Diagnose conventional-commit issues for already-resolved target crates.
  pub fn diagnose_commits(&self, crate_names: &[String]) -> RailResult<Vec<(String, Vec<CommitDiagnostic>)>> {
    let mut histories: HashMap<HistoryKey, AttributedHistory> = HashMap::new();
    let attributor = CommitAttributor::new(self.ctx);
    let mut diagnostics = Vec::with_capacity(crate_names.len());

    for crate_name in crate_names {
      let changelog_config = self
        .ctx
        .config
        .as_ref()
        .and_then(|config| config.crates.get(crate_name))
        .and_then(|crate_config| crate_config.changelog.as_ref());
      if changelog_config.is_some_and(|config| config.skip) {
        continue;
      }

      let previous_tag = self.find_previous_tag(crate_name)?;
      let filters = effective_changelog_filters(&self.release_config.changelog.filters, changelog_config);
      let history = history_for_range(&mut histories, &attributor, previous_tag, "HEAD", Some(filters))?;
      let commit_refs = history.commit_refs_for_crate(crate_name);
      let spec = ChangelogSpec::resolve(&self.release_config.changelog, changelog_config)?;
      let crate_diagnostics = spec.diagnose(&commit_refs);
      if !crate_diagnostics.is_empty() {
        diagnostics.push((crate_name.clone(), crate_diagnostics));
      }
    }

    Ok(diagnostics)
  }

  /// Find crates with code changes but no covering pending change file.
  pub fn missing_change_file_coverage(&self, crate_names: &[String]) -> RailResult<Vec<String>> {
    if !self.release_config.require_change_files.is_enabled() {
      return Ok(Vec::new());
    }

    let workspace_members = self.ctx.graph.workspace_members();
    let pending_changes = PendingChangeSet::load(self.ctx.workspace_root(), workspace_members)?;
    let mut histories: HashMap<HistoryKey, AttributedHistory> = HashMap::new();
    let attributor = CommitAttributor::new(self.ctx);
    let mut missing = Vec::new();

    for crate_name in crate_names {
      if !self.release_config.require_change_files.applies_to(crate_name) || pending_changes.covers(crate_name) {
        continue;
      }
      let previous_tag = self.find_previous_tag(crate_name)?;
      let history = history_for_range(&mut histories, &attributor, previous_tag, "HEAD", None)?;
      if history.has_code_changes(crate_name) {
        missing.push(crate_name.clone());
      }
    }

    missing.sort();
    Ok(missing)
  }

  /// Plan release for a single crate
  fn plan_crate(
    &self,
    crate_name: &str,
    bump_request: &BumpRequest,
    version_map: &FxHashMap<String, Version>,
    histories: &mut HashMap<HistoryKey, AttributedHistory>,
    attributor: &CommitAttributor<'_>,
    pending_changes: &PendingChangeSet,
  ) -> RailResult<Option<CrateReleasePlan>> {
    // Get crate metadata
    let package = self
      .ctx
      .cargo
      .get_package(crate_name)
      .ok_or_else(|| RailError::message(format!("Crate '{}' not found", crate_name)))?;

    let manifest_path = package.manifest_path.clone().into_std_path_buf();

    // Get current version from cargo_metadata (already resolves workspace inheritance)
    let current_version = package.version.clone();

    let previous_tag = self.find_previous_tag(crate_name)?;

    // Get per-crate config (if any)
    let crate_config = self.ctx.config.as_ref().and_then(|c| c.crates.get(crate_name));

    let changelog_config = crate_config.and_then(|c| c.changelog.as_ref());
    let changelog_path = self.resolve_changelog_path(&manifest_path, changelog_config)?;

    let generate_changelog = !changelog_config.is_some_and(|changelog_cfg| changelog_cfg.skip);

    let filters = effective_changelog_filters(&self.release_config.changelog.filters, changelog_config);
    let history = history_for_range(histories, attributor, previous_tag.clone(), "HEAD", Some(filters))?;
    let attributed: Vec<&AttributedCommit> = history.for_crate(crate_name).collect();
    let commits = planned_commits(&attributed);
    let commit_refs: Vec<CommitRef<'_>> = attributed.iter().map(|commit| commit.as_ref()).collect();
    let inferred_bump = commits
      .iter()
      .filter_map(|commit| bump_level_from_str(commit.bump.as_deref()))
      .max();
    let dependency_updates = self.dependency_updates(package, version_map);
    let change_entries = planned_change_entries(pending_changes.for_crate(crate_name));
    let change_bump = change_entries.iter().map(|entry| entry.bump).max();
    let semver_breaking = if matches!(bump_request, BumpRequest::Auto) {
      self.semver_breaking_signal(crate_name)
    } else {
      None
    };
    let semver_bump = semver_breaking.as_ref().map(|_| BumpLevel::Major);

    let (bump_type, bump_reason) = match bump_request {
      BumpRequest::Explicit(bump_type) => (bump_type.clone(), "explicit".to_string()),
      BumpRequest::Auto => {
        let explicit_signal = [change_bump, inferred_bump, semver_bump].into_iter().flatten().max();
        let Some(level) = explicit_signal.or_else(|| (!dependency_updates.is_empty()).then_some(BumpLevel::Patch))
        else {
          return Ok(None);
        };
        let reason = if semver_bump == Some(BumpLevel::Major)
          && [change_bump, inferred_bump]
            .into_iter()
            .flatten()
            .max()
            .is_some_and(|base| base < BumpLevel::Major)
        {
          format!("auto: cargo-semver-checks escalated to {}", level)
        } else if change_bump.is_some() {
          format!("auto: change files -> {}", level)
        } else if inferred_bump.is_some() {
          format!("auto: conventional commits -> {}", level)
        } else if semver_bump.is_some() {
          format!("auto: cargo-semver-checks -> {}", level)
        } else {
          "auto: dependent closure -> patch".to_string()
        };
        (
          level.to_bump_type(&current_version, self.release_config.pre_1_breaking_bump),
          reason,
        )
      }
    };

    // Calculate new version
    let new_version = bump_type.apply(&current_version);

    // Determine tag name
    let tag_name = self.format_tag(crate_name, &new_version);

    let spec = ChangelogSpec::resolve(&self.release_config.changelog, changelog_config)?;
    let links = resolve_links(
      &self.release_config.changelog,
      changelog_config,
      self.ctx.workspace_root(),
    )?;
    let dependency_pairs: Vec<(String, Version)> = dependency_updates
      .iter()
      .map(|update| (update.name.clone(), update.version.clone()))
      .collect();
    let highlights: Vec<String> = change_entries
      .iter()
      .map(|entry| render_change_body(&entry.body))
      .collect();
    let changelog_body = if generate_changelog {
      spec.render(&links, &highlights, &commit_refs, &dependency_pairs)
    } else {
      String::new()
    };
    let changelog_entries = if generate_changelog {
      structured_changelog_entries(crate_name, &spec, &commit_refs, &commits)
    } else {
      Vec::new()
    };
    let commit_diagnostics = spec.diagnose(&commit_refs);

    // Check if should publish - per-crate config takes priority, then Cargo.toml
    let publish_from_cargo = crate::workspace::CargoState::is_package_publishable(package);
    let publish = crate_config
      .and_then(|c| c.release.as_ref())
      .map(|r| r.publish)
      .unwrap_or(publish_from_cargo);

    // Only mutate dependents that are explicitly part of this release plan.
    let bump = format!("{} -> {}", current_version, new_version);
    let publish_intent = if publish {
      "publish_to_crates_io".to_string()
    } else {
      "skip_publish".to_string()
    };

    Ok(Some(CrateReleasePlan {
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
      commits,
      commit_diagnostics,
      changelog_body,
      changelog_entries,
      dependency_updates,
      change_entries,
      affected_dependents: Vec::new(),
    }))
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
      ChangelogRelativeTo::Crate => Ok(
        manifest_path
          .parent()
          .ok_or_else(|| RailError::message("Invalid manifest path"))?
          .join(&changelog_relative_path),
      ),
      ChangelogRelativeTo::Workspace => Ok(self.ctx.workspace_root().join(&changelog_relative_path)),
    }
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

    match semver_checks::check_release(self.ctx, crate_name) {
      Ok(outcome) if outcome.breaking => Some(outcome.message),
      _ => None,
    }
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
    let workspace_members = self.ctx.graph.workspace_members();
    let is_single_crate = workspace_members.len() == 1;

    // For single-crate repos, use simple "{prefix}{version}" format
    // For monorepos, use tag_format with all placeholders
    if is_single_crate {
      format!("{}{}", self.release_config.tag_prefix, version)
    } else {
      // Apply all placeholders including {prefix}
      self
        .release_config
        .tag_format
        .replace("{prefix}", &self.release_config.tag_prefix)
        .replace("{crate}", crate_name)
        .replace("{version}", &version.to_string())
    }
  }

  fn find_previous_tag(&self, crate_name: &str) -> RailResult<Option<String>> {
    let workspace_members = self.ctx.graph.workspace_members();
    let is_single_crate = workspace_members.len() == 1;
    let pattern = if is_single_crate {
      format!("{}*", self.release_config.tag_prefix)
    } else {
      self
        .release_config
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
    let all_ordered = self.ctx.graph.publish_order()?;
    let Some(targets) = crate_names else {
      return Ok(all_ordered);
    };
    let workspace_members = self.ctx.graph.workspace_members();
    for crate_name in &targets {
      if !workspace_members.contains(crate_name) {
        return Err(RailError::with_help(
          format!("crate '{}' not found", crate_name),
          format!("available: {}", workspace_members.join(", ")),
        ));
      }
    }

    let requested: HashSet<String> = targets.into_iter().collect();
    let dependents = self.ctx.graph.transitive_dependents_of_set(&requested)?;

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

    let mut selected = requested;
    if dependent_policy == DependentPolicy::IncludeDependents {
      selected.extend(dependents);
    }

    Ok(all_ordered.into_iter().filter(|name| selected.contains(name)).collect())
  }
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
        "✗ (--skip-publish)".to_string()
      } else if crate_plan.publish {
        "✓".to_string()
      } else {
        "✗ (publish = false)".to_string()
      };
      output.push_str(&format!("   Publish: {}\n", publish_status));

      if !crate_plan.affected_dependents.is_empty() {
        output.push_str(&format!("   Affects: {}\n", crate_plan.affected_dependents.join(", ")));
      }
      if !crate_plan.commits.is_empty() {
        let causes: Vec<_> = crate_plan
          .commits
          .iter()
          .filter_map(|commit| {
            commit
              .bump
              .as_ref()
              .map(|bump| format!("{} ({})", commit.sha.get(..7).unwrap_or(&commit.sha), bump))
          })
          .collect();
        if !causes.is_empty() {
          output.push_str(&format!("   Causes: {}\n", causes.join(", ")));
        }
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
      "Summary: {} crate(s), {} to publish, {} tag(s)\n",
      self.summary.total_crates, effective_publish_count, effective_tag_count
    ));

    output
  }

  /// Format summary for display (default: no skip flags)
  pub fn format_summary(&self) -> String {
    self.format_summary_with_flags(false, false)
  }
}

fn history_for_range<'a>(
  histories: &'a mut HashMap<HistoryKey, AttributedHistory>,
  attributor: &CommitAttributor<'_>,
  from: Option<String>,
  to: &str,
  filters: Option<&ChangelogFilters>,
) -> RailResult<&'a AttributedHistory> {
  let key = HistoryKey::new(from, filters);
  if !histories.contains_key(&key) {
    let history = match filters {
      Some(filters) => attributor.history_with_filters(key.from.as_deref(), to, Some(filters))?,
      None => attributor.history(key.from.as_deref(), to)?,
    };
    histories.insert(key.clone(), history);
  }

  histories
    .get(&key)
    .ok_or_else(|| RailError::message("internal error: missing cached release history"))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HistoryKey {
  from: Option<String>,
  include_paths: Vec<String>,
  exclude_paths: Vec<String>,
}

impl HistoryKey {
  fn new(from: Option<String>, filters: Option<&ChangelogFilters>) -> Self {
    let (include_paths, exclude_paths) = filters
      .map(|filters| (filters.include_paths.clone(), filters.exclude_paths.clone()))
      .unwrap_or_default();
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

fn bump_level_from_str(value: Option<&str>) -> Option<BumpLevel> {
  value.and_then(|value| value.parse().ok())
}

fn effective_changelog_filters<'a>(
  workspace_filters: &'a ChangelogFilters,
  overrides: Option<&'a ChangelogConfig>,
) -> &'a ChangelogFilters {
  overrides
    .and_then(|config| config.filters.as_ref())
    .unwrap_or(workspace_filters)
}

fn structured_changelog_entries(
  crate_name: &str,
  spec: &ChangelogSpec,
  commit_refs: &[CommitRef<'_>],
  planned: &[PlannedCommit],
) -> Vec<serde_json::Value> {
  let bump_by_sha: HashMap<&str, Option<&str>> = planned
    .iter()
    .map(|commit| (commit.sha.as_str(), commit.bump.as_deref()))
    .collect();

  spec
    .entries_json(commit_refs)
    .into_iter()
    .map(|mut entry| {
      if let Some(object) = entry.as_object_mut() {
        object.insert("crate".to_string(), serde_json::Value::String(crate_name.to_string()));
        if let Some(sha) = object.get("sha").and_then(|value| value.as_str())
          && let Some(bump) = bump_by_sha.get(sha).and_then(|value| *value)
        {
          object.insert("bump".to_string(), serde_json::Value::String(bump.to_string()));
        } else {
          object.insert("bump".to_string(), serde_json::Value::Null);
        }
      }
      entry
    })
    .collect()
}
