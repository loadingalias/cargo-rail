//! Clean unification analyzer using the HYBRID approach
//!
//! This is the core of the unify implementation that properly integrates
//! with WorkspaceContext and uses multi-target metadata + manifest analysis.
//! The undeclared features detection lives here.

use crate::cargo::{
  manifest_analyzer::{ExistingWorkspaceDep, ManifestAnalyzer, parse_existing_workspace_deps},
  multi_target_metadata::MultiTargetMetadata,
  unify::version_utils::{find_major_version_conflicts, is_exact_pin, versions_compatible},
  unify::{CandidateIterator, FeaturePruner, TransitivePlanner, UnusedDepFinder},
  unify_types::{
    DuplicateCleanup, IssueSeverity, MemberEdit, UndeclaredFeature, UnificationPlan, UnifiedDep, UnifyIssue,
    ValidationResult, VersionMismatch,
  },
};
use crate::config::{ExactPinHandling, MajorVersionConflict, UnifyConfig};
use crate::error::{RailResult, ResultExt};
use crate::progress;
use crate::workspace::WorkspaceContext;
use rustc_hash::{FxHashMap, FxHashSet};
use semver::VersionReq;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Analyzes workspace for dependency unifications
pub struct UnifyAnalyzer {
  /// Multi-target metadata (Arc for cheap sharing - avoids expensive clone)
  metadata: Arc<MultiTargetMetadata>,
  manifests: ManifestAnalyzer,
  /// Configuration for unification behavior (can be overridden at runtime)
  pub config: UnifyConfig,
  /// Existing workspace dependencies (already in [workspace.dependencies])
  existing_workspace_deps: FxHashMap<String, ExistingWorkspaceDep>,
  /// Workspace root path (for path normalization)
  workspace_root: PathBuf,
}

impl UnifyAnalyzer {
  /// Create a new analyzer from workspace context
  pub fn new(ctx: &WorkspaceContext) -> RailResult<Self> {
    // Get multi-target metadata (lazily loaded on first access)
    // Uses Arc::clone for cheap reference counting instead of cloning the entire cache
    let metadata = ctx.multi_target_metadata()?;

    // Parse all manifests once
    let workspace_packages = metadata.workspace_packages();
    let manifests = ManifestAnalyzer::parse_workspace(ctx.workspace_root(), &workspace_packages)?;

    // Parse existing workspace.dependencies to avoid duplicates
    let existing_workspace_deps = parse_existing_workspace_deps(ctx.workspace_root())?;

    // Build config from context
    let config = ctx.config.as_ref().map(|c| c.unify.clone()).unwrap_or_default();

    Ok(Self {
      metadata,
      manifests,
      config,
      existing_workspace_deps,
      workspace_root: ctx.workspace_root().to_path_buf(),
    })
  }

  /// Normalize a dependency path relative to the workspace root.
  ///
  /// Takes a path that's relative to a member's Cargo.toml and converts it
  /// to be relative to the workspace root.
  ///
  /// # Arguments
  /// * `member_manifest_path` - Path to the member's Cargo.toml
  /// * `dep_path` - The dependency path (relative to member's Cargo.toml)
  fn normalize_dep_path(&self, member_manifest_path: &Path, dep_path: &str) -> PathBuf {
    // Get the member's directory (parent of Cargo.toml)
    let member_dir = member_manifest_path.parent().unwrap_or(member_manifest_path);
    // Resolve the dep path relative to the member's directory
    let absolute_dep_path = member_dir.join(dep_path);

    // Canonicalize to resolve .. and symlinks (if path exists)
    let canonical_workspace = self
      .workspace_root
      .canonicalize()
      .unwrap_or_else(|_| self.workspace_root.clone());
    let canonical_dep = absolute_dep_path.canonicalize().unwrap_or(absolute_dep_path.clone());

    // Make relative to workspace root
    if let Ok(relative) = canonical_dep.strip_prefix(&canonical_workspace) {
      return relative.to_path_buf();
    }

    // Fallback: if canonicalization moved outside workspace, try non-canonicalized path
    if let Ok(relative) = absolute_dep_path.strip_prefix(&self.workspace_root) {
      return relative.to_path_buf();
    }

    // Last resort: use the path as-is
    PathBuf::from(dep_path)
  }

