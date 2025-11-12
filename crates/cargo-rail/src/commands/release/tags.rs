//! Git tag operations for release management
//!
//! Handles finding release tags, parsing version information,
//! and determining what has changed since the last release.

use crate::core::error::RailResult;
use crate::core::vcs::git::GitBackend;
use semver::Version;
use std::collections::HashMap;
use std::path::Path;

/// Release tag information for a crate
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTag {
  /// Crate name
  pub crate_name: String,
  /// Version from tag
  pub version: Version,
  /// Full tag name (e.g., "my-crate@v1.2.3")
  pub tag_name: String,
  /// Commit SHA the tag points to
  pub commit_sha: String,
}

impl ReleaseTag {
  /// Parse a tag name to extract crate name and version
  ///
  /// Supports multiple formats:
  /// - `crate-name@v1.2.3` (preferred)
  /// - `crate-name-v1.2.3`
  /// - `v1.2.3` (for single-crate repos)
  pub fn parse(tag_name: &str) -> Option<Self> {
    // Try crate-name@vX.Y.Z format (preferred)
    if let Some((crate_name, version_str)) = tag_name.split_once('@') {
      let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
      if let Ok(version) = version_str.parse::<Version>() {
        return Some(Self {
          crate_name: crate_name.to_string(),
          version,
          tag_name: tag_name.to_string(),
          commit_sha: String::new(), // Will be filled by caller
        });
      }
    }

    // Try crate-name-vX.Y.Z format
    if let Some(v_pos) = tag_name.rfind("-v") {
      let crate_name = &tag_name[..v_pos];
      let version_str = &tag_name[v_pos + 2..]; // Skip "-v"
      if let Ok(version) = version_str.parse::<Version>() {
        return Some(Self {
          crate_name: crate_name.to_string(),
          version,
          tag_name: tag_name.to_string(),
          commit_sha: String::new(),
        });
      }
    }

    // Try vX.Y.Z format (single-crate, no name)
    if let Some(version_str) = tag_name.strip_prefix('v')
      && let Ok(version) = version_str.parse::<Version>()
    {
      return Some(Self {
        crate_name: String::new(), // Unknown crate name
        version,
        tag_name: tag_name.to_string(),
        commit_sha: String::new(),
      });
    }

    None
  }

  /// Format tag name in preferred format
  pub fn format(crate_name: &str, version: &Version) -> String {
    format!("{}@v{}", crate_name, version)
  }
}

/// Find the last release tag for each crate in the workspace
pub fn find_last_release_tags(repo_path: &Path, crate_names: &[String]) -> RailResult<HashMap<String, ReleaseTag>> {
  let git = GitBackend::open(repo_path)?;
  let all_tags = git.list_tags()?;

  let mut crate_tags: HashMap<String, Vec<ReleaseTag>> = HashMap::new();

  // Parse all tags and group by crate
  for tag_name in all_tags {
    if let Some(mut release_tag) = ReleaseTag::parse(&tag_name) {
      // Get commit SHA for this tag
      if let Ok(commit) = git.resolve_reference(&format!("refs/tags/{}", tag_name)) {
        release_tag.commit_sha = commit;

        // If tag has no crate name, try to match it to workspace crates
        if release_tag.crate_name.is_empty() {
          // For single-crate workspaces, assign to the only crate
          if crate_names.len() == 1 {
            release_tag.crate_name = crate_names[0].clone();
          } else {
            // Skip ambiguous tags in multi-crate workspaces
            continue;
          }
        }

        // Only track tags for workspace crates
        if crate_names.contains(&release_tag.crate_name) {
          crate_tags
            .entry(release_tag.crate_name.clone())
            .or_default()
            .push(release_tag);
        }
      }
    }
  }

  // Find the latest tag for each crate
  let mut latest_tags = HashMap::new();
  for (crate_name, mut tags) in crate_tags {
    tags.sort_by(|a, b| b.version.cmp(&a.version)); // Descending order
    if let Some(latest) = tags.into_iter().next() {
      latest_tags.insert(crate_name, latest);
    }
  }

  Ok(latest_tags)
}

