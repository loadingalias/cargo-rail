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
use semver::VersionReq;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Analyzes workspace for dependency unifications
pub struct UnifyAnalyzer {
  metadata: MultiTargetMetadata,
  manifests: ManifestAnalyzer,
  /// Configuration for unification behavior (can be overridden at runtime)
  pub config: UnifyConfig,
  /// Existing workspace dependencies (already in [workspace.dependencies])
  existing_workspace_deps: HashMap<String, ExistingWorkspaceDep>,
  /// Workspace root path (for path normalization)
  workspace_root: PathBuf,
}

impl UnifyAnalyzer {
  /// Create a new analyzer from workspace context
  pub fn new(ctx: &WorkspaceContext) -> RailResult<Self> {
    // Get multi-target metadata (lazily loaded on first access)
    // This clones the inner data (required for ownership by this analyzer)
    let metadata = (*ctx.multi_target_metadata()?).clone();

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

  /// Analyze workspace and generate unification plan
  pub fn analyze(&self) -> RailResult<UnificationPlan> {
    let mut workspace_deps = Vec::new();
    let mut member_edits: HashMap<String, Vec<MemberEdit>> = HashMap::new();
    let mut member_paths: HashMap<String, PathBuf> = HashMap::new();
    let mut issues = Vec::new();
    let mut duplicates_cleaned = Vec::new();
    let mut version_mismatches = Vec::new();

    // Pre-compute workspace member names for reuse
    let workspace_member_names: HashSet<String> = self
      .metadata
      .workspace_packages()
      .iter()
      .map(|pkg| pkg.name.to_string())
      .collect();

    // Build member_paths mapping from metadata
    for pkg in self.metadata.workspace_packages() {
      let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
      member_paths.insert(pkg.name.to_string(), manifest_path);
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
              dep_name: dep_key.name.to_string(),
              severity: IssueSeverity::Warning,
              message: format!(
                "Multiple major versions detected (majors: {}) - skipping unification. \
                 Consider consolidating to a single major version to reduce duplicate compilation, \
                 or set major_version_conflict = \"bump\" to force unification to highest version.",
                versions_str.join(", ")
              ),
            });
            continue; // Skip this dependency, continue with others
          }
          MajorVersionConflict::Bump => {
            // Emit warning but continue with unification to highest version
            issues.push(UnifyIssue {
              dep_name: dep_key.name.to_string(),
              severity: IssueSeverity::Warning,
              message: format!(
                "Multiple major versions detected (majors: {}) - bumping to highest resolved version. \
                 This may break crates depending on older major versions.",
                versions_str.join(", ")
              ),
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
              dep_name: dep_key.name.to_string(),
              severity: IssueSeverity::Warning,
              message: "Has exact version pin (=x.y.z) - converting to caret (^). \
                        Consider adding to unify.exclude or setting exact_pin_handling = \"skip\""
                .to_string(),
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
      let unique_versions: HashSet<_> = versions.values().collect();

      // Always use highest version (cargo's resolver already picks highest compatible)
      // When targets resolve to different versions, we unify to highest
      let version = match unique_versions.iter().max() {
        Some(max) if unique_versions.len() > 1 => {
          // Multiple versions found across targets - use highest
          // This is the "silent win" - we unify duplicates automatically
          let mut versions_found: Vec<_> = unique_versions.iter().map(|v| v.to_string()).collect();
          versions_found.sort();
          duplicates_cleaned.push(DuplicateCleanup {
            dep_name: dep_key.name.to_string(),
            versions_found,
            selected_version: max.to_string(),
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
              member: usage.used_by.to_string(),
              dep_name: dep_key.name.to_string(),
              member_version: declared.clone(),
              workspace_version: ws_version.clone(),
            });

            // Add as issue based on strictness config
            let severity = if self.config.strict_version_compat {
              IssueSeverity::Error
            } else {
              IssueSeverity::Warning
            };
            issues.push(UnifyIssue {
              dep_name: dep_key.name.to_string(),
              severity,
              message: format!(
                "{} declares \"{}\" but workspace.dependencies has \"{}\". \
                 Converting to workspace = true may break this crate.",
                usage.used_by, declared, ws_version
              ),
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

      // Check if target-specific
      let targets_with_dep = self.metadata.targets_with_dep(&dep_key.name);
      let all_targets = self.metadata.targets();
      let target = if targets_with_dep.len() < all_targets.len() {
        // This is target-specific, try to infer cfg
        Some(self.infer_target_cfg(&targets_with_dep))
      } else {
        None
      };

      // Get users - convert Arc<str> to String for output
      let users: HashSet<String> = usage_sites.iter().map(|u| u.used_by.to_string()).collect();

      // Check include_paths config before processing path dependencies
      let dep_path: Option<PathBuf> = if self.config.include_paths {
        // Check if any usage has a path (path dependencies)
        // If so, check if the dep is a workspace member to include path in workspace.dependencies
        usage_sites.iter().find_map(|u| {
          u.path.as_ref().and_then(|p| {
            if !workspace_member_names.contains(&*dep_key.name) {
              return None;
            }
            // Normalize path relative to workspace root using helper
            match &u.manifest_path {
              Some(manifest_path) => Some(self.normalize_dep_path(manifest_path, p)),
              None => Some(PathBuf::from(p)), // No manifest_path - use as-is (shouldn't happen)
            }
          })
        })
      } else {
        None // include_paths = false, skip all path deps
      };

      // Compute the unified features (this is what workspace.dependencies should have)
      let computed_features: Vec<String> = features.into_iter().collect();

      // Check if this dep already exists in workspace.dependencies
      let existing_dep = self.existing_workspace_deps.get(&*dep_key.name);

      // Determine if we need to update workspace.dependencies
      // Update if: dep doesn't exist, OR features differ, OR default-features differs, OR has path
      let needs_workspace_update = match existing_dep {
        None => true, // Doesn't exist, need to add
        Some(existing) => {
          // Check if features are different
          let mut existing_features: Vec<String> = existing.features.clone();
          let mut computed: Vec<String> = computed_features.clone();
          existing_features.sort();
          computed.sort();

          // Also check if existing has path but we found one now
          let path_differs = dep_path.is_some() && existing.path.is_none();

          existing_features != computed || existing.default_features != default_features || path_differs
        }
      };

      // The features to use for local feature calculation
      // Use computed features (which will be what goes in workspace.dependencies)
      let workspace_features = computed_features.clone();

      // Create unified dep if workspace needs update
      if needs_workspace_update {
        let unified = UnifiedDep {
          name: dep_key.name.to_string(),
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
        let local_features: Vec<_> = usage
          .unconditional_features
          .iter()
          .filter(|f| !workspace_features.contains(f))
          .cloned()
          .collect();

        // Use the cargo_toml_key from the usage - this is the actual key in Cargo.toml
        // For renamed deps like `old_serde = { package = "serde" }`, this is "old_serde"
        // This is critical for include_renamed mode where usage_sites are aggregated
        // across multiple dep_keys for the same package
        let edit = MemberEdit::UseWorkspace {
          dep_name: usage.cargo_toml_key.to_string(),
          dep_kind: usage.kind,
          target: usage.target.clone(), // Preserve target for correct section
          local_features,
          is_optional: usage.optional,
        };

        member_edits.entry(usage.used_by.to_string()).or_default().push(edit);
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
          dep_name: "rust-version".to_string(),
          severity: IssueSeverity::Warning,
          message: "enforce_msrv_inheritance is enabled but unify.msrv is false; skipping MSRV inheritance enforcement"
            .to_string(),
        });
      } else if computed_msrv.is_none() {
        issues.push(UnifyIssue {
          dep_name: "rust-version".to_string(),
          severity: IssueSeverity::Warning,
          message:
            "enforce_msrv_inheritance is enabled but no workspace MSRV could be determined; skipping MSRV inheritance enforcement"
              .to_string(),
        });
      } else {
        progress!("Enforcing MSRV inheritance on workspace members...");
        for (member_name, manifest_path) in &member_paths {
          if member_manifest_inherits_msrv(manifest_path)? {
            continue;
          }
          member_edits
            .entry(member_name.clone())
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
        member_edits.entry(member).or_default().extend(edits);
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
          dep_name: uf.dep_name.clone(),
          dep_kind: uf.dep_kind,
          target: uf.target.clone(),
          features_to_add: uf.undeclared_features.clone(),
        };
        member_edits.entry(uf.member.clone()).or_default().push(edit);
      }
    } else {
      // Add issues for undeclared features (warn mode)
      for uf in &undeclared_features {
        issues.push(UnifyIssue {
          dep_name: uf.dep_name.clone(),
          severity: IssueSeverity::Warning,
          message: format!(
            "{} uses {} features [{}] but only declares [{}]. \
             This crate relies on feature unification from other workspace members. \
             After unification, standalone builds will fail. \
             Fix: Add the missing features to {}'s Cargo.toml.",
            uf.member,
            uf.dep_name,
            uf.undeclared_features.join(", "),
            if uf.declared_features.is_empty() {
              "none".to_string()
            } else {
              uf.declared_features.join(", ")
            },
            uf.member,
          ),
        });
      }
    }

    // Run validation
    let validation_results = self.validate_targets()?;

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
    let mut results = Vec::new();

    for target in self.metadata.targets() {
      // Simple check: does metadata load successfully for this target?
      // In production, you might want to run `cargo check --target=X`
      let success = self.metadata.get(target).is_some();

      results.push(ValidationResult {
        target: target.to_string(),
        success,
        error: if !success {
          Some("Failed to load metadata".to_string())
        } else {
          None
        },
      });
    }

    Ok(results)
  }

  /// Infer a cfg expression from a list of targets
  fn infer_target_cfg(&self, targets: &[String]) -> String {
    // Simple heuristic: if all Unix-like, use cfg(unix)
    if targets.iter().all(|t| t.contains("linux") || t.contains("darwin")) {
      "cfg(unix)".to_string()
    } else if targets.iter().all(|t| t.contains("windows")) {
      "cfg(windows)".to_string()
    } else {
      // Fall back to listing all targets
      format!(
        "cfg(any({}))",
        targets
          .iter()
          .map(|t| format!("target = \"{}\"", t))
          .collect::<Vec<_>>()
          .join(", ")
      )
    }
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
  ///
  /// Example:
  /// - crate A declares: `backoff = "0.4"` (no features)
  /// - crate B declares: `backoff = { version = "0.4", features = ["tokio"] }`
  /// - crate A uses `backoff::future::retry()` which requires `features = ["futures"]`
  /// - When building the whole workspace, A gets `futures` from B via unification
  /// - After unification, A's standalone build fails
  fn detect_undeclared_features(&self) -> Vec<UndeclaredFeature> {
    let mut undeclared = Vec::new();

    // Get workspace baseline (features declared in [workspace.dependencies])
    // These are workspace policy - members don't need to re-declare them
    let workspace_baseline: HashMap<String, HashSet<String>> = self
      .existing_workspace_deps
      .iter()
      .map(|(name, dep)| {
        let mut feats: HashSet<String> = dep.features.iter().cloned().collect();
        if dep.default_features {
          feats.insert("default".to_string());
        }
        (name.clone(), feats)
      })
      .collect();

    // Build a map of what each member explicitly declares for each dependency
    // Include both unconditional_features AND conditional_features (from [features] table)
    // Also track which features each member contributes (for borrowed_from)
    let mut member_declared_features: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();

    for member in &self.manifests.members {
      let mut dep_features: HashMap<String, HashSet<String>> = HashMap::new();
      for (dep_key, usage) in &member.dependencies {
        let mut feats: HashSet<String> = usage.unconditional_features.iter().cloned().collect();
        // Include conditional features (from [features] table like `my-feat = ["dep/feature"]`)
        // The author made an intentional choice - this counts as declared
        feats.extend(usage.conditional_features.iter().cloned());
        // If default_features = true (default), add "default"
        if usage.default_features {
          feats.insert("default".to_string());
        }
        dep_features.insert(dep_key.name.to_string(), feats);
      }
      member_declared_features.insert(member.package_name.clone(), dep_features);
    }

    // Track which members contribute which features (beyond workspace baseline)
    // This powers the `borrowed_from` field for transparency
    // Structure: dep_name -> feature -> set of member names
    let mut feature_sources: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();

    for (member_name, dep_features) in &member_declared_features {
      for (dep_name, features) in dep_features {
        let baseline = workspace_baseline.get(dep_name);

        for feat in features {
          // Skip features that are in workspace baseline (workspace policy)
          if baseline.is_some_and(|b| b.contains(feat)) {
            continue;
          }

          feature_sources
            .entry(dep_name.clone())
            .or_default()
            .entry(feat.clone())
            .or_default()
            .insert(member_name.clone());
        }
      }
    }

    // Find borrowed features for each member
    // A feature is borrowed if:
    // - It's contributed by OTHER members (not this member)
    // - It's NOT in workspace baseline (workspace policy doesn't count as borrowing)
    // - It passes the skip filter (configurable via skip_undeclared_patterns)
    for member in &self.manifests.members {
      for (dep_key, usage) in &member.dependencies {
        let Some(feat_sources) = feature_sources.get(&*dep_key.name) else {
          continue;
        };

        // Get this member's declared features (unconditional + conditional)
        let mut declared: HashSet<String> = usage.unconditional_features.iter().cloned().collect();
        declared.extend(usage.conditional_features.iter().cloned());
        if usage.default_features {
          declared.insert("default".to_string());
        }

        // Find features this member borrows from others
        let mut borrowed_features: Vec<String> = Vec::new();
        let mut borrowed_from_members: HashSet<String> = HashSet::new();

        for (feat, sources) in feat_sources {
          // Skip if this member declares the feature itself
          if declared.contains(feat) {
            continue;
          }

          // Skip if configured to skip this feature pattern
          if self.config.should_skip_undeclared_feature(feat) {
            continue;
          }

          // This feature is borrowed from other members
          borrowed_features.push(feat.clone());
          for source in sources {
            if source != &member.package_name {
              borrowed_from_members.insert(source.clone());
            }
          }
        }

        if !borrowed_features.is_empty() {
          borrowed_features.sort();
          let mut borrowed_from: Vec<String> = borrowed_from_members.into_iter().collect();
          borrowed_from.sort();

          undeclared.push(UndeclaredFeature {
            member: member.package_name.clone(),
            dep_name: dep_key.name.to_string(),
            undeclared_features: borrowed_features,
            declared_features: declared.into_iter().collect(),
            resolved_features: feat_sources.keys().cloned().collect(),
            dep_kind: usage.kind,
            target: usage.target.clone(),
            borrowed_from,
          });
        }
      }
    }

    undeclared
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
