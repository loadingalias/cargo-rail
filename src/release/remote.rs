//! Authoritative Git remote identity for release and changelog operations.

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use serde::{Deserialize, Serialize};
use std::path::Path;

const RELEASE_REMOTE: &str = "origin";

/// One normalized repository endpoint, independent of Git transport syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteRepository {
    host: Option<String>,
    path: String,
}

impl RemoteRepository {
    /// Parse a single Git remote URL or local path without accepting ambiguous suffixes.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let value = value.trim().trim_end_matches('/');
        if value.is_empty() || value.chars().any(char::is_control) {
            return None;
        }

        let windows_path = value.as_bytes().get(1) == Some(&b':')
            && value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && value
                .as_bytes()
                .get(2)
                .is_some_and(|separator| matches!(separator, b'/' | b'\\'));
        let (host, path) = if windows_path || value.starts_with('/') || value.starts_with("\\\\") {
            return Self::local(value);
        } else if let Some((scheme, remainder)) = value.split_once("://") {
            let scheme = scheme.to_ascii_lowercase();
            match scheme.as_str() {
                "http" | "https" | "ssh" | "git" => {}
                "file" => return Self::local(remainder),
                _ => return None,
            }
            let (authority, path) = remainder.split_once('/')?;
            if authority.is_empty() || authority.contains('@') && scheme != "ssh" || ambiguous_hosted_path(path) {
                return None;
            }
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            if host.is_empty() || path.is_empty() {
                return None;
            }
            (Some(host.to_ascii_lowercase()), path)
        } else if let Some((authority, path)) = value.split_once(':') {
            if authority.contains('/') || authority.is_empty() || path.is_empty() {
                return Self::local(value);
            }
            if ambiguous_hosted_path(path) {
                return None;
            }
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            if host.is_empty() {
                return None;
            }
            (Some(host.to_ascii_lowercase()), path)
        } else {
            return Self::local(value);
        };

        Self::normalized(host, path)
    }

    fn local(value: &str) -> Option<Self> {
        Self::normalized(None, value)
    }

    fn normalized(host: Option<String>, path: &str) -> Option<Self> {
        let path = path.trim_end_matches('/');
        let path = if host.is_some() {
            path.strip_suffix(".git").unwrap_or(path)
        } else {
            path
        };
        if path.is_empty()
            || host.is_some()
                && path
                    .split('/')
                    .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return None;
        }
        let path = if matches!(host.as_deref(), Some("github.com" | "gitlab.com")) {
            path.to_ascii_lowercase()
        } else {
            path.to_string()
        };
        Some(Self { host, path })
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn github_owner_repo(&self) -> Option<(&str, &str)> {
        let mut parts = self.path.split('/');
        let owner = parts.next()?;
        let repository = parts.next()?;
        parts.next().is_none().then_some((owner, repository))
    }

    pub(crate) fn selector(&self) -> String {
        match self.host() {
            Some("github.com") | Some("gitlab.com") | None => self.path.clone(),
            Some(host) => format!("{}/{}", host, self.path),
        }
    }

    pub(crate) fn trailer_value(&self) -> RailResult<String> {
        serde_json::to_string(self)
            .map_err(|error| RailError::message(format!("failed to serialize release repository identity: {}", error)))
    }

    pub(crate) fn from_trailer(value: &str) -> RailResult<Self> {
        serde_json::from_str(value)
            .map_err(|error| RailError::message(format!("invalid release repository identity trailer: {}", error)))
    }
}

fn ambiguous_hosted_path(path: &str) -> bool {
    path.contains(['?', '#', '%', '\\']) || path.chars().any(char::is_whitespace)
}

/// Resolve the sole configured fetch repository for read-only changelog links.
pub(crate) fn fetch_repository(workspace_root: &Path) -> RailResult<RemoteRepository> {
    one_remote_repository(workspace_root, &["remote", "get-url", "--all", RELEASE_REMOTE], "fetch")
}