  /// Normalize a workspace member directory path relative to the workspace root.
  fn normalize_workspace_member_path(&self, member_manifest_path: &Path) -> PathBuf {
    let member_dir = member_manifest_path.parent().unwrap_or(member_manifest_path);

    let canonical_workspace = self
      .workspace_root
      .canonicalize()
      .unwrap_or_else(|_| self.workspace_root.clone());
    let canonical_member = member_dir.canonicalize().unwrap_or_else(|_| member_dir.to_path_buf());

    if let Ok(relative) = canonical_member.strip_prefix(&canonical_workspace) {
      return relative.to_path_buf();
    }

    if let Ok(relative) = member_dir.strip_prefix(&self.workspace_root) {
      return relative.to_path_buf();
    }

    member_dir.to_path_buf()
  }

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    let dep_count = self.manifests.all_dependencies().len();
    let mut workspace_deps = Vec::with_capacity(dep_count);
    let mut member_edits: FxHashMap<Arc<str>, Vec<MemberEdit>> = FxHashMap::default();
    let mut member_paths: FxHashMap<Arc<str>, PathBuf> = FxHashMap::default();
    let mut issues = Vec::with_capacity(16); // Issues are rare, small pre-alloc
    let mut duplicates_cleaned = Vec::with_capacity(8);
    let mut version_mismatches = Vec::with_capacity(8);

    // Pre-compute workspace member metadata for reuse.
    let mut workspace_member_names: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut workspace_member_paths: FxHashMap<Arc<str>, PathBuf> = FxHashMap::default();

    // Build member_paths mapping from metadata (manifest file paths)
    for pkg in self.metadata.workspace_packages() {
      let member_name = Arc::from(pkg.name.as_str());
      let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
      workspace_member_names.insert(Arc::clone(&member_name));
      workspace_member_paths.insert(
        Arc::clone(&member_name),
        self.normalize_workspace_member_path(&manifest_path),
      );
      member_paths.insert(member_name, manifest_path);
    }

    progress!("Analyzing {} dependencies...", self.manifests.all_dependencies().len());

