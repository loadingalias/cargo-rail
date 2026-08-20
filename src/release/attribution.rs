//! Commit → crate attribution via the workspace graph
//!
//! Path-glob attribution (git-cliff, release-plz) silently drops tags,
//! misroutes cross-cutting commits, and cannot express scope intent. This
//! module attributes each commit to its owning crates through the same
//! resolver-backed ownership index the planner uses for test selection:
//!
//! 1. One `git log --name-only` subprocess per release range (not per crate).
//! 2. Each changed file maps to its owning crate via the immutable longest-prefix
//!    index ([`crate::graph::WorkspaceGraph::file_to_crate`]).
//! 3. A commit scope naming a workspace crate narrows attribution to that
//!    crate — scope is an explicit human signal; files are the fallback truth.

use crate::change_detection::classify_path;
use crate::config::ChangelogFilters;
use crate::error::{ConfigError, RailError, RailResult};
use crate::release::changelog::{CommitRef, parse_subject};
use crate::workspace::WorkspaceContext;
use glob::Pattern;
use std::collections::HashSet;
use std::path::Path;

/// One commit with its owning crates resolved
#[derive(Debug, Clone)]
pub struct AttributedCommit {
    /// Full commit SHA
    pub sha: String,
    /// Commit subject line
    pub subject: String,
    /// Commit body (trimmed), if any
    pub body: Option<String>,
    /// Workspace crates this commit touches (sorted). Empty for commits that
    /// only touch workspace infrastructure (root manifests, CI, lockfile).
    pub crates: Vec<String>,
    /// Subset of `crates` touched through files that seed transitive
    /// build/test impact (source, manifests) — the change-file gate signal.
    pub code_crates: Vec<String>,
}

impl AttributedCommit {
    /// Borrow as changelog input
    pub fn as_ref(&self) -> CommitRef<'_> {
        CommitRef {
            sha: &self.sha,
            subject: &self.subject,
            body: self.body.as_deref(),
        }
    }
}

/// Attributed commit history for one release range, newest-first
#[derive(Debug, Clone, Default)]
pub struct AttributedHistory {
    commits: Vec<AttributedCommit>,
}

impl AttributedHistory {
    /// All commits in the range, newest-first
    pub fn commits(&self) -> &[AttributedCommit] {
        &self.commits
    }

    /// Commits attributed to `crate_name`, newest-first
    pub fn for_crate<'a>(&'a self, crate_name: &'a str) -> impl Iterator<Item = &'a AttributedCommit> + 'a {
        self.commits
            .iter()
            .filter(move |commit| commit.crates.iter().any(|c| c == crate_name))
    }

    /// Changelog inputs for `crate_name`, newest-first
    pub fn commit_refs_for_crate<'a>(&'a self, crate_name: &'a str) -> Vec<CommitRef<'a>> {
        self.for_crate(crate_name).map(AttributedCommit::as_ref).collect()
    }

    /// Whether `crate_name` has commits that seed build/test impact
    pub fn has_code_changes(&self, crate_name: &str) -> bool {
        self.commits
            .iter()
            .any(|commit| commit.code_crates.iter().any(|c| c == crate_name))
    }
}

/// Resolver-backed commit attributor
pub struct CommitAttributor<'a> {
    ctx: &'a WorkspaceContext,
    members: HashSet<&'a str>,
}

impl<'a> CommitAttributor<'a> {
    /// Create an attributor over the workspace graph
    pub fn new(ctx: &'a WorkspaceContext) -> Self {
        let members = ctx.graph().workspace_members().iter().map(String::as_str).collect();
        Self { ctx, members }
    }

    /// Attribute all commits in `from..to` (full history when `from` is `None`)
    ///
    /// One git subprocess for the whole range; attribution performs at most one
    /// hash lookup per path component against the immutable ownership index.
    pub fn history(&self, from: Option<&str>, to: &str) -> RailResult<AttributedHistory> {
        self.history_with_filters(from, to, None)
    }