/// Resolve one repository that both fetch and push operations identify.
pub(crate) fn release_repository(workspace_root: &Path) -> RailResult<RemoteRepository> {
    let fetch = fetch_repository(workspace_root)?;
    let push = one_remote_repository(
        workspace_root,
        &["remote", "get-url", "--push", "--all", RELEASE_REMOTE],
        "push",
    )?;
    if fetch != push {
        return Err(RailError::with_help(
            format!(
                "origin fetches from '{}' but pushes to '{}'",
                fetch.selector(),
                push.selector()
            ),
            "make remote.origin.url and the effective origin push URL identify the same repository before releasing",
        ));
    }
    Ok(push)
}

fn one_remote_repository(workspace_root: &Path, args: &[&str], operation: &str) -> RailResult<RemoteRepository> {
    let output = SystemGit::open(workspace_root)?
        .run_git(args)
        .map_err(|error| error.context(format!("could not resolve the effective origin {} URL", operation)))?;
    let stdout = std::str::from_utf8(&output.stdout).map_err(|_| {
        RailError::with_help(
            format!("the effective origin {} URL is not valid UTF-8", operation),
            "replace the origin URL with one cargo-rail can identify exactly",
        )
    })?;
    let values = stdout.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if values.len() != 1 {
        return Err(RailError::with_help(
            format!(
                "origin resolves to {} effective {} URLs; release authority requires exactly one",
                values.len(),
                operation
            ),
            "configure exactly one origin repository before releasing",
        ));
    }
    let repository = RemoteRepository::parse(values[0]).ok_or_else(|| {
        RailError::with_help(
            format!("the effective origin {} URL is malformed or ambiguous", operation),
            "use a complete Git URL or an unambiguous local repository path",
        )
    })?;
    if repository.host().is_some() {
        return Ok(repository);
    }
    let path = Path::new(repository.path());
    if !path.is_absolute() {
        return Err(RailError::with_help(
            format!("the effective origin {} path is relative", operation),
            "use an absolute local repository path so release authority remains stable across recovery",
        ));
    }
    let canonical = crate::utils::canonicalize_existing(path).map_err(|error| {
        RailError::message(format!(
            "failed to resolve the effective origin {} path '{}': {}",
            operation,
            path.display(),
            error
        ))
    })?;
    let canonical = canonical.to_str().ok_or_else(|| {
        RailError::with_help(
            format!("the effective origin {} path is not valid UTF-8", operation),
            "use a local repository path cargo-rail can identify exactly",
        )
    })?;
    RemoteRepository::local(canonical).ok_or_else(|| RailError::message("the local origin repository is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_repository_transports_to_one_identity() {
        let expected = RemoteRepository::parse("https://github.com/org/repo.git").unwrap();
        for remote in [
            "git@github.com:org/repo.git",
            "ssh://git@github.com/org/repo.git",
            "https://github.com/org/repo.git/",
            "https://github.com/org/repo/",
        ] {
            assert_eq!(RemoteRepository::parse(remote), Some(expected.clone()), "{remote}");
        }
        assert_eq!(expected.github_owner_repo(), Some(("org", "repo")));
    }

    #[test]
    fn preserves_nested_gitlab_paths() {
        let repository = RemoteRepository::parse("git@gitlab.com:group/subgroup/repo.git").unwrap();
        assert_eq!(repository.host(), Some("gitlab.com"));
        assert_eq!(repository.path, "group/subgroup/repo");
        assert_eq!(repository.selector(), "group/subgroup/repo");
    }

    #[test]
    fn rejects_ambiguous_repository_suffixes_and_paths() {
        for remote in [
            "https://github.com/org/repo.git?ref=main",
            "https://github.com/org/repo#fragment",
            "https://github.com/org/repo%2fextra",
            "https://github.com/org/repo/extra",
            "https://github.com/org//repo",
            "https://github.com/org/../repo",
            "https://user:secret@github.com/org/repo.git",
            "",
        ] {
            let parsed = RemoteRepository::parse(remote);
            assert!(
                parsed.is_none() || parsed.is_some_and(|repository| repository.github_owner_repo().is_none()),
                "{remote}"
            );
        }
    }

    #[test]
    fn trailer_round_trip_preserves_repository_identity() {
        let repository = RemoteRepository::parse("ssh://git@github.example/org/repo.git").unwrap();
        assert_eq!(
            RemoteRepository::from_trailer(&repository.trailer_value().unwrap()).unwrap(),
            repository
        );
    }
}
