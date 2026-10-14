//! Local stand-ins for the services a release talks to: GitHub's REST API, a Cargo sparse
//! registry, `ssh` for a `github.com` origin, and `gh attestation verify`.
//!
//! Each fake serves the real protocol the production code uses, so the release under test
//! takes its production path; tests read and change the fakes' state between runs.

use anyhow::{Context as _, Result, ensure};
use rscrypto::Sha256;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One HTTP request, as a fake handler sees it.
pub struct Request {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub body: Vec<u8>,
}

/// One HTTP response.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(value).expect("JSON response"),
        }
    }

    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    pub fn not_found() -> Self {
        Self::json(404, &json!({ "message": "Not Found" }))
    }
}

type Handler = Arc<dyn Fn(Request, SocketAddr) -> Response + Send + Sync>;

/// A one-request-per-connection HTTP/1.1 server on a loopback port.
struct Server {
    address: SocketAddr,
    stop: Arc<Mutex<bool>>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(handler: Handler) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(Mutex::new(false));
        let stopped = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if *stopped.lock().expect("server stop flag") {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || drop(serve(stream, &handler, address)));
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        drop(TcpStream::connect(self.address));
        if let Some(worker) = self.worker.take() {
            drop(worker.join());
        }
    }
}

fn serve(stream: TcpStream, handler: &Handler, address: SocketAddr) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut input = BufReader::new(stream);
    let mut line = String::new();
    input.read_line(&mut line)?;
    let words = line.split_whitespace().collect::<Vec<_>>();
    ensure!(words.len() == 3, "invalid request line {line:?}");
    let (method, path) = (words[0].to_owned(), words[1].to_owned());
    let mut length = 0;
    let mut authorization = String::new();
    loop {
        let mut header = String::new();
        ensure!(input.read_line(&mut header)? > 0, "truncated headers");
        if header == "\r\n" {
            break;
        }
        if let Some((key, value)) = header.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>()?;
            } else if key.eq_ignore_ascii_case("authorization") {
                authorization = value.trim().to_owned();
            } else if key.eq_ignore_ascii_case("expect") && value.trim() == "100-continue" {
                input.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            }
        }
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let response = handler(
        Request {
            method,
            path,
            authorization,
            body,
        },
        address,
    );
    let stream = input.get_mut();
    write!(stream, "HTTP/1.1 {} Fake\r\n", response.status)?;
    for (key, value) in &response.headers {
        write!(stream, "{key}: {value}\r\n")?;
    }
    write!(
        stream,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    Ok(())
}

// ----- GitHub ----------------------------------------------------------------------------

/// One check run on a commit.
#[derive(Clone)]
pub struct Check {
    pub name: String,
    pub status: &'static str,
    pub conclusion: Option<&'static str>,
    pub suite: u64,
}

impl Check {
    pub fn success(name: &str) -> Self {
        Self {
            name: name.into(),
            status: "completed",
            conclusion: Some("success"),
            suite: 1,
        }
    }
}

#[derive(Clone)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct Release {
    pub id: u64,
    pub tag: String,
    pub target: String,
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

#[derive(Clone)]
pub struct Pull {
    pub number: u64,
    pub head_ref: String,
    pub head_sha: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub merge_commit: Option<String>,
}

/// GitHub's state as the release sees it.
#[derive(Default)]
pub struct GithubState {
    /// Check runs per commit; a commit without an entry reports one passing check.
    pub checks: BTreeMap<String, Vec<Check>>,
    /// Commit statuses per commit.
    pub statuses: BTreeMap<String, Vec<(String, &'static str)>>,
    /// Workflow run ID to check suite ID.
    pub runs: BTreeMap<u64, u64>,
    pub releases: Vec<Release>,
    pub pulls: Vec<Pull>,
    /// The next request whose `METHOD path` starts with the key fails with this status, once.
    pub failures: BTreeMap<String, u16>,
    /// Every `METHOD path` served, in order.
    pub log: Vec<String>,
    next_id: u64,
}

impl GithubState {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn release_json(release: &Release, address: SocketAddr) -> Value {
        json!({
            "id": release.id,
            "tag_name": release.tag,
            "target_commitish": release.target,
            "name": release.name,
            "body": release.body,
            "draft": release.draft,
            "prerelease": release.prerelease,
            "html_url": format!("http://{address}/owner/repo/releases/{}", release.tag),
            "assets": release.assets.iter().map(|asset| json!({
                "id": asset.id,
                "name": asset.name,
                "state": "uploaded",
                "size": asset.bytes.len(),
                "digest": format!("sha256:{}", sha256(&asset.bytes)),
            })).collect::<Vec<_>>(),
        })
    }

