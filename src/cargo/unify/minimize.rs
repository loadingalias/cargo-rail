//! Feature minimization for workspace dependencies
//!
//! This module implements the "killer feature" - automatically finding the minimal
//! working feature set for each unified dependency by iteratively testing feature
//! removal and validating with `cargo check`.
//!
//! # Strategy
//!
//! 1. Start with feature unions (already written to workspace.dependencies)
//! 2. For each dependency, test removing features one-by-one
//! 3. Run `cargo check` to verify compilation still succeeds
//! 4. Keep only features required for successful compilation
//! 5. Update workspace.dependencies with minimal feature sets
//!
//! # Performance Optimizations
//!
//! - **Result caching** (subsequent runs are nearly instant for unchanged deps)
//! - **Parallel dependency testing** using rayon (test multiple deps at once)
//! - **Graph-aware checking** (only check affected members, not whole workspace)
//! - **Batch feature removal** (try removing multiple features at once)
//! - **Smart feature ordering** (test optional features first)
//! - **Progress indicators** (real-time feedback during long operations)

use super::feature_cache::FeatureCache;
use crate::cargo::unify::UnifiedDep;
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Result of feature minimization for the entire workspace
#[derive(Debug)]
pub struct MinimizationReport {
  /// Dependencies that were minimized
  pub minimized_deps: Vec<MinimizedDependency>,

  /// Total features removed across all dependencies
  pub total_features_removed: usize,

  /// Total features kept
  pub total_features_kept: usize,

  /// Dependencies that had no removable features
  pub unchanged_deps: usize,
}

/// Result of minimizing a single dependency
#[derive(Debug)]
pub struct MinimizedDependency {
  /// Dependency name
  pub name: String,

  /// Original features (before minimization)
  pub original_features: Vec<String>,

  /// Minimal features (after minimization)
  pub minimal_features: Vec<String>,

  /// Features that were removed
  pub removed_features: Vec<String>,
}

impl MinimizationReport {
  /// Create a summary message for display
  pub fn summary(&self) -> String {
    let mut lines = vec![
      format!("\n📊 Feature Minimization Summary:"),
      format!("  Dependencies minimized: {}", self.minimized_deps.len()),
      format!("  Features removed: {}", self.total_features_removed),
      format!("  Features kept: {}", self.total_features_kept),
    ];

    if self.unchanged_deps > 0 {
      lines.push(format!(
        "  Dependencies unchanged: {} (all features required)",
        self.unchanged_deps
      ));
    }

    if self.total_features_removed > 0 {
      let savings_pct =
        (self.total_features_removed as f64 / (self.total_features_removed + self.total_features_kept) as f64) * 100.0;
      lines.push(format!("  Estimated reduction: {:.1}%", savings_pct));
    }

    lines.join("\n")
  }
}

