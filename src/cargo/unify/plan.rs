//! Unification plan display

use super::types::{FeatureSource, IssueType, UnificationPlan};

impl UnificationPlan {
  /// Generate human-readable summary of the unification plan
  pub fn summary(&self) -> String {
    let mut output = String::new();

    output.push_str("📦 Workspace Dependency Unification Plan\n\n");

    // Statistics
    output.push_str("📊 Statistics:\n");
    output.push_str(&format!("  Total dependencies analyzed: {}\n", self.stats.total_deps));
    output.push_str(&format!("  ✅ Ready to unify: {}\n", self.stats.unified_count));
    if self.stats.issue_count > 0 {
      output.push_str(&format!("  ⚠️  Issues found: {}\n", self.stats.issue_count));
    }
    if self.stats.compilations_saved > 0 {
      output.push_str(&format!(
        "  🚀 Estimated compilations eliminated: {}\n",
        self.stats.compilations_saved
      ));
    }
    output.push('\n');

    // Workspace dependencies - Cargo.toml-centric view
    if !self.workspace_deps.is_empty() {
      output.push_str("✅ Dependencies to add to workspace.dependencies:\n\n");

      for dep in &self.workspace_deps {
        // Header with package name and proc-macro indicator
        if dep.is_proc_macro {
          output.push_str(&format!("  📦 {} (proc-macro)\n", dep.name));
        } else {
          output.push_str(&format!("  📦 {}\n", dep.name));
        }

        // Show target-specific info if applicable
        if let Some(ref target) = dep.target {
          output.push_str(&format!("     Platform-specific: {}\n", target));
        }

        // TOML representation
        output.push_str("     ");
        if let Some(ref path) = dep.path {
          // Workspace member path dependency
          if dep.features.is_empty() && dep.default_features {
            output.push_str(&format!("{} = {{ path = \"{}\" }}", dep.name, path));
          } else {
            output.push_str(&format!("{} = {{ path = \"{}\"", dep.name, path));
            if !dep.features.is_empty() {
              output.push_str(&format!(
                ", features = [{}]",
                dep
                  .features
                  .iter()
                  .map(|f| format!("\"{}\"", f))
                  .collect::<Vec<_>>()
                  .join(", ")
              ));
            }
            if !dep.default_features {
              output.push_str(", default-features = false");
            }
            output.push_str(" }");
          }
        } else {
          // Version dependency
          if dep.features.is_empty() && dep.default_features {
            output.push_str(&format!("{} = \"{}\"", dep.name, dep.version_req));
          } else {
            output.push_str(&format!("{} = {{ version = \"{}\"", dep.name, dep.version_req));
            if !dep.features.is_empty() {
              output.push_str(&format!(
                ", features = [{}]",
                dep
                  .features
                  .iter()
                  .map(|f| format!("\"{}\"", f))
                  .collect::<Vec<_>>()
                  .join(", ")
              ));
            }
            if !dep.default_features {
              output.push_str(", default-features = false");
            }
            output.push_str(" }");
          }
        }
        output.push('\n');

        // Workspace members using this dependency
        output.push_str(&format!("     Used by: {}\n", dep.used_by.join(", ")));

        // Show dependency kinds if multiple
        if dep.dep_kinds.len() > 1 {
          let kinds: Vec<String> = dep
            .dep_kinds
            .iter()
            .map(|k| match k {
              cargo_metadata::DependencyKind::Normal => "dependencies".to_string(),
              cargo_metadata::DependencyKind::Development => "dev-dependencies".to_string(),
              cargo_metadata::DependencyKind::Build => "build-dependencies".to_string(),
              cargo_metadata::DependencyKind::Unknown => "unknown".to_string(),
            })
            .collect();
          output.push_str(&format!("     Sections: {}\n", kinds.join(", ")));
        }

        // Feature provenance - show WHY features are enabled
        if !dep.features.is_empty() && !dep.feature_provenance.is_empty() {
          output.push_str("\n     Features (why enabled):\n");

          for feature in &dep.features {
            if let Some(sources) = dep.feature_provenance.get(feature) {
              // Classify the sources
              let direct_members: Vec<_> = sources
                .iter()
                .filter_map(|s| match s {
                  FeatureSource::Direct { member } => Some(member.as_str()),
                  _ => None,
                })
                .collect();

              let has_default = sources.iter().any(|s| matches!(s, FeatureSource::Default));
              let has_all_features = sources.iter().any(|s| matches!(s, FeatureSource::AllFeatures));

              let transitive_chains: Vec<_> = sources
                .iter()
                .filter_map(|s| match s {
                  FeatureSource::Transitive { through } => Some(through.join(" → ")),
                  _ => None,
                })
                .collect();

              // Format the provenance
              if !direct_members.is_empty() {
                output.push_str(&format!(
                  "       ✓ \"{}\" - Direct ({})\n",
                  feature,
                  direct_members.join(", ")
                ));
              } else if has_default {
                output.push_str(&format!("       ✓ \"{}\" - Default features\n", feature));
              } else if !transitive_chains.is_empty() {
                output.push_str(&format!(
                  "       ⚠ \"{}\" - Transitive (via {})\n",
                  feature,
                  transitive_chains.first().unwrap()
                ));
              } else if has_all_features {
                output.push_str(&format!("       ⚠ \"{}\" - From --all-features flag\n", feature));
              }
            }
          }
        }

        // Fragmentation savings
        if dep.fragmentation_count > 1 {
          output.push_str(&format!(
            "\n     💡 Eliminates {} duplicate compilation{}\n",
            dep.fragmentation_count - 1,
            if dep.fragmentation_count > 2 { "s" } else { "" }
          ));
        }

        output.push('\n');
      }
    }

    // Separate issues by severity
    let (info_issues, warning_issues): (Vec<_>, Vec<_>) = self
      .issues
      .iter()
      .partition(|i| matches!(i.severity, super::types::IssueSeverity::Info));

    // Display non-info issues (warnings and hard blockers)
    if !warning_issues.is_empty() {
      output.push_str("⚠️  Issues requiring attention:\n\n");
      for issue in &warning_issues {
        output.push_str(&format!("  {} - ", issue.dep_name));
        match &issue.issue_type {
          IssueType::IncompatibleVersionRequirements { requirements } => {
            output.push_str("Incompatible version requirements\n");
            for (member, version) in requirements {
              output.push_str(&format!("    {} requires {}\n", member, version));
            }
          }
          IssueType::Renamed { original, renames } => {
            output.push_str(&format!("Renamed from {}\n", original));
            for (member, rename) in renames {
              output.push_str(&format!("    {} uses name: {}\n", member, rename));
            }
          }
          IssueType::PathDependency { paths } => {
            output.push_str("Path dependency\n");
            for (member, path) in paths {
              output.push_str(&format!("    {} uses path: {}\n", member, path));
            }
          }
          IssueType::AllTargetSpecific { targets } => {
            output.push_str(&format!("All uses are target-specific: {}\n", targets.join(", ")));
          }
          IssueType::MultipleResolvedVersions { versions } => {
            output.push_str(&format!("Multiple versions resolved: {}\n", versions.join(", ")));
          }
          IssueType::InconsistentDefaultFeatures {
            members_with_default,
            members_without_default,
          } => {
            output.push_str("Inconsistent default-features\n");
            output.push_str(&format!("    With defaults: {}\n", members_with_default.join(", ")));
            output.push_str(&format!(
              "    Without defaults: {}\n",
              members_without_default.join(", ")
            ));
          }
        }
        output.push_str(&format!("    Suggestion: {}\n\n", issue.suggestion));
      }
    }

    // Display info-level issues separately (FYI only)
    if !info_issues.is_empty() {
      output.push_str("ℹ️  Informational (not blockers):\n\n");
      for issue in &info_issues {
        output.push_str(&format!("  {} - ", issue.dep_name));
        match &issue.issue_type {
          IssueType::AllTargetSpecific { targets } => {
            output.push_str(&format!("All uses are target-specific: {}\n", targets.join(", ")));
          }
          _ => {
            // Should not happen, but handle gracefully
            output.push_str("Info issue\n");
          }
        }
        output.push_str(&format!("    Note: {}\n\n", issue.suggestion));
      }
    }

    // Transitive fragmentations (optimization opportunity)
    if !self.transitive_fragmentations.is_empty() {
      output.push_str("🔍 Optimization Opportunity: Transitive Feature Consolidation\n\n");

      let total_count = self.transitive_fragmentations.len();
      let workspace_count = if total_count > 10 { 10 } else { total_count };

      output.push_str(&format!(
        "  Found {} transitive-only crate{} with fragmented feature sets.\n\n",
        total_count,
        if total_count == 1 { "" } else { "s" }
      ));

      output.push_str("  What this means:\n");
      output.push_str("    • These crates don't appear in any Cargo.toml [dependencies]\n");
      output.push_str("    • They're pulled in transitively by your direct dependencies\n");
      output.push_str("    • Cargo compiles them multiple times with different features\n");
      output.push_str("    • This is the ~5% that workspace.dependencies can't automatically unify\n\n");

      for frag in &self.transitive_fragmentations {
        output.push_str(&frag.format_explanation());
        output.push('\n');
      }

      output.push_str(&format!(
        "  💡 Recommended for workspaces with >{} crates\n\n",
        workspace_count
      ));

      output.push_str("  To consolidate (optional):\n");
      output.push_str("    cargo rail unify --consolidate-transitives\n\n");
      output.push_str("  Or configure in rail.toml:\n");
      output.push_str("    [unify]\n");
      output.push_str("    consolidate_transitive_features = true\n");
      output.push_str("    transitive_feature_host = \"auto\"  # Smart selection of host crate\n\n");
    }

    output
  }
}