    /// Attribute commits while honoring changelog path filters.
    ///
    /// `include_paths` and `exclude_paths` are an explicit escape hatch for
    /// repositories that need to override resolver-backed ownership for a
    /// changelog. The path still has to resolve to a workspace crate.
    pub fn history_with_filters(
        &self,
        from: Option<&str>,
        to: &str,
        filters: Option<&ChangelogFilters>,
    ) -> RailResult<AttributedHistory> {
        let entries = self.ctx.git()?.git().log_with_files(from, to)?;
        let path_filters = PathFilters::compile(filters)?;

        let mut commits = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut crates: Vec<String> = Vec::new();
            let mut code_crates: Vec<String> = Vec::new();
            let mut any_file_filtered = false;

            for file in &entry.files {
                let Some(workspace_path) = self.ctx.to_workspace_path(file) else {
                    continue;
                };
                if !path_filters.allows(&workspace_path) {
                    any_file_filtered = true;
                    continue;
                }
                let Some(owner) = self.ctx.graph().file_to_crate(&workspace_path) else {
                    continue;
                };
                if classify_path(&workspace_path).seeds_build_test_transitive() && !code_crates.contains(&owner) {
                    code_crates.push(owner.clone());
                }
                if !crates.contains(&owner) {
                    crates.push(owner);
                }
            }

            // Scope narrowing: an explicit crate-name scope wins over file paths.
            // A scope may claim an otherwise unattributed commit (e.g. one that
            // only touches workspace infrastructure), but never one whose files
            // were excluded by path filters — filters stay authoritative.
            let parsed = parse_subject(&entry.subject, entry.body.as_deref());
            if let Some(scope) = parsed.scope
                && self.members.contains(scope)
            {
                crates.retain(|c| c == scope);
                code_crates.retain(|c| c == scope);
                if crates.is_empty() && !any_file_filtered {
                    crates.push(scope.to_string());
                }
            }

            crates.sort_unstable();
            code_crates.sort_unstable();

            commits.push(AttributedCommit {
                sha: entry.sha,
                subject: entry.subject,
                body: entry.body,
                crates,
                code_crates,
            });
        }

        Ok(AttributedHistory { commits })
    }
}

#[derive(Debug, Default)]
struct PathFilters {
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
}

impl PathFilters {
    fn compile(filters: Option<&ChangelogFilters>) -> RailResult<Self> {
        let Some(filters) = filters else {
            return Ok(Self::default());
        };

        Ok(Self {
            include: compile_patterns("release.changelog.filters.include_paths", &filters.include_paths)?,
            exclude: compile_patterns("release.changelog.filters.exclude_paths", &filters.exclude_paths)?,
        })
    }

    fn allows(&self, path: &Path) -> bool {
        let included = self.include.is_empty() || self.include.iter().any(|pattern| pattern.matches_path(path));
        let excluded = self.exclude.iter().any(|pattern| pattern.matches_path(path));
        included && !excluded
    }
}

fn compile_patterns(field: &str, patterns: &[String]) -> RailResult<Vec<Pattern>> {
    patterns
        .iter()
        .map(|pattern| {
            Pattern::new(pattern).map_err(|e| {
                RailError::Config(ConfigError::InvalidGlobPattern {
                    pattern: format!("{} = {}", field, pattern),
                    message: e.to_string(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, crates: &[&str], code_crates: &[&str]) -> AttributedCommit {
        AttributedCommit {
            sha: "aaaaaaa1111111".to_string(),
            subject: subject.to_string(),
            body: None,
            crates: crates.iter().map(|s| s.to_string()).collect(),
            code_crates: code_crates.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn for_crate_filters_by_attribution() {
        let history = AttributedHistory {
            commits: vec![
                commit("feat: core change", &["core"], &["core"]),
                commit("fix: cli change", &["cli"], &["cli"]),
                commit("refactor: both", &["cli", "core"], &["cli", "core"]),
            ],
        };

        let core: Vec<_> = history.for_crate("core").map(|c| c.subject.as_str()).collect();
        assert_eq!(core, vec!["feat: core change", "refactor: both"]);
        assert!(history.has_code_changes("cli"));
        assert!(!history.has_code_changes("ghost"));
    }

    #[test]
    fn infra_only_commits_attribute_to_no_crate() {
        let history = AttributedHistory {
            commits: vec![commit("ci: bump action", &[], &[])],
        };
        assert_eq!(history.for_crate("core").count(), 0);
        assert!(!history.commits()[0].crates.iter().any(|_| true));
    }
}