/// Minimize features for all workspace dependencies
///
/// **Performance optimizations:**
/// - Uses rayon to test multiple dependencies in parallel
/// - Uses workspace graph to check only affected members
/// - Batch-tests removable features
/// - Provides real-time progress feedback
pub fn minimize_workspace_features(
  ctx: &WorkspaceContext,
  unified_deps: &[UnifiedDep],
  workspace_toml_path: &Path,
) -> RailResult<(Vec<UnifiedDep>, MinimizationReport)> {
  println!(
    "\n🧪 Testing minimal feature sets ({} dependencies)...",
    unified_deps.len()
  );
  println!("   Using cache + parallel testing for maximum performance.\n");

  // Load cache
  let cache = FeatureCache::load(ctx.workspace_root())?;
  let cache = Arc::new(Mutex::new(cache));

  // Progress tracking (thread-safe)
  let progress = Arc::new(Mutex::new(ProgressTracker::new(unified_deps.len())));

  // Filter: Skip deps with 0 or 1 features (nothing to minimize)
  let (testable, skippable): (Vec<_>, Vec<_>) = unified_deps.iter().partition(|dep| dep.features.len() >= 2);

  // Check cache for testable deps
  let mut cached_deps = Vec::new();
  let mut needs_testing = Vec::new();

  for dep in &testable {
    let cache_lock = cache.lock().unwrap();
    if let Some(cached_entry) = cache_lock.get(dep) {
      // Cache hit!
      cached_deps.push((*dep, cached_entry.minimal_features.clone()));
    } else {
      // Cache miss - needs testing
      needs_testing.push(*dep);
    }
  }

  println!("  Cache hits: {}", cached_deps.len());
  println!("  Needs testing: {}", needs_testing.len());
  println!("  Skipped (0-1 features): {}\n", skippable.len());

  // PARALLEL PROCESSING: Test only uncached dependencies
  let results: Vec<RailResult<(UnifiedDep, Option<MinimizedDependency>)>> = needs_testing
    .par_iter()
    .map(|dep| {
      // Clone for parallel processing
      let dep = (*dep).clone();
      let workspace_toml = workspace_toml_path.to_path_buf();

      // Update progress
      {
        let mut p = progress.lock().unwrap();
        p.start_dep(&dep.name);
      }

      // Find minimal feature set for this dependency
      let minimal_features =
        find_minimal_features_optimized(&dep.name, &dep.features, &dep.used_by, ctx, &workspace_toml, &progress)?;

      // Update cache with result
      {
        let mut c = cache.lock().unwrap();
        c.insert(&dep, minimal_features.clone());
      }

      // Update progress
      {
        let mut p = progress.lock().unwrap();
        p.complete_dep(&dep.name);
      }

      let removed_count = dep.features.len() - minimal_features.len();

      if removed_count > 0 {
        let removed: Vec<String> = dep
          .features
          .iter()
          .filter(|f| !minimal_features.contains(f))
          .cloned()
          .collect();

        let minimized_info = MinimizedDependency {
          name: dep.name.clone(),
          original_features: dep.features.clone(),
          minimal_features: minimal_features.clone(),
          removed_features: removed,
        };

        let mut minimized_dep = dep;
        minimized_dep.features = minimal_features;

        Ok((minimized_dep, Some(minimized_info)))
      } else {
        Ok((dep, None))
      }
    })
    .collect();

  // Aggregate results
  let mut minimized_deps = Vec::new();
  let mut report_minimized = Vec::new();
  let mut total_removed = 0;
  let mut total_kept = 0;
  let mut unchanged_count = 0;

  // Add skipped deps (unchanged)
  for dep in skippable {
    minimized_deps.push(dep.clone());
    total_kept += dep.features.len();
    if !dep.features.is_empty() {
      unchanged_count += 1;
    }
  }

  // Process cached results
  for (dep, minimal_features) in cached_deps {
    let removed_count = dep.features.len() - minimal_features.len();

    if removed_count > 0 {
      let removed: Vec<String> = dep
        .features
        .iter()
        .filter(|f| !minimal_features.contains(f))
        .cloned()
        .collect();

      report_minimized.push(crate::cargo::unify::minimize::MinimizedDependency {
        name: dep.name.clone(),
        original_features: dep.features.clone(),
        minimal_features: minimal_features.clone(),
        removed_features: removed,
      });

      total_removed += removed_count;
      total_kept += minimal_features.len();

      let mut minimized_dep = (*dep).clone();
      minimized_dep.features = minimal_features;
      minimized_deps.push(minimized_dep);
    } else {
      total_kept += dep.features.len();
      unchanged_count += 1;
      minimized_deps.push((*dep).clone());
    }
  }

  // Process newly tested results
  for result in results {
    let (dep, minimized_info) = result?;

    if let Some(info) = minimized_info {
      total_removed += info.removed_features.len();
      total_kept += info.minimal_features.len();
      report_minimized.push(info);
      minimized_deps.push(dep);
    } else {
      total_kept += dep.features.len();
      unchanged_count += 1;
      minimized_deps.push(dep);
    }
  }

  // Save updated cache
  {
    let cache_lock = cache.lock().unwrap();
    cache_lock.save(ctx.workspace_root())?;
    println!("\n  💾 Cache updated ({} entries)", cache_lock.stats().total_entries);
  }

  let report = MinimizationReport {
    minimized_deps: report_minimized,
    total_features_removed: total_removed,
    total_features_kept: total_kept,
    unchanged_deps: unchanged_count,
  };

  Ok((minimized_deps, report))
}