    // Process each dependency using the CandidateIterator
    // This encapsulates include_renamed branching and filtering logic
    for candidate in CandidateIterator::new(&self.manifests, &self.config) {
      let dep_key = candidate.dep_key;
      let usage_sites = candidate.usages;

      // Check for major version conflicts
      // Different major versions cannot be safely merged - behavior depends on config.
      let major_versions = find_major_version_conflicts(&usage_sites);
      if major_versions.len() > 1 {
        let versions_str: Vec<_> = major_versions.iter().map(|v| v.to_string()).collect();
        match self.config.major_version_conflict {
          MajorVersionConflict::Warn => {
            // Skip unification, emit warning - both versions stay in the graph
            issues.push(UnifyIssue {
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(format!(
                "Multiple major versions detected (majors: {}) - skipping unification. \
                 Consider consolidating to a single major version to reduce duplicate compilation, \
                 or set major_version_conflict = \"bump\" to force unification to highest version.",
                versions_str.join(", ")
              )),
            });
            continue; // Skip this dependency, continue with others
          }
          MajorVersionConflict::Bump => {
            // Emit warning but continue with unification to highest version
            issues.push(UnifyIssue {
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(format!(
                "Multiple major versions detected (majors: {}) - bumping to highest resolved version. \
                 This may break crates depending on older major versions.",
                versions_str.join(", ")
              )),
            });
            // Don't continue - fall through to use highest resolved version
          }
        }
      }

      // Check for exact version pins
      let has_exact_pin = usage_sites
        .iter()
        .any(|u| u.declared_version.as_ref().map(|v| is_exact_pin(v)).unwrap_or(false));

      if has_exact_pin {
        match self.config.exact_pin_handling {
          ExactPinHandling::Skip => {
            // Skip this dep entirely - don't unify exact pins
            continue;
          }
          ExactPinHandling::Warn => {
            issues.push(UnifyIssue {
              dep_name: Arc::clone(&dep_key.name),
              severity: IssueSeverity::Warning,
              message: Arc::from(
                "Has exact version pin (=x.y.z) - converting to caret (^). \
                        Consider adding to unify.exclude or setting exact_pin_handling = \"skip\"",
              ),
            });
          }
          ExactPinHandling::Preserve => {
            // Will be handled below when constructing version_req
          }
        }
      }

      // Get version from metadata - but ONLY from direct dependencies of workspace members
      // This uses direct_dep_versions() which filters out transitive dependencies
      let versions = self.metadata.direct_dep_versions(&dep_key.name);
      if versions.is_empty() {
        continue; // Not a direct dependency of any workspace member
      }

      // Get unique versions across all targets
      let unique_versions: FxHashSet<_> = versions.values().collect();

      // Always use highest version (cargo's resolver already picks highest compatible)
      // When targets resolve to different versions, we unify to highest
      let version = match unique_versions.iter().max() {
        Some(max) if unique_versions.len() > 1 => {
          // Multiple versions found across targets - use highest
          // This is the "silent win" - we unify duplicates automatically
          let mut versions_found: Vec<Arc<str>> = unique_versions.iter().map(|v| Arc::from(v.to_string())).collect();
          versions_found.sort();
          duplicates_cleaned.push(DuplicateCleanup {
            dep_name: Arc::clone(&dep_key.name),
            versions_found,
            selected_version: Arc::from(max.to_string()),
          });
          max
        }
        Some(version) => version,
        None => continue, // Empty set - shouldn't happen given versions check above
      };

      // Construct version requirement - preserve exact pins if configured
      let version_req = if has_exact_pin && self.config.exact_pin_handling == ExactPinHandling::Preserve {
        // Find the exact pin version from declared_version
        let exact_version = usage_sites
          .iter()
          .find_map(|u| u.declared_version.as_ref().filter(|v| is_exact_pin(v)))
          .cloned()
          .unwrap_or_else(|| format!("={}", version));
        // Try exact version first, fall back to caret format, then ultimate fallback
        VersionReq::parse(&exact_version)
          .or_else(|_| VersionReq::parse(&format!("^{}", version)))
          .unwrap_or_else(|_| VersionReq::default())
      } else {
        // Try caret format first, fall back to plain version, then ultimate fallback
        VersionReq::parse(&format!("^{}", version))
          .or_else(|_| VersionReq::parse(&version.to_string()))
          .unwrap_or_else(|_| VersionReq::default())
      };

      // Check for version mismatches with existing workspace.dependencies
      if let Some(existing_ws_dep) = self.existing_workspace_deps.get(&*dep_key.name)
        && let Some(ref ws_version) = existing_ws_dep.version
      {
        for usage in &usage_sites {
          if let Some(ref declared) = usage.declared_version
            && !versions_compatible(declared, ws_version)
          {
            version_mismatches.push(VersionMismatch {
              member: Arc::clone(&usage.used_by),
              dep_name: Arc::clone(&dep_key.name),
              member_version: Arc::from(declared.as_str()),
              workspace_version: Arc::from(ws_version.as_str()),
            });

            // Add as issue based on strictness config
            let severity = if self.config.strict_version_compat {
              IssueSeverity::Error
            } else {
              IssueSeverity::Warning
            };
            issues.push(UnifyIssue {
              dep_name: Arc::clone(&dep_key.name),
              severity,
              message: Arc::from(format!(
                "{} declares \"{}\" but workspace.dependencies has \"{}\". \
                 Converting to workspace = true may break this crate.",
                usage.used_by, declared, ws_version
              )),
            });
          }
        }
      }

      // Check for mixed defaults - use union strategy if present
      // When include_renamed = true, check across all package variants
      let has_mixed_defaults = if self.config.include_renamed {
        self.manifests.package_has_mixed_defaults(&dep_key.name)
      } else {
        self.manifests.has_mixed_defaults(dep_key)
      };

      // Compute features - when include_renamed = true, aggregate across all variants
      // Note: intersection doesn't make sense across renamed deps (they're separate usages)
      // so we use union to ensure all needed features are included
      let (features, default_features) = if self.config.include_renamed {
        // For package-level aggregation, always use union strategy
        // (renamed deps typically have distinct feature needs)
        let features = self.manifests.compute_package_union(&dep_key.name);
        // When mixed defaults detected, use default-features = true to preserve
        // features for crates that rely on them (same logic as non-include_renamed path)
        let df = if has_mixed_defaults {
          true
        } else {
          self
            .manifests
            .package_default_features_policy(&dep_key.name)
            .unwrap_or(true)
        };
        (features, df)
      } else {
        // Standard per-dep_key logic
        let intersection = self.manifests.compute_intersection(dep_key);

        if has_mixed_defaults || intersection.is_empty() {
          // Use union strategy for:
          // 1. Mixed default-features settings
          // 2. No common features (empty intersection)
          // Set default-features = true (max/union strategy)
          (self.manifests.compute_union(dep_key), true)
        } else {
          // Use intersection (minimal) strategy
          // Use conservative default-features policy
          let df = self.manifests.default_features_policy(dep_key).unwrap_or(true);
          (intersection, df)
        }
      };

      // Note: Target constraints stay in member manifests, not workspace.dependencies.
      // Members use `[target.'cfg(...)'.dependencies] dep = { workspace = true }`.
      // We don't set target on UnifiedDep because [workspace.dependencies] cannot
      // have [target] sections in virtual workspaces, and even in non-virtual ones
      // it's cleaner to keep target constraints where they're used.
      let target = None;

      // Get users - use Arc<str> to share allocations
      let users: FxHashSet<Arc<str>> = usage_sites.iter().map(|u| Arc::clone(&u.used_by)).collect();

      // Workspace members must always carry `path` so member-to-member deps stay local.
      // This prevents dual-resolution in fresh lockfiles (local member + crates.io package).
      let dep_path: Option<PathBuf> = if workspace_member_names.contains(&dep_key.name) {
        workspace_member_paths.get(&dep_key.name).cloned()
      } else if self.config.include_paths {
        // Include explicit path deps for non-member packages only when requested.
        usage_sites.iter().find_map(|u| {
          u.path.as_ref().and_then(|p| {
            u.manifest_path
              .as_ref()
              .map(|manifest_path| self.normalize_dep_path(manifest_path, p))
          })
        })
      } else {
        None // include_paths = false, skip all path deps
      };

      // Compute the unified features (this is what workspace.dependencies should have)
      let computed_features: Vec<Arc<str>> = features.into_iter().map(Arc::from).collect();

      // Check if this dep already exists in workspace.dependencies
      let existing_dep = self.existing_workspace_deps.get(&*dep_key.name);

      // Determine if we need to update workspace.dependencies
      // Update if: dep doesn't exist, OR features differ, OR default-features differs, OR has path
      let needs_workspace_update = match existing_dep {
        None => true, // Doesn't exist, need to add
        Some(existing) => {
          // Check if features are different (compare as &str)
          let mut existing_features: Vec<&str> = existing.features.iter().map(String::as_str).collect();
          let mut computed: Vec<&str> = computed_features.iter().map(|s| &**s).collect();
          existing_features.sort();
          computed.sort();

          // Also check if existing path is missing or differs from resolved path.
          let path_differs = match (&dep_path, &existing.path) {
            (Some(new_path), Some(existing_path)) => Path::new(existing_path) != new_path,
            (Some(_), None) => true,
            _ => false,
          };

          existing_features != computed || existing.default_features != default_features || path_differs
        }
      };

      // The features to use for local feature calculation
      // Use computed features (which will be what goes in workspace.dependencies)
      let workspace_features = computed_features.clone();

      // Create unified dep if workspace needs update
      if needs_workspace_update {
        let unified = UnifiedDep {
          name: Arc::clone(&dep_key.name),
          version_req,
          features: workspace_features.clone(),
          default_features,
          used_by: users.into_iter().collect(),
          target,
          path: dep_path, // Include path for workspace member deps
        };
        workspace_deps.push(unified);
      }

      // Compute member edits for ALL dep kinds (normal, dev, build)
      // Per design doc: all dep kinds should be unified, not just Normal
      // This happens whether or not the dep already exists in workspace.dependencies
      for usage in usage_sites {
        // Compute local features (member features - workspace features)
        // Convert to Arc<str> and filter out features already in workspace
        let local_features: Vec<Arc<str>> = usage
          .unconditional_features
          .iter()
          .filter(|f| !workspace_features.iter().any(|wf| &**wf == *f))
          .map(|f| Arc::from(f.as_str()))
          .collect();

        // Use the cargo_toml_key from the usage - this is the actual key in Cargo.toml
        // For renamed deps like `old_serde = { package = "serde" }`, this is "old_serde"
        // This is critical for include_renamed mode where usage_sites are aggregated
        // across multiple dep_keys for the same package
        let edit = MemberEdit::UseWorkspace {
          dep_name: Arc::clone(&usage.cargo_toml_key),
          dep_kind: usage.kind,
          target: usage.target.as_ref().map(|t| Arc::from(t.as_str())), // Preserve target for correct section
          local_features,
          is_optional: usage.optional,
        };

        member_edits.entry(Arc::clone(&usage.used_by)).or_default().push(edit);
      }
    }

