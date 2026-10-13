//! Captured changelog and forge presentation for a release plan.

use crate::error::{RailError, RailResult};
use crate::release::planner::CrateReleasePlan;
use crate::source::ContentDigest;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Exact changelog write captured during planning.
pub struct PlannedChangelog {
    /// Changelog path.
    pub path: PathBuf,
    /// Digest before the write.
    pub before_digest: Option<String>,
    /// Digest after the write.
    pub after_digest: String,
    /// Exact post-release content.
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Captured user-facing release presentation.
pub struct PlannedPresentation {
    /// Release date used in the heading.
    pub date: String,
    /// Optional exact changelog mutation.
    pub changelog: Option<PlannedChangelog>,
    /// GitHub release body: the changelog section for this version.
    pub release_notes: String,
}

pub(crate) fn read_optional(root: &Path, path: &Path) -> RailResult<Option<String>> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    match fs::read_to_string(&full) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RailError::message(format!(
            "failed to read {}: {error}",
            full.display()
        ))),
    }
}

pub(crate) fn extract_section(content: &str, version: &str) -> Option<String> {
    let marker = format!("## [{version}]");
    let start = content.find(&marker)?;
    let rest = content.get(start..)?;
    let end = rest
        .get(marker.len()..)?
        .find("\n## ")
        .map(|i| marker.len() + i)
        .unwrap_or(rest.len());
    Some(rest.get(..end)?.trim_end().to_string())
}

pub(crate) fn version_header(version: &str, date: &str, compare_url: Option<&str>) -> String {
    match (compare_url, date.is_empty()) {
        (Some(url), true) => format!("## [{version}]({url})\n"),
        (Some(url), false) => format!("## [{version}]({url}) - {date}\n"),
        (None, true) => format!("## [{version}]\n"),
        (None, false) => format!("## [{version}] - {date}\n"),
    }
}

pub(crate) fn insert_release_at(
    existing: &str,
    version: &str,
    date: &str,
    body: &str,
    compare_url: Option<&str>,
) -> String {
    let section = format!(
        "{}\n{}\n",
        version_header(version, date, compare_url).trim_end(),
        body.trim()
    );
    if existing.trim().is_empty() {
        return section;
    }
    let insertion = existing.find("\n## ").map(|i| i + 1).unwrap_or(existing.len());
    let mut out = String::with_capacity(existing.len() + section.len() + 1);
    out.push_str(existing.get(..insertion).unwrap_or(existing));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&section);
    out.push_str(existing.get(insertion..).unwrap_or_default());
    out
}

pub(crate) fn validate_entry(body: &str) -> RailResult<()> {
    if body.contains('\0') {
        return Err(RailError::message("release presentation contains NUL"));
    }
    Ok(())
}

pub(crate) fn capture(
    root: &Path,
    plan: &CrateReleasePlan,
    date: &str,
    github: Option<&(String, String)>,
) -> RailResult<PlannedPresentation> {
    validate_entry(&plan.changelog_body)?;
    let existing = read_optional(root, &plan.changelog_path)?;
    let changelog = if plan.generate_changelog {
        let before_digest = existing
            .as_ref()
            .map(|s| ContentDigest::sha256(s.as_bytes()).to_string());
        let compare_url = existing
            .as_deref()
            .filter(|content| {
                content
                    .lines()
                    .any(|line| line.starts_with("## [") && line.contains("](") && line.contains("/compare/"))
            })
            .and_then(|_| github.zip(plan.previous_tag.as_deref()))
            .map(|((owner, repo), previous_tag)| {
                format!(
                    "https://github.com/{owner}/{repo}/compare/{previous_tag}...{}",
                    plan.tag_name
                )
            });
        let content = insert_release_at(
            existing.as_deref().unwrap_or("# Changelog\n\n"),
            &plan.new_version.to_string(),
            date,
            &plan.changelog_body,
            compare_url.as_deref(),
        );
        let after_digest = ContentDigest::sha256(content.as_bytes()).to_string();
        Some(PlannedChangelog {
            path: plan.changelog_path.clone(),
            before_digest,
            after_digest,
            content,
        })
    } else {
        None
    };
    Ok(PlannedPresentation {
        date: date.to_string(),
        changelog,
        release_notes: plan.changelog_body.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::insert_release_at;

    #[test]
    fn inserts_release_before_history_preserving_the_preamble() {
        let existing = "# Changelog\n\nProject notes.\n\n## [1.0.0] - 2026-01-01\n\nOld entry.\n";
        assert_eq!(
            insert_release_at(existing, "1.1.0", "2026-02-03", "  New entry.\n\n", None),
            "# Changelog\n\nProject notes.\n\n## [1.1.0] - 2026-02-03\nNew entry.\n## [1.0.0] - 2026-01-01\n\nOld entry.\n"
        );
    }

    #[test]
    fn inserts_first_release_with_optional_date_and_header() {
        for existing in ["", " \n\t"] {
            assert_eq!(
                insert_release_at(existing, "1.0.0", "", "First entry.", None),
                "## [1.0.0]\nFirst entry.\n"
            );
        }
        assert_eq!(
            insert_release_at("# Changelog", "1.0.0", "", "First entry.", None),
            "# Changelog\n## [1.0.0]\nFirst entry.\n"
        );
    }
}
