//! The GitHub REST calls a release makes: commit checks, releases, and release pull requests.
//!
//! The API root comes from `GITHUB_API_URL` (set by GitHub Actions and GitHub Enterprise Server),
//! and the token from `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`, so `gh` is optional.

use std::io::Read as _;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{RailError, RailResult};

use super::remote::RemoteRepository;

const DEFAULT_API: &str = "https://api.github.com/";
const API_VERSION: &str = "2022-11-28";
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// GitHub rejects release bodies above 125,000 characters.
pub(crate) const MAX_RELEASE_BODY: usize = 125_000;
/// Release pull requests come from branches under this prefix.
pub(crate) const RELEASE_BRANCH_PREFIX: &str = "rail/release/";
/// Bounds the draft search when a tag has no published release.
const MAX_RELEASE_PAGES: u32 = 10;

/// The owner and name of the GitHub repository that `origin` identifies, if any.
pub(crate) fn github_repository(origin: &RemoteRepository) -> Option<(String, String)> {
    let host = origin.host()?;
    let server = std::env::var("GITHUB_SERVER_URL")
        .ok()
        .and_then(|url| Url::parse(&url).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    if host != "github.com" && server.as_deref() != Some(host) {
        return None;
    }
    origin
        .github_owner_repo()
        .map(|(owner, repo)| (owner.to_owned(), repo.to_owned()))
}

/// An authenticated client for one repository.
#[derive(Debug)]
pub(crate) struct GitHub {
    api: Url,
    host: String,
    owner: String,
    repo: String,
    token: String,
    client: Client,
}

/// Largest release asset a release downloads to verify.
const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The aggregate state of the checks GitHub reports on one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checks {
    /// Every reported check succeeded.
    Passed(usize),
    /// Some checks have not finished; none failed.
    Pending(Vec<String>),
    /// These checks failed.
    Failed(Vec<String>),
    /// GitHub reports no checks for the commit.
    Missing,
}

/// One GitHub release.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Release {
    pub(crate) id: u64,
    pub(crate) tag_name: String,
    pub(crate) draft: bool,
    pub(crate) html_url: String,
    #[serde(default)]
    pub(crate) assets: Vec<ReleaseAsset>,
}

/// One file attached to a release.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReleaseAsset {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) size: u64,
    /// `sha256:<hex>`, recorded by GitHub when the file was uploaded.
    #[serde(default)]
    pub(crate) digest: Option<String>,
}

impl Release {
    /// The configured asset names that are not uploaded yet, in configuration order.
    pub(crate) fn missing_assets<'a>(&self, expected: &'a [String]) -> Vec<&'a str> {
        expected
            .iter()
            .filter(|name| {
                !self
                    .assets
                    .iter()
                    .any(|asset| &asset.name == *name && asset.state == "uploaded")
            })
            .map(String::as_str)
            .collect()
    }
}

/// One pull request.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullRequest {
    pub(crate) number: u64,
    pub(crate) html_url: String,
    head: PullRequestHead,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestHead {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<PullRequestRepository>,
}

/// A merged release pull request: the release commit it proposed and the commit the merge made.
#[derive(Debug, Clone)]
pub(crate) struct MergedPullRequest {
    pub(crate) number: u64,
    pub(crate) head: String,
    pub(crate) merge_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestRepository {
    full_name: String,
}

/// A release to create.
pub(crate) struct NewRelease<'a> {
    pub(crate) tag: &'a str,
    pub(crate) commit: &'a str,
    pub(crate) name: &'a str,
    pub(crate) body: &'a str,
    pub(crate) draft: bool,
    pub(crate) prerelease: bool,
}

#[derive(Deserialize)]
struct CheckRuns {
    total_count: usize,
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    check_suite: Option<CheckSuite>,
}

#[derive(Deserialize)]
struct CheckSuite {
    id: u64,
}

#[derive(Deserialize)]
struct CombinedStatus {
    statuses: Vec<CommitStatus>,
}

#[derive(Deserialize)]
struct CommitStatus {
    context: String,
    state: String,
}

#[derive(Deserialize)]
struct WorkflowRun {
    check_suite_id: u64,
}