    fn pull_json(pull: &Pull, address: SocketAddr) -> Value {
        json!({
            "number": pull.number,
            "html_url": format!("http://{address}/owner/repo/pull/{}", pull.number),
            "state": if pull.open { "open" } else { "closed" },
            "merged_at": pull.merge_commit.as_ref().map(|_| format!("2026-10-13T00:00:{:02}Z", pull.number)),
            "merge_commit_sha": pull.merge_commit,
            "head": { "ref": pull.head_ref, "sha": pull.head_sha, "repo": { "full_name": "owner/repo" } },
            "base": { "ref": pull.base },
        })
    }
}

/// A fake GitHub REST API for `owner/repo`.
pub struct FakeGithub {
    server: Server,
    pub state: Arc<Mutex<GithubState>>,
}

impl FakeGithub {
    pub fn start() -> Result<Self> {
        let state = Arc::new(Mutex::new(GithubState::default()));
        let shared = Arc::clone(&state);
        let server = Server::start(Arc::new(move |request, address| {
            let mut state = shared.lock().expect("GitHub state");
            github(&mut state, request, address)
        }))?;
        Ok(Self { server, state })
    }

    pub fn api_url(&self) -> String {
        format!("http://{}", self.server.address)
    }

    pub fn with<T>(&self, change: impl FnOnce(&mut GithubState) -> T) -> T {
        change(&mut self.state.lock().expect("GitHub state"))
    }

    pub fn release(&self, tag: &str) -> Option<Release> {
        self.with(|state| state.releases.iter().find(|release| release.tag == tag).cloned())
    }

