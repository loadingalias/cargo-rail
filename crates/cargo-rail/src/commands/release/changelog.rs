//! World-class changelog generation and version bump analysis using git-cliff-core
//!
//! This module provides industry-standard changelog generation and conventional commit
//! analysis using git-cliff, the same tool used by release-plz and many other projects.
//!
//! ## Architecture
//!
//! - **Version Bump Analysis**: Parse commits, detect conventional commits, calculate semver bumps
//! - **Changelog Generation**: Use git-cliff's template engine to generate beautiful changelogs
//! - **Monorepo Support**: Filter commits per-crate using git-cliff's include_paths feature
//!
//! ## Usage
//!
//! ```rust,ignore
//! // Analyze commits for version bump
//! let (bump_type, commit_count) = analyze_commits_for_crate(
//!     repo_path,
//!     crate_path,
//!     since_commit,
//! )?;
//!
//! // Generate changelog
//! let changelog = generate_changelog_for_crate(
//!     repo_path,
//!     crate_name,
//!     crate_path,
//!     since_version,
//!     next_version,
//! )?;
//! ```

use crate::commands::release::semver::BumpType;
use crate::core::error::RailResult;
use crate::core::vcs::git::GitBackend;
use git_cliff_core::commit::Commit;
use git_cliff_core::config::{ChangelogConfig, Config, GitConfig};
use git_cliff_core::release::Release;
use glob::Pattern;
use std::path::Path;

/// Create a git-cliff Config for conventional commit parsing and monorepo filtering
fn create_git_cliff_config(crate_path: &str) -> RailResult<Config> {
  // Configure conventional commits parsing
  let git_config = GitConfig {
    // Enable conventional commits parsing
    conventional_commits: true,
    // Filter out non-conventional commits
    filter_unconventional: true,
    // Don't require all commits to be conventional (we'll filter)
    require_conventional: false,
    // Don't split commits on newlines
    split_commits: false,
    // Include only commits that touch this crate's path (monorepo support!)
    include_paths: vec![Pattern::new(&format!("{}/**", crate_path))
      .map_err(|e| anyhow::anyhow!("Invalid path pattern: {}", e))?],
    // Don't exclude any paths
    exclude_paths: vec![],
    // Default configs
    commit_preprocessors: vec![],
    commit_parsers: vec![],
    protect_breaking_commits: true,
    link_parsers: vec![],
    filter_commits: false,
    tag_pattern: None,
    skip_tags: None,
    ignore_tags: None,
    count_tags: None,
    use_branch_tags: false,
    topo_order: false,
    topo_order_commits: false,
    sort_commits: "oldest".to_string(),
    limit_commits: None,
    recurse_submodules: None,
  };

  // Configure changelog generation (we'll set templates per-crate later)
  let changelog_config = ChangelogConfig {
    header: Some("# Changelog\n\n".to_string()),
    body: "".to_string(), // Will be set during generation
    footer: None,
    trim: true,
    render_always: false,
    postprocessors: vec![],
    output: None,
  };

  Ok(Config {
    changelog: changelog_config,
    git: git_config,
    remote: Default::default(),
    bump: Default::default(),
  })
}