impl GitHub {
    /// Connect to the GitHub repository `origin` identifies.
    pub(crate) fn connect(owner: &str, repo: &str, host: &str) -> RailResult<Self> {
        let api = std::env::var("GITHUB_API_URL").unwrap_or_else(|_| DEFAULT_API.to_owned());
        let api = Url::parse(&format!("{}/", api.trim_end_matches('/')))
            .map_err(|_| RailError::message("GITHUB_API_URL is not a valid URL"))?;
        let loopback = is_loopback(&api);
        if !(api.scheme() == "https" || api.scheme() == "http" && loopback)
            || !api.username().is_empty()
            || api.password().is_some()
            || api.query().is_some()
        {
            return Err(RailError::message(
                "GITHUB_API_URL must be an HTTPS URL without credentials or a query",
            ));
        }
        let token = token(host)?;
        let mut client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("cargo-rail/", env!("CARGO_PKG_VERSION")));
        if loopback {
            client = client.no_proxy();
        }
        let client = client
            .build()
            .map_err(|_| RailError::message("could not initialize the GitHub client"))?;
        Ok(Self {
            api,
            host: host.to_owned(),
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            token,
            client,
        })
    }

    /// `owner/repo`.
    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    fn request(&self, method: Method, path: &str) -> RailResult<RequestBuilder> {
        let url = self
            .api
            .join(&format!("repos/{}/{}/{path}", self.owner, self.repo))
            .map_err(|_| RailError::message("invalid GitHub API path"))?;
        Ok(self
            .client
            .request(method, url)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION))
    }

    /// Send a request; a 404 is `None`, and any other non-success status is an error.
    fn send<T: DeserializeOwned>(&self, request: RequestBuilder, what: &str) -> RailResult<Option<T>> {
        let response = request
            .send()
            .map_err(|error| RailError::message(format!("GitHub request for {what} failed: {error}")))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|_| RailError::message(format!("GitHub response for {what} was incomplete")))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(RailError::message(format!(
                "GitHub response for {what} exceeds its size limit"
            )));
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value["message"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let help = match status {
                StatusCode::UNAUTHORIZED => "set GH_TOKEN to a token for this repository, or run `gh auth login`",
                StatusCode::FORBIDDEN => "the token needs `contents: write` (and `pull-requests: write` for --pr)",
                _ => "rerun `cargo rail release`; completed steps are not repeated",
            };
            return Err(RailError::with_help(
                format!("GitHub returned HTTP {} for {what}: {detail}", status.as_u16()),
                help,
            ));
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|error| RailError::message(format!("GitHub returned an unexpected {what} response: {error}")))
    }

    fn get<T: DeserializeOwned>(&self, path: &str, what: &str) -> RailResult<Option<T>> {
        self.send(self.request(Method::GET, path)?, what)
    }

    /// The aggregate state of every check run and commit status on `commit`,
    /// excluding the workflow run that invokes this release.
    pub(crate) fn checks(&self, commit: &str) -> RailResult<Checks> {
        let own_suite = self.own_check_suite()?;
        let mut pending = Vec::new();
        let mut failed = Vec::new();
        let mut passed = 0;
        let mut seen = 0;
        for page in 1.. {
            let runs: CheckRuns = self
                .get(
                    &format!("commits/{commit}/check-runs?per_page=100&page={page}"),
                    "commit check runs",
                )?
                .ok_or_else(|| RailError::message(format!("GitHub does not know commit {commit}")))?;
            seen += runs.check_runs.len();
            let finished = runs.check_runs.is_empty() || seen >= runs.total_count;
            for run in runs.check_runs {
                if own_suite.is_some() && run.check_suite.as_ref().map(|suite| suite.id) == own_suite {
                    continue;
                }
                match (run.status.as_str(), run.conclusion.as_deref()) {
                    ("completed", Some("success" | "neutral" | "skipped")) => passed += 1,
                    ("completed", conclusion) => {
                        failed.push(format!("{} ({})", run.name, conclusion.unwrap_or("no conclusion")));
                    }
                    _ => pending.push(run.name),
                }
            }
            if finished {
                break;
            }
        }
        let status: CombinedStatus = self
            .get(&format!("commits/{commit}/status?per_page=100"), "commit statuses")?
            .ok_or_else(|| RailError::message(format!("GitHub does not know commit {commit}")))?;
        for status in status.statuses {
            match status.state.as_str() {
                "success" => passed += 1,
                "pending" => pending.push(status.context),
                state => failed.push(format!("{} ({state})", status.context)),
            }
        }
        failed.sort();
        pending.sort();
        Ok(if !failed.is_empty() {
            Checks::Failed(failed)
        } else if !pending.is_empty() {
            Checks::Pending(pending)
        } else if passed == 0 {
            Checks::Missing
        } else {
            Checks::Passed(passed)
        })
    }

    /// The check suite of the GitHub Actions run that invokes this process, if it runs in this repository.
    fn own_check_suite(&self) -> RailResult<Option<u64>> {
        let (Ok(run), Ok(repository)) = (std::env::var("GITHUB_RUN_ID"), std::env::var("GITHUB_REPOSITORY")) else {
            return Ok(None);
        };
        if !repository.eq_ignore_ascii_case(&self.full_name()) || run.parse::<u64>().is_err() {
            return Ok(None);
        }
        Ok(self
            .get::<WorkflowRun>(&format!("actions/runs/{run}"), "the invoking workflow run")?
            .map(|run| run.check_suite_id))
    }

    /// The release for `tag`, including a draft.
    pub(crate) fn release(&self, tag: &str) -> RailResult<Option<Release>> {
        if let Some(release) = self.get::<Release>(&format!("releases/tags/{}", encode(tag)), "a release")? {
            return Ok(Some(release));
        }
        // Drafts are not addressable by tag, so search the newest releases.
        let mut found = None;
        for page in 1..=MAX_RELEASE_PAGES {
            let releases: Vec<Release> = self
                .get(&format!("releases?per_page=100&page={page}"), "releases")?
                .unwrap_or_default();
            let last = releases.len() < 100;
            for release in releases.into_iter().filter(|release| release.tag_name == tag) {
                if found.replace(release).is_some() {
                    return Err(RailError::with_help(
                        format!("GitHub has more than one release for tag {tag}"),
                        "delete the duplicate draft, then rerun `cargo rail release`",
                    ));
                }
            }
            if last {
                break;
            }
        }
        Ok(found)
    }

    /// Create a release for an existing tag.
    pub(crate) fn create_release(&self, release: &NewRelease<'_>) -> RailResult<Release> {
        let body = serde_json::json!({
            "tag_name": release.tag,
            "target_commitish": release.commit,
            "name": release.name,
            "body": release.body,
            "draft": release.draft,
            "prerelease": release.prerelease,
        });
        self.send(
            with_json(self.request(Method::POST, "releases")?, &body)?,
            "a new release",
        )?
        .ok_or_else(|| RailError::message("GitHub did not create the release"))
    }

    /// Publish a draft release.
    pub(crate) fn publish_release(&self, id: u64) -> RailResult<Release> {
        self.send(
            with_json(
                self.request(Method::PATCH, &format!("releases/{id}"))?,
                &serde_json::json!({ "draft": false }),
            )?,
            "release publication",
        )?
        .ok_or_else(|| RailError::message("GitHub lost the draft release before publication"))
    }

    /// Download one release asset to `destination` and return its SHA-256 in lowercase hex.
    ///
    /// GitHub answers with a redirect to signed storage; the token never leaves GitHub's API host.
    pub(crate) fn download_asset(&self, asset: &ReleaseAsset, destination: &std::path::Path) -> RailResult<String> {
        use std::io::Write as _;
        if asset.size > MAX_ASSET_BYTES {
            return Err(RailError::message(format!(
                "release asset {} exceeds its size limit",
                asset.name
            )));
        }
        let response = self
            .request(Method::GET, &format!("releases/assets/{}", asset.id))?
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .send()
            .map_err(|error| RailError::message(format!("downloading release asset {} failed: {error}", asset.name)))?;
        let mut response = if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| Url::parse(value).ok())
                .filter(|url| url.scheme() == "https" || is_loopback(url))
                .ok_or_else(|| {
                    RailError::message(format!("release asset {} redirected to an invalid URL", asset.name))
                })?;
            self.client
                .get(location)
                .timeout(Duration::from_secs(600))
                .send()
                .map_err(|error| {
                    RailError::message(format!("downloading release asset {} failed: {error}", asset.name))
                })?
        } else {
            response
        };
        if !response.status().is_success() {
            return Err(RailError::message(format!(
                "downloading release asset {} returned HTTP {}",
                asset.name,
                response.status().as_u16()
            )));
        }
        let mut file = std::fs::File::create(destination)?;
        let mut hasher = rscrypto::Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut total = 0_u64;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|_| RailError::message(format!("release asset {} download was incomplete", asset.name)))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > asset.size {
                return Err(RailError::message(format!(
                    "release asset {} is larger than GitHub reports",
                    asset.name
                )));
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
        }
        file.sync_all()?;
        if total != asset.size {
            return Err(RailError::message(format!(
                "release asset {} download was incomplete",
                asset.name
            )));
        }
        Ok(crate::source::ContentDigest::from_sha256_bytes(hasher.finalize()).to_string())
    }

    /// Environment that lets the GitHub CLI act for this client's host and token.
    pub(crate) fn cli_environment(&self) -> Vec<(&'static str, String)> {
        let mut environment = vec![("GH_TOKEN", self.token.clone())];
        if self.host != "github.com" {
            environment.push(("GH_HOST", self.host.clone()));
        }
        environment
    }

    /// Open pull requests whose head is a release branch in this repository.
    pub(crate) fn open_release_pull_requests(&self) -> RailResult<Vec<PullRequest>> {
        let pulls: Vec<PullRequest> = self
            .get("pulls?state=open&per_page=100", "open pull requests")?
            .unwrap_or_default();
        Ok(pulls.into_iter().filter(|pull| self.is_release_pull(pull)).collect())
    }

    /// The most recently merged release pull request, if any.
    pub(crate) fn merged_release_pull_request(&self) -> RailResult<Option<MergedPullRequest>> {
        let pulls: Vec<PullRequest> = self
            .get(
                "pulls?state=closed&sort=updated&direction=desc&per_page=30",
                "closed pull requests",
            )?
            .unwrap_or_default();
        Ok(pulls
            .into_iter()
            .filter(|pull| self.is_release_pull(pull))
            .filter_map(|pull| {
                let merged_at = pull.merged_at?;
                let merge_commit = pull.merge_commit_sha.filter(|sha| is_object_id(sha))?;
                is_object_id(&pull.head.sha).then_some((
                    merged_at,
                    MergedPullRequest {
                        number: pull.number,
                        head: pull.head.sha,
                        merge_commit,
                    },
                ))
            })
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, pull)| pull))
    }

    fn is_release_pull(&self, pull: &PullRequest) -> bool {
        pull.head.name.starts_with(RELEASE_BRANCH_PREFIX)
            && pull
                .head
                .repo
                .as_ref()
                .is_some_and(|repo| repo.full_name.eq_ignore_ascii_case(&self.full_name()))
    }

    /// Open a pull request from `head` into `base`.
    pub(crate) fn open_pull_request(&self, head: &str, base: &str, title: &str, body: &str) -> RailResult<PullRequest> {
        let request = serde_json::json!({ "head": head, "base": base, "title": title, "body": body });
        self.send(
            with_json(self.request(Method::POST, "pulls")?, &request)?,
            "a release pull request",
        )?
        .ok_or_else(|| RailError::message("GitHub did not open the release pull request"))
    }
}

