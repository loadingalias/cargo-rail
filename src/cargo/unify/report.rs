//! Unification report generation

use super::types::UnificationPlan;
use crate::cargo::WorkspaceMetadata;
use crate::error::{RailError, RailResult};
use std::path::Path;

/// Generates a detailed markdown report of the unification process
pub struct UnifyReport;

impl UnifyReport {
  /// Generate markdown report content
  pub fn generate(plan: &UnificationPlan, _metadata: &WorkspaceMetadata) -> String {
    let mut md = String::new();

    // Title and Header
    md.push_str("# Cargo Rail Unification Report\n\n");

    // Summary Statistics
    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Value |\n");
    md.push_str("|---|---|\n");
    md.push_str(&format!(
      "| Total Dependencies Analyzed | {} |\n",
      plan.stats.total_deps
    ));
    md.push_str(&format!("| Dependencies Unified | {} |\n", plan.stats.unified_count));
    md.push_str(&format!("| Issues Detected | {} |\n", plan.stats.issue_count));
    md.push_str(&format!(
      "| Redundant Compilations Saved | {} |\n",
      plan.stats.compilations_saved
    ));
    md.push('\n');

    // Issues Section
    if !plan.issues.is_empty() {
      md.push_str("## ⚠️ Issues Detected\n\n");
      for issue in &plan.issues {
        md.push_str(&format!("### {}\n\n", issue.dep_name));
        md.push_str(&format!("**Type:** {:?}\n\n", issue.issue_type));
        md.push_str(&format!("**Severity:** {:?}\n\n", issue.severity));
        md.push_str(&format!("**Description:** {}\n\n", issue.format_message()));
        md.push_str(&format!(
          "**Affected Members:** {}\n\n",
          issue.affected_members.join(", ")
        ));
        md.push_str("---\n\n");
      }
    }

    // Unified Dependencies Section
    if !plan.workspace_deps.is_empty() {
      md.push_str("## ✅ Unified Dependencies\n\n");
      md.push_str("The following dependencies have been unified in `[workspace.dependencies]`:\n\n");

      md.push_str("| Dependency | Version | Features | Used By |\n");
      md.push_str("|---|---|---|---|\n");

      for dep in &plan.workspace_deps {
        let features = if dep.features.is_empty() {
          "(none)".to_string()
        } else {
          dep.features.join(", ")
        };

        let used_by_count = dep.used_by.len();
        let used_by_str = if used_by_count > 3 {
          format!("{} members", used_by_count)
        } else {
          dep.used_by.join(", ")
        };

        md.push_str(&format!(
          "| **{}** | `{}` | `{}` | {} |\n",
          dep.name, dep.version_req, features, used_by_str
        ));
      }
      md.push('\n');
    }

    // Member Edits Section
    if !plan.member_edits.is_empty() {
      md.push_str("## 📝 Member Updates\n\n");
      md.push_str("The following workspace members were updated to use workspace inheritance:\n\n");

      let mut members: Vec<_> = plan.member_edits.keys().collect();
      members.sort();

      for member in members {
        let edits = &plan.member_edits[member];
        md.push_str(&format!("### {}\n\n", member));
        md.push_str(&format!("Updated {} dependencies:\n", edits.len()));
        for edit in edits {
          match edit {
            super::types::MemberEdit::UseWorkspace { dep_name, .. } => {
              md.push_str(&format!("- `{}` -> `workspace = true`\n", dep_name));
            }
          }
        }
        md.push('\n');
      }
    }

    md
  }

  /// Write report to file
  pub fn write_to_file(plan: &UnificationPlan, metadata: &WorkspaceMetadata, path: &Path) -> RailResult<()> {
    let content = Self::generate(plan, metadata);
    std::fs::write(path, content)
      .map_err(|e| RailError::message(format!("Failed to write report to {}: {}", path.display(), e)))?;
    Ok(())
  }
}