    // Handle transitive fragmentation
    let transitive_pins = if self.config.pin_transitives {
      TransitivePlanner::new(&self.metadata).find_pins()
    } else {
      Vec::new()
    };

    // Compute MSRV if enabled
    let computed_msrv = if self.config.msrv {
      progress!("Computing MSRV from dependency graph...");
      self
        .metadata
        .compute_msrv_with_config(&self.workspace_root, self.config.msrv_source)
    } else {
      None
    };

    // Optionally ensure every workspace member inherits MSRV from the workspace.
    //
    // This is only safe when we have (or will write) `[workspace.package].rust-version`.
    // When msrv is disabled or cannot be computed, `rust-version = { workspace = true }`
    // would cause Cargo to error if the workspace value is absent.
    if self.config.enforce_msrv_inheritance {
      if !self.config.msrv {
        issues.push(UnifyIssue {
          dep_name: Arc::from("rust-version"),
          severity: IssueSeverity::Warning,
          message: Arc::from(
            "enforce_msrv_inheritance is enabled but unify.msrv is false; skipping MSRV inheritance enforcement",
          ),
        });
      } else if computed_msrv.is_none() {
        issues.push(UnifyIssue {
          dep_name: Arc::from("rust-version"),
          severity: IssueSeverity::Warning,
          message: Arc::from(
            "enforce_msrv_inheritance is enabled but no workspace MSRV could be determined; skipping MSRV inheritance enforcement",
          ),
        });
      } else {
        progress!("Enforcing MSRV inheritance on workspace members...");
        for (member_name, manifest_path) in &member_paths {
          if member_manifest_inherits_msrv(manifest_path)? {
            continue;
          }
          member_edits
            .entry(Arc::clone(member_name))
            .or_default()
            .push(MemberEdit::EnforceMsrvInheritance);
        }
      }
    }

