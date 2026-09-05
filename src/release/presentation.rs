//! Captured changelog and forge presentation for a release plan.

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::release::planner::CrateReleasePlan;
use crate::source::ContentDigest;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A captured manual release-note input.
pub struct ReleaseNoteInput {
    /// Workspace-relative source path.
    pub path: PathBuf,
    /// SHA-256 digest of the source bytes.
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// Captured user-facing release presentation.
pub struct PlannedPresentation {
    /// Release date used in the heading.
    pub date: String,
    /// Optional exact changelog mutation.
    pub changelog: Option<PlannedChangelog>,
    /// Forge release body.
    pub release_notes: String,
    /// Manual inputs used for the body.
    pub note_inputs: Vec<ReleaseNoteInput>,
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

pub(crate) fn version_header(version: &str, date: &str) -> String {
    if date.is_empty() {
        format!("## [{version}]\n")
    } else {
        format!("## [{version}] - {date}\n")
    }
}

#[cfg(test)]
pub(crate) fn insert_release(existing: &str, release: &str) -> String {
    let insertion = existing.find("\n## ").map(|i| i + 1).unwrap_or(existing.len());
    let mut out = String::with_capacity(existing.len() + release.len() + 1);
    out.push_str(existing.get(..insertion).unwrap_or(existing));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(release.trim_end());
    out.push('\n');
    out.push_str(existing.get(insertion..).unwrap_or_default());
    out
}

pub(crate) fn insert_release_at(existing: &str, version: &str, date: &str, body: &str) -> String {
    let section = format!("{}\n{}\n", version_header(version, date).trim_end(), body.trim());
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
    config: &ReleaseConfig,
    plan: &CrateReleasePlan,
    date: &str,
    _github: Option<&(String, String)>,
) -> RailResult<PlannedPresentation> {
    validate_entry(&plan.changelog_body)?;
    let existing = read_optional(root, &plan.changelog_path)?;
    let changelog = if plan.generate_changelog {
        let before_digest = existing
            .as_ref()
            .map(|s| ContentDigest::sha256(s.as_bytes()).to_string());
        let content = insert_release_at(
            existing.as_deref().unwrap_or("# Changelog\n\n"),
            &plan.new_version.to_string(),
            date,
            &plan.changelog_body,
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
    let mut release_notes = plan.changelog_body.clone();
    let mut release_note_inputs = Vec::new();
    let notes_dir = root.join(&config.release_notes_dir);
    for candidate in [
        notes_dir.join(format!("{}.md", plan.new_version)),
        notes_dir.join(format!("{}.md", plan.tag_name)),
    ] {
        if let Ok(bytes) = fs::read(&candidate) {
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| RailError::message(format!("release notes {} are not UTF-8", candidate.display())))?;
            release_notes = text;
            release_note_inputs.push(ReleaseNoteInput {
                path: candidate,
                digest: ContentDigest::sha256(&bytes).to_string(),
            });
            break;
        }
    }
    Ok(PlannedPresentation {
        date: date.to_string(),
        changelog,
        release_notes,
        note_inputs: release_note_inputs,
    })
}

pub(crate) fn validate_inputs(root: &Path, plan: &CrateReleasePlan) -> RailResult<()> {
    let Some(presentation) = &plan.presentation else {
        return Err(RailError::message("release has no captured presentation"));
    };
    if let Some(changelog) = &presentation.changelog {
        let current = read_optional(root, &changelog.path)?;
        let digest = current
            .as_ref()
            .map(|s| ContentDigest::sha256(s.as_bytes()).to_string());
        if digest != changelog.before_digest && digest != Some(changelog.after_digest.clone()) {
            return Err(RailError::message(format!(
                "changelog changed since release plan capture: {}",
                changelog.path.display()
            )));
        }
    }
    for input in &presentation.note_inputs {
        let bytes = fs::read(&input.path)
            .map_err(|e| RailError::message(format!("failed to read {}: {e}", input.path.display())))?;
        if ContentDigest::sha256(&bytes).to_string() != input.digest {
            return Err(RailError::message(format!(
                "release notes changed since release plan capture: {}",
                input.path.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn apply_changelog(root: &Path, path: &Path, write: &PlannedChangelog) -> RailResult<()> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if full
        != if write.path.is_absolute() {
            write.path.clone()
        } else {
            root.join(&write.path)
        }
    {
        return Err(RailError::message("changelog presentation path mismatch"));
    }
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| RailError::message(format!("failed to create {}: {e}", parent.display())))?;
    }
    crate::utils::write_file_atomic(&full, write.content.as_bytes())
}