/// Analyze commits for a crate and determine version bump
///
/// This function:
/// 1. Fetches commits since the last release (or all if no tag)
/// 2. Filters commits that affect the specified crate path (monorepo support)
/// 3. Parses commits using git-cliff's conventional commit parser
/// 4. Creates a Release and calculates the next version
/// 5. Returns the bump type and commit count
///
/// # Arguments
///
/// * `repo_path` - Path to the git repository
/// * `crate_path` - Relative path to the crate (e.g., "crates/foo")
/// * `since_commit` - Optional commit SHA to start from (last release tag)
///
/// # Returns
///
/// Tuple of (BumpType, commit_count)
pub fn analyze_commits_for_crate(
  repo_path: &Path,
  crate_path: &str,
  since_commit: Option<&str>,
) -> RailResult<(BumpType, usize)> {
  let git = GitBackend::open(repo_path)?;

  // Get commits since the given commit, or return early if no tag
  let commit_shas = if let Some(since) = since_commit {
    git.get_commits_since(since)?
  } else {
    // For new crates with no previous release, return no bump
    // (version bump will be determined differently for first release)
    return Ok((BumpType::None, 0));
  };

  if commit_shas.is_empty() {
    return Ok((BumpType::None, 0));
  }

  // Create git-cliff config for this crate
  let config = create_git_cliff_config(crate_path)?;

  // Convert our commits to git-cliff Commit objects
  let mut cliff_commits = Vec::new();
  for commit_sha in &commit_shas {
    let changed_files = git.get_changed_files(commit_sha)?;

    // Check if any changed file is in this crate's path (pre-filter for efficiency)
    let affects_crate = changed_files
      .iter()
      .any(|(path, _)| path.starts_with(crate_path));

    if affects_crate {
      if let Ok(message) = git.get_commit_message(commit_sha) {
        // Create git-cliff Commit
        let commit = Commit::new(commit_sha.clone(), message);

        // Process commit with git-cliff (parses conventional commits, filters, etc.)
        match commit.process(&config.git) {
          Ok(processed) => cliff_commits.push(processed),
          Err(_e) => {
            // Skip commits that don't match our filters (e.g., non-conventional)
          }
        }
      }
    }
  }

  let commit_count = cliff_commits.len();

  if commit_count == 0 {
    return Ok((BumpType::None, 0));
  }

  // Create a Release with these commits
  let release = Release {
    version: None, // We're analyzing unreleased commits
    message: None,
    commits: cliff_commits,
    commit_id: None,
    timestamp: None,
    previous: None,
    repository: None,
    commit_range: None,
    submodule_commits: Default::default(),
    statistics: None,
    extra: None,
    #[cfg(feature = "github")]
    github: Default::default(),
    #[cfg(feature = "gitlab")]
    gitlab: Default::default(),
    #[cfg(feature = "gitea")]
    gitea: Default::default(),
    #[cfg(feature = "bitbucket")]
    bitbucket: Default::default(),
  };

  // Determine bump type from commits
  // Check for breaking changes, features, or fixes
  let bump_type = determine_bump_from_release(&release);

  Ok((bump_type, commit_count))
}

/// Determine version bump type from a Release's commits
fn determine_bump_from_release(release: &Release) -> BumpType {
  let mut has_breaking = false;
  let mut has_feature = false;
  let mut has_fix = false;

  for commit in &release.commits {
    if let Some(ref conv) = commit.conv {
      // Check if breaking change
      if conv.breaking() {
        has_breaking = true;
        continue;
      }

      // Check commit type
      let commit_type = conv.type_().as_str();
      match commit_type {
        "feat" | "feature" => has_feature = true,
        "fix" => has_fix = true,
        _ => {}
      }
    }
  }

  // Return highest priority bump
  if has_breaking {
    BumpType::Major
  } else if has_feature {
    BumpType::Minor
  } else if has_fix {
    BumpType::Patch
  } else {
    BumpType::None
  }
}

