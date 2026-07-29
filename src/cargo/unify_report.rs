//! Markdown report generation for unification plans
//!
//! Generates human-readable reports from `UnificationPlan` results.

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::unify_types::{IssueSeverity, MemberEdit, UnificationPlan, UnusedReason};
use crate::error::RailResult;

/// Generate a markdown report from the unification plan
pub struct UnifyReport;

impl UnifyReport {
  /// Generates a markdown report from the unification plan
  pub fn from_plan(plan: &UnificationPlan) -> String {
    let mut md = String::new();

    md.push_str("# Cargo-Rail Unification Report\n\n");
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

    if plan.member_edits.values().flatten().any(|edit| {
      matches!(
        edit,
        MemberEdit::UseWorkspace { local_features, .. } if !local_features.is_empty()
      )
    }) {
      md.push_str("## Member-Local Features\n\n");
      md.push_str(
        "These features remain on their exact member declaration instead of leaking into the workspace baseline.\n\n",
      );
      md.push_str("| Member | Dependency | Kind | Target | Features |\n");
      md.push_str("|--------|------------|------|--------|----------|\n");

      let mut members: Vec<_> = plan.member_edits.iter().collect();
      members.sort_unstable_by_key(|(member, _)| *member);

      for (member, edits) in members {
        let mut local_edits: Vec<_> = edits
          .iter()
          .filter_map(|edit| match edit {
            MemberEdit::UseWorkspace {
              dep_name,
              dep_kind,
              target,
              local_features,
              ..
            } if !local_features.is_empty() => Some((dep_name, dep_kind, target, local_features)),
            _ => None,
          })
          .collect();
        local_edits.sort_unstable_by(|left, right| {
          left
            .0
            .cmp(right.0)
            .then_with(|| dep_kind_label(left.1).cmp(dep_kind_label(right.1)))
            .then_with(|| left.2.cmp(right.2))
        });

        for (dep_name, dep_kind, target, local_features) in local_edits {
          md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            member,
            dep_name,
            dep_kind_label(dep_kind),
            target.as_deref().unwrap_or("(all)"),
            local_features.join(", ")
          ));
        }
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
        let kind_str = dep_kind_label(&ud.kind);
        let reason_str = match &ud.reason {
          UnusedReason::NotUsedInSource => "unused in source".to_string(),
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

const fn dep_kind_label(kind: &DepKind) -> &'static str {
  match kind {
    DepKind::Normal => "normal",
    DepKind::Dev => "dev",
    DepKind::Build => "build",
  }
}