    // Scan for dead and optional features if enabled
    let feature_pruner = FeaturePruner::new(&self.metadata, &self.config);
    let (pruned_features, optional_features) = if self.config.prune_dead_features {
      progress!("Scanning for dead features in resolved graph...");
      feature_pruner.scan()
    } else {
      (Vec::new(), Vec::new())
    };

    // Detect unused dependencies
    let unused_finder = UnusedDepFinder::new(&self.metadata, &self.manifests);
    let unused_deps = if self.config.detect_unused {
      progress!("Detecting unused dependencies...");
      unused_finder.find()
    } else {
      Vec::new()
    };

    // Generate removal edits if remove_unused is enabled
    if self.config.remove_unused && !unused_deps.is_empty() {
      progress!(
        "Generating removal edits for {} unused dependencies...",
        unused_deps.len()
      );
      for (member, edits) in unused_finder.generate_removal_edits(&unused_deps) {
        member_edits.entry(Arc::from(member)).or_default().extend(edits);
      }
    }

    // Generate removal edits for dead features (empty no-ops)
    for (crate_name, edits) in feature_pruner.generate_prune_edits(&pruned_features) {
      member_edits.entry(crate_name).or_default().extend(edits);
    }

    // Detect undeclared features
    // Find cases where a crate uses features that it didn't declare in Cargo.toml
    // This happens when Cargo's feature unification "borrows" features from other members
    let undeclared_features = if self.config.detect_undeclared_features {
      progress!("Checking for undeclared feature dependencies...");
      self.detect_undeclared_features()
    } else {
      Vec::new()
    };