/// Detect which crates have changed since their last release
pub fn detect_changed_crates(
  repo_path: &Path,
  crate_names: &[String],
  crate_paths: &HashMap<String, String>,
) -> RailResult<HashMap<String, bool>> {
  let git = GitBackend::open(repo_path)?;
  let last_tags = find_last_release_tags(repo_path, crate_names)?;

  let mut changed = HashMap::new();

  for crate_name in crate_names {
    let has_changes = if let Some(tag) = last_tags.get(crate_name) {
      // Get commits since last tag
      let commits_since_tag = git.get_commits_since(&tag.commit_sha)?;

      // Check if any commit touched this crate's files
      let crate_path = crate_paths
        .get(crate_name)
        .ok_or_else(|| anyhow::anyhow!("No path found for crate '{}'", crate_name))?;

      commits_since_tag.iter().any(|commit_sha| {
        git
          .get_changed_files(commit_sha)
          .ok()
          .map(|files| files.iter().any(|(path, _)| path.starts_with(crate_path.as_str())))
          .unwrap_or(false)
      })
    } else {
      // No previous tag - this is a new crate, mark as changed
      true
    };

    changed.insert(crate_name.clone(), has_changes);
  }

  Ok(changed)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_tag_preferred_format() {
    let tag = ReleaseTag::parse("my-crate@v1.2.3").unwrap();
    assert_eq!(tag.crate_name, "my-crate");
    assert_eq!(tag.version, Version::new(1, 2, 3));
    assert_eq!(tag.tag_name, "my-crate@v1.2.3");
  }

  #[test]
  fn test_parse_tag_dash_format() {
    let tag = ReleaseTag::parse("my-crate-v1.2.3").unwrap();
    assert_eq!(tag.crate_name, "my-crate");
    assert_eq!(tag.version, Version::new(1, 2, 3));
  }

  #[test]
  fn test_parse_tag_version_only() {
    let tag = ReleaseTag::parse("v1.2.3").unwrap();
    assert_eq!(tag.crate_name, "");
    assert_eq!(tag.version, Version::new(1, 2, 3));
  }

  #[test]
  fn test_parse_tag_with_prerelease() {
    let tag = ReleaseTag::parse("my-crate@v1.2.3-alpha.1").unwrap();
    assert_eq!(tag.crate_name, "my-crate");
    assert_eq!(tag.version.major, 1);
    assert_eq!(tag.version.minor, 2);
    assert_eq!(tag.version.patch, 3);
  }

  #[test]
  fn test_parse_tag_invalid() {
    assert!(ReleaseTag::parse("not-a-version").is_none());
    assert!(ReleaseTag::parse("my-crate-1.2.3").is_none()); // Missing 'v'
    assert!(ReleaseTag::parse("").is_none());
  }

  #[test]
  fn test_format_tag() {
    let formatted = ReleaseTag::format("my-crate", &Version::new(1, 2, 3));
    assert_eq!(formatted, "my-crate@v1.2.3");
  }

  #[test]
  fn test_parse_crate_with_dashes() {
    let tag = ReleaseTag::parse("my-complex-crate@v2.0.0").unwrap();
    assert_eq!(tag.crate_name, "my-complex-crate");
    assert_eq!(tag.version, Version::new(2, 0, 0));
  }

  #[test]
  fn test_parse_disambiguates_dash_format() {
    // Should parse the LAST occurrence of "-v" to handle crates with dashes
    let tag = ReleaseTag::parse("my-crate-name-v1.0.0").unwrap();
    assert_eq!(tag.crate_name, "my-crate-name");
    assert_eq!(tag.version, Version::new(1, 0, 0));
  }
}