fn with_json(request: RequestBuilder, body: &serde_json::Value) -> RailResult<RequestBuilder> {
    Ok(request
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(body)?))
}

fn is_loopback(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Percent-encode a tag for one URL path segment.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A token from the environment, or from `gh` when it is installed and signed in.
fn token(host: &str) -> RailResult<String> {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return Ok(value.trim().to_owned());
        }
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .stdin(std::process::Stdio::null())
        .output();
    if let Ok(output) = output
        && output.status.success()
        && let Ok(value) = String::from_utf8(output.stdout)
        && !value.trim().is_empty()
    {
        return Ok(value.trim().to_owned());
    }
    Err(RailError::with_help(
        "a GitHub release needs a token",
        "set GH_TOKEN, or run `gh auth login`; set release.github_release = false and release.checks = \"none\" to release without GitHub",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_encode_as_one_path_segment() {
        assert_eq!(encode("cargo-rail-v0.32.0"), "cargo-rail-v0.32.0");
        assert_eq!(encode("a/b+c"), "a%2Fb%2Bc");
    }

    #[test]
    fn missing_assets_keep_configuration_order_and_ignore_partial_uploads() {
        let release = Release {
            id: 1,
            tag_name: "v1.0.0".into(),
            draft: true,
            html_url: String::new(),
            assets: vec![
                ReleaseAsset {
                    id: 1,
                    name: "b.zip".into(),
                    state: "uploaded".into(),
                    size: 1,
                    digest: None,
                },
                ReleaseAsset {
                    id: 2,
                    name: "a.zip".into(),
                    state: "starter".into(),
                    size: 1,
                    digest: None,
                },
            ],
        };
        let expected = ["c.zip".to_owned(), "a.zip".to_owned(), "b.zip".to_owned()];
        assert_eq!(release.missing_assets(&expected), ["c.zip", "a.zip"]);
    }
}