    // Generate fixes or warnings for undeclared features
    if self.config.fix_undeclared_features && !undeclared_features.is_empty() {
      progress!(
        "Generating fixes for {} undeclared feature issues...",
        undeclared_features.len()
      );
      for uf in &undeclared_features {
        let edit = MemberEdit::AddFeatures {
          dep_name: Arc::clone(&uf.dep_name),
          dep_kind: uf.dep_kind,
          target: uf.target.clone(),
          features_to_add: uf.undeclared_features.clone(),
        };
        member_edits.entry(Arc::clone(&uf.member)).or_default().push(edit);
      }
    } else {
      // Add issues for undeclared features (warn mode)
      for uf in &undeclared_features {
        let undeclared_str: Vec<&str> = uf.undeclared_features.iter().map(|s| &**s).collect();
        let declared_str = if uf.declared_features.is_empty() {
          "none".to_string()
        } else {
          let strs: Vec<&str> = uf.declared_features.iter().map(|s| &**s).collect();
          strs.join(", ")
        };
        issues.push(UnifyIssue {
          dep_name: Arc::clone(&uf.dep_name),
          severity: IssueSeverity::Warning,
          message: Arc::from(format!(
            "{} uses {} features [{}] but only declares [{}]. \
             This crate relies on feature unification from other workspace members. \
             After unification, standalone builds will fail. \
             Fix: Add the missing features to {}'s Cargo.toml.",
            uf.member,
            uf.dep_name,
            undeclared_str.join(", "),
            declared_str,
            uf.member,
          )),
        });
      }
    }

    // Run validation
    let validation_results = self.validate_targets()?;

    // Defensive cleanup: avoid representing "no-op members" as pending changes.
    member_edits.retain(|_, edits| !edits.is_empty());