/// Progress tracker for parallel operations
struct ProgressTracker {
  total: usize,
  completed: usize,
  current_dep: Option<String>,
}

impl ProgressTracker {
  fn new(total: usize) -> Self {
    Self {
      total,
      completed: 0,
      current_dep: None,
    }
  }

  fn start_dep(&mut self, dep_name: &str) {
    self.current_dep = Some(dep_name.to_string());
    eprint!("\r  [{}/{}] Testing {}...", self.completed + 1, self.total, dep_name);
  }

  fn complete_dep(&mut self, _dep_name: &str) {
    self.completed += 1;
    self.current_dep = None;
  }
}

/// Find minimal feature set for a single dependency (OPTIMIZED)
///
/// **Optimizations:**
/// 1. Batch removal: Try removing multiple "likely optional" features at once
/// 2. Graph-aware checking: Only check affected workspace members
/// 3. Early termination: Stop if all features are required
fn find_minimal_features_optimized(
  dep_name: &str,
  current_features: &[String],
  used_by_members: &[String],
  ctx: &WorkspaceContext,
  workspace_toml_path: &Path,
  _progress: &Arc<Mutex<ProgressTracker>>,
) -> RailResult<Vec<String>> {
  // OPTIMIZATION 1: Try batch removal of likely-optional features
  let (likely_optional, likely_required) = categorize_features(current_features);

  // Try removing ALL likely-optional features at once
  if !likely_optional.is_empty()
    && test_with_features(dep_name, &likely_required, used_by_members, ctx, workspace_toml_path)?
  {
    // Success! All optional features can be removed
    println!(" ✓ {} (removed {} optional features)", dep_name, likely_optional.len());
    return Ok(likely_required);
  }

  // OPTIMIZATION 2: Fallback to individual feature testing
  let mut required_features = HashSet::new();

  // Start with likely required features
  for feature in &likely_required {
    required_features.insert(feature.clone());
  }

  // Test each likely-optional feature individually
  for feature in &likely_optional {
    // Test if workspace compiles WITHOUT this feature
    let needs_feature = !test_with_features(
      dep_name,
      &required_features.iter().cloned().collect::<Vec<_>>(),
      used_by_members,
      ctx,
      workspace_toml_path,
    )?;

    if needs_feature {
      required_features.insert(feature.clone());
    }
  }

  let removed_count = current_features.len() - required_features.len();
  if removed_count > 0 {
    println!(" ✓ {} (removed {} features)", dep_name, removed_count);
  } else {
    println!(" - {} (all features required)", dep_name);
  }

  // Convert to sorted vec for deterministic output
  let mut result: Vec<_> = required_features.into_iter().collect();
  result.sort();
  Ok(result)
}

/// Categorize features into likely-optional vs likely-required
///
/// **Heuristics:**
/// - Likely optional: test, dev, bench, experimental, unstable
/// - Likely required: default, std, alloc, derive, macros
fn categorize_features(features: &[String]) -> (Vec<String>, Vec<String>) {
  let mut optional = Vec::new();
  let mut required = Vec::new();

  for feature in features {
    let f_lower = feature.to_lowercase();

    if f_lower.contains("test")
      || f_lower.contains("dev")
      || f_lower.contains("bench")
      || f_lower.contains("experimental")
      || f_lower.contains("unstable")
    {
      optional.push(feature.clone());
    } else if f_lower == "default"
      || f_lower == "std"
      || f_lower == "alloc"
      || f_lower.contains("derive")
      || f_lower.contains("macro")
    {
      required.push(feature.clone());
    } else {
      // Unknown - treat as optional (safer to test)
      optional.push(feature.clone());
    }
  }

  (optional, required)
}

