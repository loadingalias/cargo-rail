//! Unification plan display

use super::types::{IssueType, UnificationPlan};

impl UnificationPlan {
  /// Generate human-readable summary of the unification plan
  pub fn summary(&self) -> String {
    let mut output = String::new();

    output.push_str("📦 Workspace Dependency Unification Plan\n\n");

    // Statistics
    output.push_str("📊 Statistics:\n");
    output.push_str(&format!("  Total dependencies: {}\n", self.stats.total_deps));
    output.push_str(&format!("  To unify: {}\n", self.stats.unified_count));
    output.push_str(&format!("  Issues: {}\n", self.stats.issue_count));
    output.push_str(&format!("  Compilations saved: {}\n\n", self.stats.compilations_saved));

    // Workspace dependencies
    if !self.workspace_deps.is_empty() {
      output.push_str("✅ Dependencies to unify:\n\n");
      for dep in &self.workspace_deps {
        output.push_str(&format!("  {} = ", dep.name));

        // Check if this is a path dependency
        if let Some(ref path) = dep.path {
          // Workspace member path dependency
          if dep.features.is_empty() && dep.default_features {
            output.push_str(&format!("{{ path = \"{}\" }}", path));
          } else {
            output.push_str("{ ");
            output.push_str(&format!("path = \"{}\"", path));
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
            output.push_str(&format!("\"{}\"", dep.version_req));
          } else {
            output.push_str("{ ");
            output.push_str(&format!("version = \"{}\"", dep.version_req));
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

        output.push_str(&format!("\n    Used by: {}\n", dep.used_by.join(", ")));

        // Show dependency kinds (normal, dev, build)
        if !dep.dep_kinds.is_empty() {
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
          output.push_str(&format!("    Used as: {}\n", kinds.join(", ")));
        }

        if dep.fragmentation_count > 1 {
          output.push_str(&format!(
            "    Eliminates {} duplicate compilations\n",
            dep.fragmentation_count - 1
          ));
        }
        output.push('\n');
      }
    }

    // Issues
    if !self.issues.is_empty() {
      output.push_str("⚠️  Issues requiring attention:\n\n");
      for issue in &self.issues {
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

    // Transitive fragmentations (informational)
    if !self.transitive_fragmentations.is_empty() {
      output.push_str("ℹ️  Transitive-only crates with fragmented features:\n\n");
      output.push_str(&format!(
        "  Found {} transitive-only crate(s) with multiple feature sets.\n",
        self.transitive_fragmentations.len()
      ));
      output.push_str("  These crates:\n");
      output.push_str("    • Don't appear in any Cargo.toml [dependencies]\n");
      output.push_str("    • Resolve with different features in different contexts\n");
      output.push_str("    • Cargo's model can't enforce a single feature set without explicit deps\n\n");
      output.push_str("  Current state: ~95% unified (this is the remaining ~5%)\n\n");

      for frag in &self.transitive_fragmentations {
        output.push_str(&frag.format_explanation());
        output.push('\n');
      }

      output.push_str("  To pin these crates under workspace control:\n");
      output.push_str("    cargo rail unify apply --pin-transitives\n\n");
      output.push_str("  Or configure in .config/rust/rail.toml:\n");
      output.push_str("    [unify]\n");
      output.push_str("    pin_transitives = true\n");
      output.push_str("    pin_hosts = [\"my-meta-crate\"]  # optional\n\n");
    }

    output
  }
}
