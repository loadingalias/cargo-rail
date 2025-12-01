//! Markdown report generation for unification plans
//!
//! Generates human-readable reports from `UnificationPlan` results.

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::unify_types::{IssueSeverity, UnificationPlan, UnusedReason};
use crate::error::RailResult;

/// Generate a markdown report from the unification plan
pub struct UnifyReport;

impl UnifyReport {
  /// Generates a markdown report from the unification plan
  pub fn from_plan(plan: &UnificationPlan) -> String {
    let mut md = String::new();

    md.push_str("# Cargo Rail Unification Report\n\n");
    md.push_str(&format!(
      "Generated: {}\n\n",
      chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    md.push_str("## Summary\n\n");
    md.push_str(&format!("- **Dependencies Unified:** {}\n", plan.workspace_deps.len()));
    md.push_str(&format!("- **Members Updated:** {}\n", plan.member_edits.len()));
    md.push_str(&format!("- **Transitive Pins:** {}\n", plan.transitive_pins.len()));
    md.push_str(&format!(
      "- **Duplicates Cleaned:** {}\n",
      plan.duplicates_cleaned.len()
    ));
    md.push_str(&format!(
      "- **Dead Features Pruned:** {} (empty no-ops)\n",
      plan.pruned_features.len()
    ));
    md.push_str(&format!(
      "- **Optional Features:** {} (user-facing, preserved)\n",
      plan.optional_features.len()
    ));
    md.push_str(&format!(
      "- **Version Mismatches:** {}\n",
      plan.version_mismatches.len()
    ));
    md.push_str(&format!("- **Unused Dependencies:** {}\n", plan.unused_deps.len()));
    md.push_str(&format!("- **Issues:** {}\n\n", plan.issues.len()));

    if !plan.workspace_deps.is_empty() {
      md.push_str("## Unified Dependencies\n\n");
      md.push_str("| Dependency | Version | Features | Used By |\n");
      md.push_str("|------------|---------|----------|----------|\n");

      for dep in &plan.workspace_deps {
        let features = if dep.features.is_empty() {
          "(default)".to_string()
        } else {
          dep.features.join(", ")
        };

        let users = if dep.used_by.len() > 3 {
          format!("{} crates", dep.used_by.len())
        } else {
          dep.used_by.join(", ")
        };

        md.push_str(&format!(
          "| {} | {} | {} | {} |\n",
          dep.name, dep.version_req, features, users
        ));
      }
      md.push('\n');
    }

    if !plan.duplicates_cleaned.is_empty() {
      md.push_str("## Duplicates Unified\n\n");
      md.push_str("| Dependency | Selected Version | Previous Versions |\n");
      md.push_str("|------------|------------------|-------------------|\n");

      for dup in &plan.duplicates_cleaned {
        md.push_str(&format!(
          "| {} | {} | {} |\n",
          dup.dep_name,
          dup.selected_version,
          dup.versions_found.join(", ")
        ));
      }
      md.push('\n');
    }

    if !plan.pruned_features.is_empty() {
      md.push_str("## Dead Features Pruned (Empty No-Ops)\n\n");
      md.push_str("These features do nothing (`feature = []`) and can be safely removed.\n\n");
      md.push_str("| Crate | Feature |\n");
      md.push_str("|-------|----------|\n");

      for pf in &plan.pruned_features {
        md.push_str(&format!("| {} | {} |\n", pf.crate_name, pf.feature_name));
      }
      md.push('\n');
    }

    if !plan.optional_features.is_empty() {
      md.push_str("## Optional Features (User-Facing API)\n\n");
      md.push_str("These features are not enabled by default but provide functionality. They are preserved.\n\n");
      md.push_str("| Crate | Feature | Enables |\n");
      md.push_str("|-------|----------|----------|\n");

      for of in &plan.optional_features {
        let enables = if of.enables.is_empty() {
          "(nothing)".to_string()
        } else {
          of.enables.join(", ")
        };
        md.push_str(&format!("| {} | {} | {} |\n", of.crate_name, of.feature_name, enables));
      }
      md.push('\n');
    }

    if !plan.version_mismatches.is_empty() {
      md.push_str("## Version Mismatches\n\n");
      md.push_str("| Member | Dependency | Declared | Workspace |\n");
      md.push_str("|--------|------------|----------|------------|\n");

      for mismatch in &plan.version_mismatches {
        md.push_str(&format!(
          "| {} | {} | {} | {} |\n",
          mismatch.member, mismatch.dep_name, mismatch.member_version, mismatch.workspace_version
        ));
      }
      md.push('\n');
    }

    if !plan.unused_deps.is_empty() {
      md.push_str("## Unused Dependencies\n\n");
      md.push_str("| Member | Dependency | Kind | Reason |\n");
      md.push_str("|--------|------------|------|--------|\n");

      for ud in &plan.unused_deps {
        let kind_str = match ud.kind {
          DepKind::Normal => "normal",
          DepKind::Dev => "dev",
          DepKind::Build => "build",
        };
        let reason_str = match &ud.reason {
          UnusedReason::NotInResolvedGraph => "not in resolved graph".to_string(),
          UnusedReason::TargetConfiguredButNotResolved { target_cfg } => {
            format!("target `{}` configured but not resolved", target_cfg)
          }
        };
        md.push_str(&format!(
          "| {} | {} | {} | {} |\n",
          ud.member, ud.dep_name, kind_str, reason_str
        ));
      }
      md.push('\n');
    }

    if !plan.issues.is_empty() {
      md.push_str("## Issues\n\n");
      for issue in &plan.issues {
        let icon = if issue.severity == IssueSeverity::Error {
          "[ERROR]"
        } else {
          "[WARN]"
        };
        md.push_str(&format!("{} **{}**: {}\n", icon, issue.dep_name, issue.message));
      }
      md.push('\n');
    }

    md
  }

  /// Writes the report to a file
  pub fn write_to_file(plan: &UnificationPlan, path: &std::path::Path) -> RailResult<()> {
    let content = Self::from_plan(plan);

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    std::fs::write(path, content)?;
    Ok(())
  }
}