    /// Attach a file to the release for `tag`, as the asset sub-action does.
    pub fn attach(&self, tag: &str, name: &str, bytes: &[u8]) -> Result<()> {
        self.with(|state| {
            let id = state.id();
            let release = state
                .releases
                .iter_mut()
                .find(|release| release.tag == tag)
                .context("no release to attach to")?;
            release.assets.retain(|asset| asset.name != name);
            release.assets.push(Asset {
                id,
                name: name.into(),
                bytes: bytes.to_vec(),
            });
            Ok(())
        })
    }
}

fn github(state: &mut GithubState, request: Request, address: SocketAddr) -> Response {
    let key = format!("{} {}", request.method, request.path);
    state.log.push(key.clone());
    if request.authorization != "Bearer test-token" && !request.path.starts_with("/download/") {
        return Response::json(401, &json!({ "message": "Bad credentials" }));
    }
    if let Some(prefix) = state
        .failures
        .keys()
        .find(|prefix| key.starts_with(prefix.as_str()))
        .cloned()
    {
        let status = state.failures.remove(&prefix).unwrap_or(500);
        return Response::json(status, &json!({ "message": "injected failure" }));
    }
    let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
    if let Some(id) = path.strip_prefix("/download/assets/") {
        let id = id.parse::<u64>().unwrap_or_default();
        return state
            .releases
            .iter()
            .flat_map(|release| &release.assets)
            .find(|asset| asset.id == id)
            .map_or_else(Response::not_found, |asset| Response::bytes(200, asset.bytes.clone()));
    }
    let Some(route) = path.strip_prefix("/repos/owner/repo/") else {
        return Response::not_found();
    };
    let page = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("page="))
        .and_then(|page| page.parse::<u32>().ok())
        .unwrap_or(1);
    let segments = route.split('/').collect::<Vec<_>>();
    let body = || serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["commits", sha, "check-runs"]) => {
            let checks = state
                .checks
                .get(*sha)
                .cloned()
                .unwrap_or_else(|| vec![Check::success("ci")]);
            let runs = if page == 1 { checks.clone() } else { Vec::new() };
            Response::json(
                200,
                &json!({
                    "total_count": checks.len(),
                    "check_runs": runs.iter().map(|check| json!({
                        "name": check.name,
                        "status": check.status,
                        "conclusion": check.conclusion,
                        "check_suite": { "id": check.suite },
                    })).collect::<Vec<_>>(),
                }),
            )
        }
        ("GET", ["commits", sha, "status"]) => {
            let statuses = state.statuses.get(*sha).cloned().unwrap_or_default();
            Response::json(
                200,
                &json!({ "statuses": statuses.iter().map(|(context, state)| json!({ "context": context, "state": state })).collect::<Vec<_>>() }),
            )
        }
        ("GET", ["actions", "runs", id]) => id
            .parse::<u64>()
            .ok()
            .and_then(|id| state.runs.get(&id))
            .map_or_else(Response::not_found, |suite| {
                Response::json(200, &json!({ "check_suite_id": suite }))
            }),
        ("GET", ["releases", "tags", tag]) => state
            .releases
            .iter()
            .find(|release| release.tag == *tag && !release.draft)
            .map_or_else(Response::not_found, |release| {
                Response::json(200, &GithubState::release_json(release, address))
            }),
        ("GET", ["releases"]) => {
            let releases = if page == 1 {
                state
                    .releases
                    .iter()
                    .rev()
                    .map(|release| GithubState::release_json(release, address))
                    .collect()
            } else {
                Vec::new()
            };
            Response::json(200, &Value::Array(releases))
        }
        ("POST", ["releases"]) => {
            let body = body();
            let id = state.id();
            let release = Release {
                id,
                tag: body["tag_name"].as_str().unwrap_or_default().to_owned(),
                target: body["target_commitish"].as_str().unwrap_or_default().to_owned(),
                name: body["name"].as_str().unwrap_or_default().to_owned(),
                body: body["body"].as_str().unwrap_or_default().to_owned(),
                draft: body["draft"].as_bool().unwrap_or_default(),
                prerelease: body["prerelease"].as_bool().unwrap_or_default(),
                assets: Vec::new(),
            };
            let response = GithubState::release_json(&release, address);
            state.releases.push(release);
            Response::json(201, &response)
        }
        ("PATCH", ["releases", id]) => {
            let draft = body()["draft"].as_bool();
            let Some(release) = state
                .releases
                .iter_mut()
                .find(|release| id.parse::<u64>().is_ok_and(|id| release.id == id))
            else {
                return Response::not_found();
            };
            if let Some(draft) = draft {
                release.draft = draft;
            }
            Response::json(200, &GithubState::release_json(release, address))
        }
        ("GET", ["releases", "assets", id]) => {
            // GitHub redirects asset downloads to signed storage on another host.
            Response {
                status: 302,
                headers: vec![("Location".into(), format!("http://{address}/download/assets/{id}"))],
                body: Vec::new(),
            }
        }
        ("GET", ["pulls"]) => {
            let open = !query.contains("state=closed");
            let pulls = state
                .pulls
                .iter()
                .filter(|pull| pull.open == open)
                .map(|pull| GithubState::pull_json(pull, address))
                .collect();
            Response::json(200, &Value::Array(pulls))
        }
        ("POST", ["pulls"]) => {
            let body = body();
            let number = state.id();
            let pull = Pull {
                number,
                head_ref: body["head"].as_str().unwrap_or_default().to_owned(),
                head_sha: String::new(),
                base: body["base"].as_str().unwrap_or_default().to_owned(),
                title: body["title"].as_str().unwrap_or_default().to_owned(),
                body: body["body"].as_str().unwrap_or_default().to_owned(),
                open: true,
                merge_commit: None,
            };
            let response = GithubState::pull_json(&pull, address);
            state.pulls.push(pull);
            Response::json(201, &response)
        }
        _ => Response::not_found(),
    }
}