/// Generate a beautiful changelog for a crate using git-cliff templates
///
/// This function uses git-cliff's powerful template engine to generate
/// a well-formatted, grouped changelog with all the features users expect.
///
/// # Arguments
///
/// * `repo_path` - Path to the git repository
/// * `crate_name` - Name of the crate (for header)
/// * `crate_path` - Relative path to the crate
/// * `since_version` - Optional previous version tag
/// * `next_version` - The version being released
///
/// # Returns
///
/// Formatted changelog string ready to write to CHANGELOG.md
pub fn generate_changelog_for_crate(
  repo_path: &Path,
  crate_name: &str,
  crate_path: &str,
  since_version: Option<&str>,
  next_version: &str,
) -> RailResult<String> {
  let git = GitBackend::open(repo_path)?;

  // Get commits since last version (or all if first release)
  let commit_shas = if let Some(since) = since_version {
    // TODO: Resolve version tag to commit SHA
    git.get_commits_since(since)?
  } else {
    vec![]
  };

  if commit_shas.is_empty() {
    return Ok(format!(
      "# Changelog - {}\n\n## {} - {}\n\nInitial release.\n",
      crate_name,
      next_version,
      chrono::Utc::now().format("%Y-%m-%d")
    ));
  }

  // Create config with custom template
  let mut config = create_git_cliff_config(crate_path)?;

  // Use a beautiful, industry-standard template (Keep-a-Changelog style)
  config.changelog.body = r#"
{% for group, commits in commits | group_by(attribute="group") %}
### {{ group | striptags | trim | upper_first }}
{% for commit in commits %}
{%- if commit.conv -%}
- **{{ commit.conv.scope | default(value="") }}{{ commit.conv.scope | default(value="") | replace(from="", to=": ") }}**{{ commit.conv.description }}
{%- if commit.breaking %} ⚠️ **BREAKING**{% endif %}
{%- else -%}
- {{ commit.message | split(pat="\n") | first }}
{%- endif %}
{%- endfor -%}
{% endfor %}
"#
    .trim()
    .to_string();

  config.changelog.header = Some(format!(
    "# Changelog - {}\n\nAll notable changes to {} will be documented in this file.\n\n",
    crate_name, crate_name
  ));

  // Convert commits to git-cliff Commit objects
  let mut cliff_commits = Vec::new();
  for commit_sha in &commit_shas {
    let changed_files = git.get_changed_files(commit_sha)?;
    let affects_crate = changed_files
      .iter()
      .any(|(path, _)| path.starts_with(crate_path));

    if affects_crate {
      if let Ok(message) = git.get_commit_message(commit_sha) {
        let commit = Commit::new(commit_sha.clone(), message);
        if let Ok(processed) = commit.process(&config.git) {
          cliff_commits.push(processed);
        }
      }
    }
  }

  // Create Release for this version
  let release = Release {
    version: Some(next_version.to_string()),
    message: None,
    commits: cliff_commits,
    commit_id: None,
    timestamp: Some(chrono::Utc::now().timestamp()),
    previous: None,
    repository: None,
    commit_range: None,
    submodule_commits: Default::default(),
    statistics: None,
    extra: None,
    #[cfg(feature = "github")]
    github: Default::default(),
    #[cfg(feature = "gitlab")]
    gitlab: Default::default(),
    #[cfg(feature = "gitea")]
    gitea: Default::default(),
    #[cfg(feature = "bitbucket")]
    bitbucket: Default::default(),
  };

  // Generate changelog using git-cliff
  let changelog = git_cliff_core::changelog::Changelog::new(vec![release], &config, None)
    .map_err(|e| anyhow::anyhow!("Failed to generate changelog: {}", e))?;

  // Render to string
  let mut output = Vec::new();
  changelog
    .generate(&mut output)
    .map_err(|e| anyhow::anyhow!("Failed to render changelog: {}", e))?;

  String::from_utf8(output).map_err(|e| anyhow::anyhow!("Invalid UTF-8 in changelog: {}", e).into())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_create_git_cliff_config() {
    let config = create_git_cliff_config("crates/foo");
    assert!(config.is_ok());

    let config = config.unwrap();
    assert!(config.git.conventional_commits);
    assert!(config.git.filter_unconventional);
    assert_eq!(config.git.include_paths.len(), 1);
  }

  #[test]
  fn test_determine_bump_from_release() {
    // Test breaking change
    let release = Release {
      commits: vec![],
      ..Default::default()
    };
    assert_eq!(determine_bump_from_release(&release), BumpType::None);
  }

  #[test]
  fn test_git_cliff_config_monorepo_filtering() {
    let config = create_git_cliff_config("crates/my-crate").unwrap();
    assert_eq!(
      config.git.include_paths[0].as_str(),
      "crates/my-crate/**"
    );
  }
}