    Ok(UnificationPlan {
      workspace_deps,
      member_edits,
      member_paths,
      transitive_pins,
      validation_results,
      issues,
      computed_msrv,
      duplicates_cleaned,
      pruned_features,
      optional_features,
      version_mismatches,
      unused_deps,
      undeclared_features,
    })
  }

  /// Validate that the unification works for all targets
  fn validate_targets(&self) -> RailResult<Vec<ValidationResult>> {
    let targets = self.metadata.targets();
    let mut results = Vec::with_capacity(targets.len());

    for target in targets {
      // Simple check: does metadata load successfully for this target?
      // In production, you might want to run `cargo check --target=X`
      let success = self.metadata.get(target).is_some();

      results.push(ValidationResult {
        target: Arc::from(target),
        success,
        error: if !success {
          Some(Arc::from("Failed to load metadata"))
        } else {
          None
        },
      });
    }

    Ok(results)
  }

  // ============================================================================
  // Helper methods for detect_undeclared_features
  // ============================================================================

  /// Get workspace member package names
  fn workspace_member_names(&self) -> FxHashSet<String> {
    self
      .metadata
      .workspace_packages()
      .iter()
      .map(|p| p.name.to_string())
      .collect()
  }

  /// Build workspace baseline features from [workspace.dependencies]
  ///
  /// These are workspace policy - members don't need to re-declare them.
  fn workspace_baseline_features(&self) -> FxHashMap<String, FxHashSet<String>> {
    self
      .existing_workspace_deps
      .iter()
      .map(|(name, dep)| {
        let mut feats: FxHashSet<String> = dep.features.iter().cloned().collect();
        if dep.default_features {
          feats.insert("default".to_string());
        }
        (name.clone(), feats)
      })
      .collect()
  }

  /// Track which members contribute which features (beyond workspace baseline)
  ///
  /// Returns (feature_sources, feature_is_target_specific):
  /// - feature_sources: dep_name -> feature -> set of members that declare it
  /// - feature_is_target_specific: dep_name -> feature -> true if ALL declarations are target-specific
  fn track_feature_sources(
    &self,
    workspace_baseline: &FxHashMap<String, FxHashSet<String>>,
  ) -> (
    FxHashMap<String, FxHashMap<String, FxHashSet<String>>>,
    FxHashMap<String, FxHashMap<String, bool>>,
  ) {
    let mut feature_sources: FxHashMap<String, FxHashMap<String, FxHashSet<String>>> = FxHashMap::default();
    let mut feature_is_target_specific: FxHashMap<String, FxHashMap<String, bool>> = FxHashMap::default();

    for member in &self.manifests.members {
      for (dep_key, usages) in &member.dependencies {
        let baseline = workspace_baseline.get(&*dep_key.name);
        // Compute dep name string once per (member, dep) pair instead of per-feature
        let dep_name_str = dep_key.name.to_string();

        for usage in usages {
          let mut feats: FxHashSet<&String> = usage.unconditional_features.iter().collect();
          feats.extend(usage.conditional_features.iter());

          let is_target_specific = usage.target.is_some();

          for feat in feats {
            // Skip features in workspace baseline (workspace policy)
            if baseline.is_some_and(|b| b.contains(feat)) {
              continue;
            }

            // Clone dep_name_str once per feature instead of allocating twice
            feature_sources
              .entry(dep_name_str.clone())
              .or_default()
              .entry(feat.clone())
              .or_default()
              .insert(member.package_name.clone());

            // Track target-specificity: only consider target-specific if ALL declarations are
            let entry = feature_is_target_specific
              .entry(dep_name_str.clone())
              .or_default()
              .entry(feat.clone())
              .or_insert(true);
            if !is_target_specific {
              *entry = false;
            }
          }
        }
      }
    }

    (feature_sources, feature_is_target_specific)
  }

  /// Detect dependencies where resolved features exceed declared features
  ///
  /// This catches cases where a crate relies on Cargo's feature unification to
  /// "borrow" features from other workspace members. After workspace dependency
  /// unification, standalone builds (cargo test -p <crate>) will fail because
  /// the borrowed features are no longer available.
  ///
  /// Key distinctions:
  /// - **Workspace policy**: Features in `[workspace.dependencies]` are intentional
  ///   workspace-level declarations. Members using these don't need to re-declare.
  /// - **Member borrowing**: Features that only come from another member are
  ///   implicit coupling and should be flagged.
  /// - **Conditional features**: Features declared via `[features]` table (e.g.,
  ///   `my-feature = ["dep/feat"]`) count as declared - the author made an
  ///   intentional choice.
  fn detect_undeclared_features(&self) -> Vec<UndeclaredFeature> {
    let workspace_member_names = self.workspace_member_names();
    let workspace_baseline = self.workspace_baseline_features();
    let (feature_sources, feature_is_target_specific) = self.track_feature_sources(&workspace_baseline);

    let mut undeclared = Vec::with_capacity(16);
    let mut skipped_platform_features: Vec<(String, String, String)> = Vec::with_capacity(8);
    let mut skipped_workspace_member_deps: FxHashSet<String> = FxHashSet::default();

    // Find borrowed features for each member
    for member in &self.manifests.members {
      for (dep_key, usages) in &member.dependencies {
        // Skip workspace member deps - their optional features are intentional opt-ins
        if workspace_member_names.contains(&*dep_key.name) {
          skipped_workspace_member_deps.insert(dep_key.name.to_string());
          continue;
        }

        let Some(feat_sources) = feature_sources.get(&*dep_key.name) else {
          continue;
        };

        let target_specific_map = feature_is_target_specific.get(&*dep_key.name);

        for usage in usages {
          if let Some(borrowed) = self.find_borrowed_features_for_usage(
            member,
            dep_key,
            usage,
            feat_sources,
            target_specific_map,
            &mut skipped_platform_features,
          ) {
            undeclared.push(borrowed);
          }
        }
      }
    }

    // Log skipped items
    if !skipped_workspace_member_deps.is_empty() {
      progress!(
        "  Skipped undeclared feature detection for {} workspace member deps (features are opt-in)",
        skipped_workspace_member_deps.len()
      );
    }
    if !skipped_platform_features.is_empty() {
      progress!(
        "  Skipped {} platform-specific undeclared features (target-constrained declarations)",
        skipped_platform_features.len()
      );
    }

    undeclared
  }

  /// Find borrowed features for a single dependency usage
  #[allow(clippy::too_many_arguments)]
  fn find_borrowed_features_for_usage(
    &self,
    member: &crate::cargo::manifest_analyzer::ParsedManifest,
    dep_key: &crate::cargo::manifest_analyzer::DepKey,
    usage: &crate::cargo::manifest_analyzer::DepUsage,
    feat_sources: &FxHashMap<String, FxHashSet<String>>,
    target_specific_map: Option<&FxHashMap<String, bool>>,
    skipped_platform_features: &mut Vec<(String, String, String)>,
  ) -> Option<UndeclaredFeature> {
    // Get this usage's declared features
    let mut declared: FxHashSet<String> = usage.unconditional_features.iter().cloned().collect();
    declared.extend(usage.conditional_features.iter().cloned());
    if usage.default_features {
      declared.insert("default".to_string());
    }

    let mut borrowed_features: Vec<Arc<str>> = Vec::with_capacity(feat_sources.len());
    let mut borrowed_from_members: FxHashSet<Arc<str>> = FxHashSet::default();

    for (feat, sources) in feat_sources {
      // Skip if this member declares the feature itself
      if declared.contains(feat) {
        continue;
      }

      // Skip if configured to skip this feature pattern
      if self.config.should_skip_undeclared_feature(feat) {
        continue;
      }

      // Skip if feature is target-specific (all declarations have target constraints)
      if let Some(ts_map) = target_specific_map
        && ts_map.get(feat).copied().unwrap_or(false)
      {
        skipped_platform_features.push((member.package_name.clone(), dep_key.name.to_string(), feat.clone()));
        continue;
      }

      // This feature is borrowed from other members
      borrowed_features.push(Arc::from(feat.as_str()));
      for source in sources {
        if source != &member.package_name {
          borrowed_from_members.insert(Arc::from(source.as_str()));
        }
      }
    }

    if borrowed_features.is_empty() {
      return None;
    }

    borrowed_features.sort();
    let mut borrowed_from: Vec<Arc<str>> = borrowed_from_members.into_iter().collect();
    borrowed_from.sort();

    Some(UndeclaredFeature {
      member: Arc::from(member.package_name.as_str()),
      dep_name: Arc::clone(&dep_key.name),
      undeclared_features: borrowed_features,
      declared_features: declared.into_iter().map(|s| Arc::from(s.as_str())).collect(),
      resolved_features: feat_sources.keys().map(|s| Arc::from(s.as_str())).collect(),
      dep_kind: usage.kind,
      target: usage.target.as_ref().map(|t| Arc::from(t.as_str())),
      borrowed_from,
    })
  }
}

fn member_manifest_inherits_msrv(manifest_path: &Path) -> RailResult<bool> {
  let content =
    std::fs::read_to_string(manifest_path).context(format!("Failed to read {}", manifest_path.display()))?;
  let doc: toml_edit::DocumentMut = content
    .parse()
    .context(format!("Failed to parse {}", manifest_path.display()))?;

  let Some(pkg) = doc.get("package").and_then(|p| p.as_table()) else {
    // No [package] section (likely a virtual workspace member). Nothing to enforce.
    return Ok(true);
  };

  let Some(rv) = pkg.get("rust-version") else {
    return Ok(false);
  };

  let Some(rv_tbl) = rv.as_table_like() else {
    return Ok(false);
  };

  Ok(rv_tbl.get("workspace").and_then(|v| v.as_bool()) == Some(true))
}