// ----- Registry --------------------------------------------------------------------------

/// A Cargo sparse registry with publish, index, and download endpoints.
#[derive(Default)]
pub struct RegistryState {
    pub uploads: BTreeMap<String, Vec<u8>>,
    pub index: BTreeMap<String, Vec<Value>>,
    /// Every upload attempt, by `name@version`.
    pub attempts: Vec<String>,
    /// Packages whose next upload fails with 503, once.
    pub reject_once: Vec<String>,
}

pub struct FakeRegistry {
    server: Server,
    pub state: Arc<Mutex<RegistryState>>,
}

impl FakeRegistry {
    pub fn start() -> Result<Self> {
        let state = Arc::new(Mutex::new(RegistryState::default()));
        let shared = Arc::clone(&state);
        let server = Server::start(Arc::new(move |request, address| {
            let mut state = shared.lock().expect("registry state");
            registry(&mut state, request, address)
                .unwrap_or_else(|error| Response::json(500, &json!({ "errors": [{ "detail": error.to_string() }] })))
        }))?;
        Ok(Self { server, state })
    }

    pub fn index_url(&self) -> String {
        format!("sparse+http://{}/index/", self.server.address)
    }

    pub fn with<T>(&self, change: impl FnOnce(&mut RegistryState) -> T) -> T {
        change(&mut self.state.lock().expect("registry state"))
    }

    /// The `.cargo_vcs_info.json` commit of an uploaded package.
    pub fn source_commit(&self, name: &str, version: &str) -> Result<Option<String>> {
        let Some(archive) = self.with(|state| state.uploads.get(&format!("{name}@{version}")).cloned()) else {
            return Ok(None);
        };
        let mut tar = Vec::new();
        flate2::read::GzDecoder::new(archive.as_slice()).read_to_end(&mut tar)?;
        let wanted = format!("{name}-{version}/.cargo_vcs_info.json");
        let mut offset = 0;
        while offset + 512 <= tar.len() {
            let header = &tar[offset..offset + 512];
            if header.iter().all(|byte| *byte == 0) {
                break;
            }
            let field = |range: std::ops::Range<usize>| {
                let bytes = &header[range];
                let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            };
            let size = usize::from_str_radix(field(124..136).trim(), 8)?;
            let path = match field(345..500) {
                prefix if prefix.is_empty() => field(0..100),
                prefix => format!("{prefix}/{}", field(0..100)),
            };
            let data = &tar[offset + 512..offset + 512 + size];
            if path == wanted {
                let info: Value = serde_json::from_slice(data)?;
                return Ok(info["git"]["sha1"].as_str().map(str::to_owned));
            }
            offset += 512 + size.div_ceil(512) * 512;
        }
        Ok(None)
    }
}