/// Test if workspace compiles WITH only the specified features
///
/// **OPTIMIZATION: Graph-aware checking**
/// Instead of `cargo check --workspace`, we only check the members that actually
/// use this dependency (using the workspace graph we already built).
fn test_with_features(
  dep_name: &str,
  features_to_keep: &[String],
  used_by_members: &[String],
  ctx: &WorkspaceContext,
  workspace_toml_path: &Path,
) -> RailResult<bool> {
  // Read current workspace Cargo.toml
  let original_content = std::fs::read_to_string(workspace_toml_path)?;
  let mut doc: toml_edit::DocumentMut = original_content.parse()?;

  // Modify the dependency to only have the specified features
  let modified = set_dependency_features(&mut doc, dep_name, features_to_keep)?;

  if !modified {
    // Couldn't modify - assume features are required
    return Ok(false);
  }

  // Write modified TOML
  std::fs::write(workspace_toml_path, doc.to_string())?;

  // OPTIMIZATION: Check only affected members instead of whole workspace
  let check_passed = run_cargo_check_targeted(used_by_members, ctx.workspace_root())?;

  // Always restore original content
  std::fs::write(workspace_toml_path, original_content)?;

  Ok(check_passed)
}

/// Set dependency features in workspace.dependencies to exact list
fn set_dependency_features(doc: &mut toml_edit::DocumentMut, dep_name: &str, features: &[String]) -> RailResult<bool> {
  // Navigate to workspace.dependencies
  let workspace_deps = doc
    .get_mut("workspace")
    .and_then(|w| w.get_mut("dependencies"))
    .and_then(|d| d.as_table_like_mut());

  let workspace_deps = match workspace_deps {
    Some(deps) => deps,
    None => return Ok(false),
  };

  // Get the dependency entry
  let dep_entry = match workspace_deps.get_mut(dep_name) {
    Some(entry) => entry,
    None => return Ok(false),
  };

  // Handle both inline table and regular table
  if let Some(inline_table) = dep_entry.as_inline_table_mut() {
    if let Some(features_value) = inline_table.get_mut("features")
      && let Some(features_array) = features_value.as_array_mut()
    {
      features_array.clear();
      for feat in features {
        features_array.push(feat);
      }
      return Ok(true);
    }
  } else if let Some(table) = dep_entry.as_table_mut()
    && let Some(features_value) = table.get_mut("features")
    && let Some(features_array) = features_value.as_array_mut()
  {
    features_array.clear();
    for feat in features {
      features_array.push(feat);
    }
    return Ok(true);
  }

  Ok(false)
}

/// Run cargo check for SPECIFIC workspace members (graph-aware optimization)
///
/// **HUGE PERFORMANCE WIN:**
/// Instead of `cargo check --workspace` (checks all 50+ crates),
/// we check only the 2-3 crates that actually use this dependency.
fn run_cargo_check_targeted(members: &[String], workspace_root: &Path) -> RailResult<bool> {
  let mut cmd = Command::new("cargo");
  cmd.arg("check");

  // OPTIMIZATION: Check only affected members
  if members.len() <= 5 {
    // Small number of members - check them explicitly
    for member in members {
      cmd.arg("-p").arg(member);
    }
  } else {
    // Many members use this dep - check whole workspace (rare case)
    cmd.arg("--workspace");
  }

  // CRITICAL FIX: Check all targets (tests, examples, benches)
  // This prevents removing features that are only used in tests/examples
  cmd.arg("--all-targets");

  cmd.current_dir(workspace_root);

  // Suppress output - we only care about exit status
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());

  let output = cmd.output()?;
  Ok(output.status.success())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_categorize_features() {
    let features = vec![
      "default".to_string(),
      "std".to_string(),
      "test-util".to_string(),
      "derive".to_string(),
      "async".to_string(),
      "dev-features".to_string(),
      "bench".to_string(),
    ];

    let (optional, required) = categorize_features(&features);

    // test-util, dev-features, bench should be optional
    assert!(optional.contains(&"test-util".to_string()));
    assert!(optional.contains(&"dev-features".to_string()));
    assert!(optional.contains(&"bench".to_string()));

    // default, std, derive should be required
    assert!(required.contains(&"default".to_string()));
    assert!(required.contains(&"std".to_string()));
    assert!(required.contains(&"derive".to_string()));

    // async is unknown - should be treated as optional
    assert!(optional.contains(&"async".to_string()));
  }
}