fn registry(state: &mut RegistryState, request: Request, address: SocketAddr) -> Result<Response> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/index/config.json") => Ok(Response::json(
            200,
            &json!({ "api": format!("http://{address}"), "dl": format!("http://{address}/download/{{crate}}/{{version}}") }),
        )),
        ("GET", path) if path.starts_with("/index/") => {
            let name = path.rsplit('/').next().context("index name")?;
            Ok(match state.index.get(name) {
                Some(records) => Response::bytes(
                    200,
                    records
                        .iter()
                        .map(|record| format!("{record}\n"))
                        .collect::<String>()
                        .into_bytes(),
                ),
                None => Response::bytes(404, Vec::new()),
            })
        }
        ("GET", path) if path.starts_with("/download/") => {
            let mut parts = path.trim_start_matches("/download/").split('/');
            let (name, version) = (parts.next().context("name")?, parts.next().context("version")?);
            Ok(state.uploads.get(&format!("{name}@{version}")).map_or_else(
                || Response::bytes(404, Vec::new()),
                |bytes| Response::bytes(200, bytes.clone()),
            ))
        }
        ("PUT", "/api/v1/crates/new") => {
            let body = &request.body;
            let meta_size = usize::try_from(u32::from_le_bytes(body.get(..4).context("metadata size")?.try_into()?))?;
            let meta: Value = serde_json::from_slice(body.get(4..4 + meta_size).context("metadata")?)?;
            let offset = 4 + meta_size;
            let size = usize::try_from(u32::from_le_bytes(
                body.get(offset..offset + 4).context("crate size")?.try_into()?,
            ))?;
            let archive = body.get(offset + 4..).context("archive")?;
            ensure!(archive.len() == size, "invalid upload framing");
            ensure!(request.authorization == "fixture-token", "missing registry token");
            let name = meta["name"].as_str().context("name")?.to_owned();
            let version = meta["vers"].as_str().context("version")?.to_owned();
            let key = format!("{name}@{version}");
            state.attempts.push(key.clone());
            if let Some(index) = state.reject_once.iter().position(|rejected| rejected == &name) {
                state.reject_once.remove(index);
                return Ok(Response::json(
                    503,
                    &json!({ "errors": [{ "detail": "registry unavailable" }] }),
                ));
            }
            if state.uploads.contains_key(&key) {
                return Ok(Response::json(
                    400,
                    &json!({ "errors": [{ "detail": format!("crate version `{version}` is already uploaded") }] }),
                ));
            }
            let deps = meta["deps"]
                .as_array()
                .context("deps")?
                .iter()
                .map(|dep| {
                    json!({
                        "name": dep.get("explicit_name_in_toml").unwrap_or_else(|| &dep["name"]),
                        "package": dep["name"], "req": dep["version_req"], "features": dep["features"],
                        "optional": dep["optional"], "default_features": dep["default_features"],
                        "target": dep["target"], "kind": dep["kind"], "registry": dep["registry"],
                    })
                })
                .collect::<Vec<_>>();
            state.index.entry(name.clone()).or_default().push(json!({
                "name": name, "vers": version, "cksum": sha256(archive),
                "deps": deps, "features": meta["features"], "yanked": false,
            }));
            state.uploads.insert(key, archive.to_vec());
            Ok(Response::json(200, &json!({ "warnings": {} })))
        }
        _ => Ok(Response::bytes(404, Vec::new())),
    }
}

// ----- Programs --------------------------------------------------------------------------

/// Write the fake `ssh` that serves `github.com:owner/repo` from a bare repository.
pub fn write_ssh(directory: &Path, bare: &Path) -> Result<std::path::PathBuf> {
    let path = directory.join("ssh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$*\" in *git-receive-pack*) exec git-receive-pack '{0}' ;; *git-upload-pack*) exec git-upload-pack '{0}' ;; esac\nexit 1\n",
            bare.display()
        ),
    )?;
    executable(&path)?;
    Ok(path)
}

/// Write a fake `gh` whose `attestation verify` reports, in gh's JSON shape, the source commit
/// recorded in `$FAKE_GH_ATTESTED/<name>`, and checks the repository the release asked about.
pub fn write_gh(directory: &Path) -> Result<()> {
    let path = directory.join("gh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
[ "$1" = attestation ] && [ "$2" = verify ] || { echo "unexpected gh $*" >&2; exit 2; }
file=$3; shift 3
repo= signer= format=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --signer-repo) signer=$2; shift 2 ;;
    --format) format=$2; shift 2 ;;
    *) shift ;;
  esac
done
[ "$GH_TOKEN" = test-token ] || { echo "gh has no token" >&2; exit 1; }
[ "$repo" = owner/repo ] && [ "$signer" = owner/repo ] && [ "$format" = json ] || { echo "unexpected request $repo $signer $format" >&2; exit 1; }
name=$(basename "$file")
[ -f "$FAKE_GH_ATTESTED/$name" ] || { echo "no attestations found for $name" >&2; exit 1; }
printf '[{"verificationResult":{"signature":{"certificate":{"sourceRepositoryDigest":"%s"}}}}]\n' "$(cat "$FAKE_GH_ATTESTED/$name")"
"#,
    )?;
    executable(&path)
}

#[cfg(unix)]
fn executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Windows has no executable mode bit.
#[cfg(not(unix))]
fn executable(_path: &Path) -> Result<()> {
    Ok(())
}
