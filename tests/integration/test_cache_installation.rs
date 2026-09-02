//! Front-door coverage for transparent local compiler-cache installation.

use anyhow::{Context as _, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::helpers::TestWorkspace;

#[cfg(unix)]
struct UnchangedFileEvidence {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
struct UnchangedFileEvidence {
    _deny_write_and_delete: fs::File,
}

#[cfg(unix)]
fn capture_unchanged_file(path: &Path) -> Result<UnchangedFileEvidence> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::metadata(path).with_context(|| format!("read identity for {}", path.display()))?;
    Ok(UnchangedFileEvidence {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn capture_unchanged_file(path: &Path) -> Result<UnchangedFileEvidence> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    // The retained handle permits the reads required by setup while denying
    // writes and deletes. An in-place rewrite or atomic replacement therefore
    // fails with a sharing violation instead of passing through a weak timestamp
    // comparison.
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .with_context(|| format!("protect unchanged file {}", path.display()))?;
    Ok(UnchangedFileEvidence {
        _deny_write_and_delete: file,
    })
}

#[cfg(unix)]
fn assert_unchanged_file(path: &Path, expected: &UnchangedFileEvidence, description: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::metadata(path).with_context(|| format!("read identity for {}", path.display()))?;
    anyhow::ensure!(
        (metadata.dev(), metadata.ino()) == (expected.device, expected.inode),
        "{description} was replaced"
    );
    Ok(())
}

#[cfg(windows)]
fn assert_unchanged_file(path: &Path, _expected: &UnchangedFileEvidence, description: &str) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("read protected file {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "{description} is no longer a regular file");
    Ok(())
}

fn rail(workspace: &Path, cargo_home: &Path, arguments: &[&str]) -> Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(workspace)
        .args(arguments)
        .env("CARGO_HOME", cargo_home)
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()
        .context("run cargo-rail cache command")
}

fn selected_profile_status(workspace: &Path, cargo_home: &Path) -> Result<serde_json::Value> {
    let output = rail(
        workspace,
        cargo_home,
        &["rail", "cache", "status", "--scope", "local", "-f", "json"],
    )?;
    anyhow::ensure!(output.status.success(), "cache status failed: {output:?}");
    json(&output)
}

fn selected_profile_cache_root(workspace: &Path, cargo_home: &Path) -> Result<PathBuf> {
    let status = selected_profile_status(workspace, cargo_home)?;
    status["status"]["local"]["cache"]["root"]
        .as_str()
        .map(PathBuf::from)
        .context("selected profile cache root")
}

fn selected_profile_state_root(workspace: &Path, cargo_home: &Path) -> Result<PathBuf> {
    let status = selected_profile_status(workspace, cargo_home)?;
    let profile_id = status["status"]["installation"]["profile_id"]
        .as_str()
        .context("selected profile ID")?;
    Ok(cargo_home.join("cargo-rail/cache-profiles-v1/state").join(profile_id))
}

#[cfg(unix)]
fn profile_authority_projection(status: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "profile_id": status["status"]["installation"]["profile_id"],
        "bound_workspace_root": status["status"]["installation"]["bound_workspace_root"],
        "trust_domain": status["status"]["installation"]["trust_domain"],
        "cache_root": status["status"]["local"]["cache"]["root"],
        "max_bytes": status["status"]["installation"]["max_bytes"],
        "root_portability": status["status"]["installation"]["root_portability"],
        "remote_authority": status["status"]["remote"]["authority"],
        "remote_mode": status["status"]["remote"]["mode"],
        "selection_source": status["status"]["remote"]["selection_source"],
    })
}

fn remote_probe(workspace: &Path, cargo_home: &Path, remote: &str, mode: &str) -> Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(workspace)
        .args(["rail", "cache", "probe", "-f", "json"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_RAIL_CACHE_REMOTE", remote)
        .env("CARGO_RAIL_CACHE_MODE", mode)
        .env("AWS_ACCESS_KEY_ID", "fixture-access-key")
        .env("AWS_SECRET_ACCESS_KEY", "fixture-secret-key")
        .env("AWS_SESSION_TOKEN", "fixture-session-token")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", workspace.join("missing-aws-config"))
        .env("AWS_SHARED_CREDENTIALS_FILE", workspace.join("missing-aws-credentials"))
        .env_remove("AWS_ENDPOINT_URL")
        .env_remove("AWS_ENDPOINT_URL_S3")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_DEFAULT_PROFILE")
        .output()
        .context("probe loopback remote cache")
}

fn cargo_check(workspace: &Path, cargo_home: &Path, rustc: Option<&Path>, cache: Option<&str>) -> Result<Output> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .args(["check", "--quiet"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    if let Some(rustc) = rustc {
        command.env("RUSTC", rustc);
    }
    if let Some(cache) = cache {
        command.env("CARGO_RAIL_CACHE", cache);
    }
    command.output().context("run isolated cargo check")
}

#[cfg(unix)]
fn cargo_check_remote(
    workspace: &Path,
    cargo_home: &Path,
    remote: &str,
    mode: &str,
    rustc: Option<&Path>,
    coverage: Option<&Path>,
) -> Result<Output> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .args(["check", "--quiet"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_RAIL_CACHE_REMOTE", remote)
        .env("CARGO_RAIL_CACHE_MODE", mode)
        .env("AWS_ACCESS_KEY_ID", "fixture-access-key")
        .env("AWS_SECRET_ACCESS_KEY", "fixture-secret-key")
        .env("AWS_SESSION_TOKEN", "fixture-session-token")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", workspace.join("missing-aws-config"))
        .env("AWS_SHARED_CREDENTIALS_FILE", workspace.join("missing-aws-credentials"))
        .env_remove("AWS_ENDPOINT_URL")
        .env_remove("AWS_ENDPOINT_URL_S3")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_DEFAULT_PROFILE")
        .env_remove("OUT_DIR")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    if let Some(rustc) = rustc {
        command
            .env("RUSTC", rustc)
            .env("REAL_RUSTC", "rustc")
            .env("REMOTE_ENV_LOG", workspace.join("remote-compiler-environment.log"));
    }
    if let Some(coverage) = coverage {
        let coverage = fs::canonicalize(coverage).context("canonicalize native-cache coverage directory")?;
        command
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", coverage);
    }
    command.output().context("run cargo check with loopback remote cache")
}

fn cargo_check_installed_remote(workspace: &Path, cargo_home: &Path, coverage: &Path) -> Result<Output> {
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, None, None)
}

fn cargo_check_installed_remote_in_target(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    target: &Path,
) -> Result<Output> {
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, None, Some(target))
}

fn cargo_check_installed_remote_with_rustflags(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    rustflags: Option<&str>,
) -> Result<Output> {
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, rustflags, None)
}

fn cargo_check_installed_remote_with_options(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    rustflags: Option<&str>,
    target: Option<&Path>,
) -> Result<Output> {
    let coverage = fs::canonicalize(coverage).context("canonicalize native-cache coverage directory")?;
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .args(["check", "--quiet"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
        .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", coverage)
        .env("AWS_ACCESS_KEY_ID", "fixture-access-key")
        .env("AWS_SECRET_ACCESS_KEY", "fixture-secret-key")
        .env("AWS_SESSION_TOKEN", "fixture-session-token")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_CONFIG_FILE", workspace.join("missing-aws-config"))
        .env("AWS_SHARED_CREDENTIALS_FILE", workspace.join("missing-aws-credentials"))
        .env_remove("AWS_ENDPOINT_URL")
        .env_remove("AWS_ENDPOINT_URL_S3")
        .env_remove("CARGO_RAIL_CACHE_REMOTE")
        .env_remove("CARGO_RAIL_CACHE_MODE")
        .env_remove("CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT")
        .env_remove("AWS_SECURITY_TOKEN")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_DEFAULT_PROFILE")
        .env_remove("OUT_DIR")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    if let Some(rustflags) = rustflags {
        command.env("RUSTFLAGS", rustflags);
    } else {
        command.env_remove("RUSTFLAGS");
    }
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    } else {
        command.env_remove("CARGO_TARGET_DIR");
    }
    command.output().context("run cargo check with installed remote policy")
}

#[derive(Clone)]
struct FixtureObject {
    body: Vec<u8>,
    etag: String,
}

#[derive(Default)]
struct LoopbackS3State {
    objects: BTreeMap<String, FixtureObject>,
    requests: Vec<(String, String)>,
    generation: u64,
}

struct LoopbackS3 {
    address: SocketAddr,
    state: Arc<Mutex<LoopbackS3State>>,
    #[cfg(unix)]
    available: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LoopbackS3 {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(Mutex::new(LoopbackS3State::default()));
        let available = Arc::new(AtomicBool::new(true));
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_available = Arc::clone(&available);
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            while !worker_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let request_state = Arc::clone(&worker_state);
                        let request_available = Arc::clone(&worker_available);
                        requests.push(thread::spawn(move || {
                            drop(serve_s3_request(
                                stream,
                                &request_state,
                                request_available.load(Ordering::Acquire),
                            ));
                        }));
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
                let mut index = 0usize;
                while index < requests.len() {
                    if requests[index].is_finished() {
                        let request = requests.swap_remove(index);
                        drop(request.join());
                    } else {
                        index = index.saturating_add(1);
                    }
                }
            }
            for request in requests {
                drop(request.join());
            }
        });
        Ok(Self {
            address,
            state,
            #[cfg(unix)]
            available,
            stopping,
            worker: Some(worker),
        })
    }

    fn remote_url(&self) -> String {
        format!("s3+http://{}/fixture-bucket/team?region=test-1", self.address)
    }

    #[cfg(unix)]
    fn request_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.requests.len())
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .map_or_else(|_| Vec::new(), |state| state.requests.clone())
    }

    #[cfg(unix)]
    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::Release);
    }

    #[cfg(unix)]
    fn corrupt_result(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(object) = state
            .objects
            .iter_mut()
            .find_map(|(key, object)| key.contains("/entries/").then_some(object))
        else {
            return false;
        };
        let Some(last) = object.body.last_mut() else {
            return false;
        };
        *last ^= 0xff;
        object.etag = "\"corrupt-result\"".to_string();
        true
    }

    fn corrupt_protocol_marker(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(object) = state
            .objects
            .iter_mut()
            .find_map(|(key, object)| key.ends_with("/protocol").then_some(object))
        else {
            return false;
        };
        object.body = b"incompatible protocol".to_vec();
        object.etag = "\"corrupt-protocol\"".to_string();
        true
    }
}

impl Drop for LoopbackS3 {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        drop(TcpStream::connect(self.address));
        if let Some(worker) = self.worker.take() {
            drop(worker.join());
        }
    }
}

fn serve_s3_request(
    mut stream: TcpStream,
    state: &Arc<Mutex<LoopbackS3State>>,
    available: bool,
) -> std::io::Result<()> {
    // Windows accepted sockets can inherit the listener's nonblocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let header_end = loop {
        if request.len() > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture request headers exceeded their bound",
            ));
        }
        let mut buffer = [0_u8; 16 * 1024];
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(offset) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let header = std::str::from_utf8(&request[..header_end])
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture request header was not UTF-8"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture request line was absent"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let target = request_parts.next().unwrap_or_default();
    let path = target.split('?').next().unwrap_or(target).to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > 64 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture request body exceeded its bound",
        ));
    }
    while request.len() < header_end.saturating_add(content_length) {
        let mut buffer = [0_u8; 64 * 1024];
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    if request.len() != header_end.saturating_add(content_length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "fixture request body was truncated",
        ));
    }
    let encoded_body = &request[header_end..];
    let body = if headers
        .get("content-encoding")
        .is_some_and(|value| value.split(',').any(|encoding| encoding.trim() == "aws-chunked"))
    {
        decode_chunked_fixture_body(encoded_body)?
    } else {
        encoded_body.to_vec()
    };

    let mut state = state
        .lock()
        .map_err(|_| std::io::Error::other("fixture state lock was poisoned"))?;
    state.requests.push((method.clone(), path.clone()));
    if !available {
        return write_s3_error(&mut stream, 503, "ServiceUnavailable");
    }
    match method.as_str() {
        "GET" => match state.objects.get(&path) {
            Some(object) => write_s3_response(&mut stream, 200, &object.etag, &object.body),
            None => write_s3_error(&mut stream, 404, "NoSuchKey"),
        },
        "PUT" => {
            let allowed = match (
                headers.get("if-none-match"),
                headers.get("if-match"),
                state.objects.get(&path),
            ) {
                (Some(value), _, None) if value == "*" => true,
                (Some(value), _, Some(_)) if value == "*" => false,
                (_, Some(expected), Some(object)) => expected == &object.etag,
                (_, Some(_), None) => false,
                (None, None, _) => true,
                _ => false,
            };
            if !allowed {
                return write_s3_error(&mut stream, 412, "PreconditionFailed");
            }
            state.generation = state.generation.saturating_add(1);
            let etag = format!("\"fixture-{}\"", state.generation);
            state.objects.insert(
                path,
                FixtureObject {
                    body,
                    etag: etag.clone(),
                },
            );
            write_s3_response(&mut stream, 200, &etag, b"")
        }
        _ => write_s3_error(&mut stream, 405, "MethodNotAllowed"),
    }
}

fn decode_chunked_fixture_body(encoded: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut offset = 0usize;
    loop {
        let line_end = encoded[offset..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|position| offset + position)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture chunk header was truncated")
            })?;
        let header = std::str::from_utf8(&encoded[offset..line_end])
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture chunk header was not UTF-8"))?;
        let length = usize::from_str_radix(header.split(';').next().unwrap_or_default(), 16)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture chunk length was invalid"))?;
        offset = line_end.saturating_add(2);
        if length == 0 {
            break;
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixture chunk length overflowed"))?;
        if end.saturating_add(2) > encoded.len() || &encoded[end..end + 2] != b"\r\n" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "fixture chunk payload was truncated",
            ));
        }
        decoded.extend_from_slice(&encoded[offset..end]);
        offset = end + 2;
    }
    Ok(decoded)
}

fn write_s3_response(stream: &mut TcpStream, status: u16, etag: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nETag: {etag}\r\nx-amz-request-id: fixture\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn write_s3_error(stream: &mut TcpStream, status: u16, code: &str) -> std::io::Result<()> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{code}</Message><RequestId>fixture</RequestId><HostId>fixture</HostId></Error>"
    );
    write!(
        stream,
        "HTTP/1.1 {status} Error\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nx-amz-request-id: fixture\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn coverage_events(directory: &Path) -> Result<Vec<serde_json::Value>> {
    fs::read_dir(directory)?
        .map(|entry| {
            let path = entry?.path();
            serde_json::from_slice(&fs::read(path)?).context("decode native-cache coverage event")
        })
        .collect()
}

fn json(output: &Output) -> Result<serde_json::Value> {
    serde_json::from_slice(&output.stdout).context("decode command JSON")
}

#[test]
fn setup_check_in_a_source_checkout_reports_the_missing_component_recovery() {
    let result: Result<()> = (|| {
        let source_checkout = tempfile::tempdir()?;
        let source_root = source_checkout.path();
        let source_target = source_root.join("target");
        fs::create_dir_all(&source_target)?;
        let isolated_bin = tempfile::Builder::new()
            .prefix("cargo-rail-missing-worker-")
            .tempdir_in(&source_target)?;
        let executable_name = Path::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .file_name()
            .context("cargo-rail test binary has no file name")?;
        let executable = isolated_bin.path().join(executable_name);
        fs::copy(env!("CARGO_BIN_EXE_cargo-rail"), &executable)?;
        let cargo_home = tempfile::tempdir()?;

        let output = Command::new(&executable)
            .current_dir(source_root)
            .args(["rail", "cache", "setup", "--check"])
            .env("CARGO_HOME", cargo_home.path())
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;

        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("native compiler cache worker executable is unavailable"),
            "{stderr}"
        );
        assert!(stderr.contains("just build-all"), "{stderr}");
        assert!(stderr.contains("cargo rail cache setup --check"), "{stderr}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn setup_preview_apply_repeat_status_and_exact_remove_are_lossless() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-setup", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(cargo_home.path(), fs::Permissions::from_mode(0o755))?;
        }
        let config = cargo_home.path().join("config.toml");
        let original = "# retained\n[net]\noffline = true\n";
        fs::write(&config, original)?;

        let check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check", "-f", "json"],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "setup preview must report changes: {check:?}"
        );
        assert_eq!(
            fs::read_to_string(&config)?,
            original,
            "setup preview mutated Cargo config"
        );
        assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());

        let apply = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--max-size", "32MiB", "-f", "json"],
        )?;
        assert!(apply.status.success(), "setup failed: {apply:?}");
        let applied = json(&apply)?;
        assert_eq!(applied["changed"], true);
        assert_eq!(applied["pending"], false);
        assert_eq!(applied["max_bytes"], 32 * 1024 * 1024);
        let configured = fs::read_to_string(&config)?;
        assert!(configured.starts_with(original));
        assert!(configured.contains("rustc-wrapper"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(cargo_home.path())?.permissions().mode() & 0o777, 0o755);
        }

        let repeated = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check", "-f", "json"],
        )?;
        assert!(
            repeated.status.success(),
            "repeat setup was not idempotent: {repeated:?}"
        );
        assert_eq!(json(&repeated)?["pending"], false);

        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert!(status.status.success(), "installation status failed: {status:?}");
        let status = json(&status)?;
        assert_eq!(status["status"]["schema_version"], 15);
        assert_eq!(status["status"]["installation"]["state"], "installed");
        assert_eq!(status["status"]["installation"]["healthy"], true);
        assert_eq!(status["status"]["installation"]["root_portability"], "physical");
        let wrapper = PathBuf::from(
            status["status"]["installation"]["wrapper_path"]
                .as_str()
                .context("installed wrapper path")?,
        );
        let profile_cache_root = PathBuf::from(
            status["status"]["local"]["cache"]["root"]
                .as_str()
                .context("profile cache root")?,
        );

        fs::remove_file(&wrapper)?;
        let drifted = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert_eq!(json(&drifted)?["status"]["installation"]["state"], "drifted");
        let repair_check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check"],
        )?;
        assert_eq!(repair_check.status.code(), Some(1));
        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "setup repair failed: {repair:?}");
        assert!(wrapper.is_file(), "setup repair did not restore the owned wrapper");
        #[cfg(not(windows))]
        let worker = wrapper.with_file_name("cargo-rail-native-rustc-worker");
        #[cfg(windows)]
        let worker = wrapper.with_file_name("cargo-rail-native-rustc-worker.exe");
        assert!(worker.is_file(), "setup did not install the owned worker");
        fs::remove_file(&worker)?;
        let drifted = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert_eq!(json(&drifted)?["status"]["installation"]["state"], "drifted");
        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "worker repair failed: {repair:?}");
        assert!(worker.is_file(), "setup repair did not restore the owned worker");

        let remove_check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "uninstall", "--check", "-f", "json"],
        )?;
        assert_eq!(remove_check.status.code(), Some(1));
        assert!(config.exists(), "removal preview mutated Cargo config");
        let remove = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "uninstall", "-f", "json"],
        )?;
        assert!(remove.status.success(), "removal failed: {remove:?}");
        assert_eq!(fs::read_to_string(&config)?, original);
        assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());
        assert!(profile_cache_root.exists(), "uninstall deleted the profile CAS");
        assert!(
            cargo_home.path().join("cargo-rail/cache-profiles-v1").is_dir(),
            "uninstall deleted the profile registry"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn markerless_local_cas_recovery_quarantines_every_byte_before_reinitializing() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-recovery", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let seeded = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(seeded.status.success(), "cache seed failed: {seeded:?}");

        let root = fs::canonicalize(selected_profile_cache_root(&workspace.path, cargo_home.path())?)?;
        fs::remove_file(root.join("OWNER"))?;
        fs::remove_file(root.join("CAPACITY.json"))?;
        fs::remove_file(root.join("NATIVE_LEDGER.json"))?;
        let retained = directory_snapshot(&root)?;
        assert!(!retained.is_empty(), "partial CAS fixture retained no cache bytes");

        let preview = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "recover", "--check", "-f", "json"],
        )?;
        assert_eq!(
            preview.status.code(),
            Some(1),
            "recovery preview did not report pending work"
        );
        let preview = json(&preview)?;
        assert_eq!(preview["pending"], true);
        assert_eq!(preview["recovery"]["selected_root"], root.to_string_lossy().as_ref());
        let quarantine = PathBuf::from(
            preview["recovery"]["quarantine_root"]
                .as_str()
                .context("quarantine path")?,
        );
        let receipt = PathBuf::from(
            preview["recovery"]["receipt_path"]
                .as_str()
                .context("recovery receipt path")?,
        );
        assert!(root.is_dir());
        assert!(!quarantine.exists());
        assert!(!receipt.exists());

        let applied = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "recover", "-f", "json"],
        )?;
        assert!(applied.status.success(), "recovery failed: {applied:?}");
        let applied = json(&applied)?;
        assert_eq!(
            applied["recovery"]["quarantine_root"],
            quarantine.to_string_lossy().as_ref()
        );
        assert_eq!(directory_snapshot(&quarantine)?, retained);
        assert!(root.join("OWNER").is_file());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&receipt)?)?["state"],
            "completed"
        );

        let repeated = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "recover", "--check", "-f", "json"],
        )?;
        assert!(
            repeated.status.success(),
            "repeated recovery was not clean: {repeated:?}"
        );
        assert_eq!(json(&repeated)?["pending"], false);

        fs::remove_dir_all(workspace.path.join("target"))?;
        let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(cold.status.success(), "fresh CAS did not populate: {cold:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let reused = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(reused.status.success(), "fresh CAS did not restore: {reused:?}");
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert!(
            json(&status)?["status"]["installation"]["usage"]["hits"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn receipt_qualified_local_distribution_executes_an_ordinary_cargo_library() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("distributed_front_door", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
        }
        let coverage = fs::canonicalize(coverage.path())?;

        let preview = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--distributed-local", "--check", "-f", "json"],
        )?;
        assert_eq!(
            preview.status.code(),
            Some(1),
            "qualification preview failed: {preview:?}"
        );
        assert_eq!(json(&preview)?["distributed"], "local_process_qualification_v1");
        assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());

        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--distributed-local", "-f", "json"],
        )?;
        assert!(setup.status.success(), "qualification setup failed: {setup:?}");
        assert_eq!(json(&setup)?["distributed"], "local_process_qualification_v1");
        let installation = cargo_home.path().join("cargo-rail/compiler-cache-v1");
        #[cfg(not(windows))]
        let distributed_worker = installation.join("cargo-rail-distributed-worker");
        #[cfg(windows)]
        let distributed_worker = installation.join("cargo-rail-distributed-worker.exe");
        assert!(
            distributed_worker.is_file(),
            "setup omitted the receipt-owned distributed worker"
        );

        let build = || {
            Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["build", "--release", "--lib", "--message-format=json"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
                .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
                .env_remove("OUT_DIR")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()
        };
        let built = build()?;
        assert!(
            built.status.success(),
            "ordinary Cargo did not seed its first-seen compiler environment\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        let seeded_events = coverage_events(&coverage)?;
        assert!(
            seeded_events.iter().any(|event| {
                event["status"] == "miss"
                    && event["reason"].as_str().is_some_and(|reason| {
                        reason.starts_with("environment_selector_not_found;stored_verified_result")
                    })
            }),
            "ordinary Cargo did not establish local compiler-environment authority: {seeded_events:?}"
        );
        fs::remove_dir_all(workspace.path.join("target"))?;
        let cache_roots = fs::read_dir(cargo_home.path().join("cargo-rail"))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("local-cas-v2"))
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        let [cache_root] = cache_roots.as_slice() else {
            anyhow::bail!("local distribution test did not contain one local CAS root: {cache_roots:?}");
        };
        for entry in fs::read_dir(cache_root.path().join("native-actions-v2"))? {
            fs::remove_file(entry?.path())?;
        }
        for entry in fs::read_dir(&coverage)? {
            fs::remove_file(entry?.path())?;
        }

        let built = build()?;
        assert!(
            built.status.success(),
            "ordinary Cargo did not complete through local distribution\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        let events = coverage_events(&coverage)?;
        assert!(
            events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution"),
            "ordinary Cargo never crossed the distributed admission boundary\nstderr:\n{}\nevents: {events:?}",
            String::from_utf8_lossy(&built.stderr)
        );

        fs::write(
            workspace.path.join("src/lib.rs"),
            "compile_error!(\"distributed failure proof\");\n",
        )?;
        fs::remove_dir_all(workspace.path.join("target"))?;
        let failed = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["build", "--release", "--lib", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env_remove("OUT_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            !failed.status.success(),
            "worker compiler failure unexpectedly succeeded"
        );
        let diagnostics = [failed.stdout.as_slice(), failed.stderr.as_slice()].concat();
        assert!(
            diagnostics
                .windows(b"distributed failure proof".len())
                .any(|window| window == b"distributed failure proof"),
            "first-seen compiler diagnostics were not preserved: {failed:?}"
        );
        assert!(
            !diagnostics
                .windows(b"/cargo-rail/exec/v3".len())
                .any(|window| window == b"/cargo-rail/exec/v3"),
            "distributed virtual paths escaped into Cargo diagnostics: {failed:?}"
        );

        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        let status = json(&status)?;
        assert_eq!(status["status"]["installation"]["state"], "installed");
        assert_eq!(
            status["status"]["installation"]["distributed"],
            "local_process_qualification_v1"
        );

        fs::write(&distributed_worker, b"drifted")?;
        let drifted = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert_eq!(json(&drifted)?["status"]["installation"]["state"], "drifted");
        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "qualification repair failed: {repair:?}");
        assert_ne!(fs::read(&distributed_worker)?, b"drifted");

        let remove = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "uninstall"])?;
        assert!(remove.status.success(), "qualification removal failed: {remove:?}");
        assert!(
            !distributed_worker.exists(),
            "removal retained the receipt-owned worker"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(debug_assertions)]
#[test]
fn failure_reason_counters_remain_live_after_the_usage_ledger_fills() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-failure-telemetry", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "cache setup failed: {setup:?}");

        let profile_state = selected_profile_state_root(&workspace.path, cargo_home.path())?;
        fs::write(profile_state.join("usage-v1.log"), vec![b'B'; 64 * 1024])?;
        let phases = [
            ("action_capture", "complete_action_capture_unavailable"),
            ("action_identity", "complete_action_identity_unavailable"),
            (
                "post_execution_witness",
                "post_execution_witness_validation_unavailable",
            ),
        ];
        let mut expected = BTreeMap::from([
            ("complete_action_capture_unavailable", 0_u64),
            ("complete_action_identity_unavailable", 0_u64),
            ("post_execution_witness_validation_unavailable", 0_u64),
        ]);

        for (index, (phase, reason)) in phases.into_iter().enumerate() {
            fs::write(
                workspace.path.join("src/lib.rs"),
                format!("pub fn telemetry_value() -> usize {{ {index} }}\n"),
            )?;
            let compiled = Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["check", "--quiet"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_TEST_NATIVE_ACTION_FAULT", phase)
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?;
            assert!(
                compiled.status.success(),
                "injected {phase} failure changed the compiler result: {compiled:?}"
            );
            *expected.get_mut(reason).context("known failure reason")? += 1;

            let status = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "status", "--scope", "local", "-f", "json"],
            )?;
            assert!(status.status.success(), "cache status failed: {status:?}");
            let status = json(&status)?;
            let installation_status = &status["status"]["installation"];
            let usage = &installation_status["usage"];
            assert_eq!(installation_status["healthy"], true);
            assert_eq!(usage["recorded_events"], 64 * 1024);
            assert_eq!(usage["ledger_full"], true);
            assert_eq!(usage["failure_reason_counts_available"], true);
            for (candidate, count) in &expected {
                assert_eq!(
                    usage["failure_reasons"][candidate], *count,
                    "{phase} incremented the wrong stable failure class: {usage}"
                );
            }
        }

        let human = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(human.status.success(), "human cache status failed: {human:?}");
        let human = String::from_utf8_lossy(&human.stdout);
        for reason in expected.keys() {
            assert!(
                human.contains(&format!("Cache failure {reason}: 1")),
                "human status omitted {reason}:\n{human}"
            );
        }

        let counters = profile_state.join("failure-counters-v1.json");
        let counter_lock = profile_state.join("failure-counters-v1.lock");
        assert!(fs::metadata(&counters)?.len() <= 4 * 1024);
        assert_eq!(fs::metadata(&counter_lock)?.len(), 0);

        fs::write(&counters, b"{}\n")?;
        let unavailable = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        let unavailable = json(&unavailable)?;
        assert_eq!(unavailable["status"]["installation"]["healthy"], true);
        assert_eq!(
            unavailable["status"]["installation"]["usage"]["failure_reason_counts_available"],
            false
        );

        let status = selected_profile_status(&workspace.path, cargo_home.path())?;
        let profile_id = status["status"]["installation"]["profile_id"]
            .as_str()
            .context("profile ID")?
            .to_string();
        let detach = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "detach"])?;
        assert!(detach.status.success(), "profile detach failed: {detach:?}");
        let remove = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "drop-profile", "--profile", &profile_id],
        )?;
        assert!(remove.status.success(), "profile removal failed: {remove:?}");
        assert!(!counters.exists());
        assert!(!counter_lock.exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn setup_refuses_global_conflicts_and_workspace_shadowing() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-conflict", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let config = cargo_home.path().join("config.toml");
        fs::write(&config, "[build]\nrustc-wrapper = 'sccache'\n")?;
        let conflict = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check"],
        )?;
        assert_eq!(conflict.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&conflict.stderr).contains("already selects rustc wrapper"));

        fs::write(&config, "[net]\noffline = true\n")?;
        for name in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
            let shadowed = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
                .current_dir(&workspace.path)
                .args(["rail", "cache", "setup", "--check"])
                .env("CARGO_HOME", cargo_home.path())
                .env(name, "environment-wrapper")
                .env_remove(if name == "RUSTC_WRAPPER" {
                    "CARGO_BUILD_RUSTC_WRAPPER"
                } else {
                    "RUSTC_WRAPPER"
                })
                .output()?;
            assert_eq!(shadowed.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&shadowed.stderr).contains("shadows Cargo's user rustc-wrapper setting"));
        }
        fs::create_dir_all(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            "[build]\nrustc-wrapper = 'workspace-wrapper'\n",
        )?;
        let shadowed = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check"],
        )?;
        assert_eq!(shadowed.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&shadowed.stderr).contains("shadows the user rustc-wrapper setting"));
        assert_eq!(fs::read_to_string(config)?, "[net]\noffline = true\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cache_status_reports_only_redacted_machine_selected_remote_authority() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-remote-status", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(&workspace.path)
            .args(["rail", "cache", "status", "--scope", "local", "-f", "json"])
            .env("CARGO_HOME", cargo_home.path())
            .env(
                "CARGO_RAIL_CACHE_REMOTE",
                "s3://cargo-rail-cache-fixture/cache?region=us-east-1&owner=123456789012",
            )
            .env("CARGO_RAIL_CACHE_MODE", "read")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .output()?;
        assert!(output.status.success(), "remote status failed: {output:?}");
        let value = json(&output)?;
        assert_eq!(value["status"]["schema_version"], 15);
        assert_eq!(value["status"]["remote"]["activation"], "direct_transport_selected");
        assert_eq!(value["status"]["remote"]["provider"], "aws-s3");
        assert_eq!(value["status"]["remote"]["mode"], "read");
        assert!(value["status"]["remote"].get("normalized_url").is_none());
        assert!(
            value["status"]["remote"]["authority"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("remote-authority-v1-sha256-"))
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn setup_owned_remote_is_automatic_coordinated_and_removable() {
    let result: Result<()> = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-remote-setup", "0.1.0")?;
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();

        let seed_home = tempfile::tempdir()?;
        let seed_setup = rail(
            &workspace.path,
            seed_home.path(),
            &[
                "rail",
                "cache",
                "setup",
                "--remote",
                &remote_url,
                "--remote-mode",
                "read-write",
                "-f",
                "json",
            ],
        )?;
        assert!(seed_setup.status.success(), "remote setup failed: {seed_setup:?}");
        let setup_value = json(&seed_setup)?;
        assert_eq!(setup_value["remote"]["activation"], "direct_transport_selected");
        assert_eq!(setup_value["remote"]["mode"], "read-write");
        assert!(setup_value["remote"].get("normalized_url").is_none());

        let seed_coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        fs::set_permissions(seed_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let seed = cargo_check_installed_remote(&workspace.path, seed_home.path(), seed_coverage.path())?;
        assert!(seed.status.success(), "automatic remote seed failed: {seed:?}");
        let seed_events = coverage_events(seed_coverage.path())?;
        assert_setup_owned_remote_transport(&seed_events, "seed");
        let seed_requests = remote.requests();
        assert!(
            seed_requests
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
            "automatic remote seed did not publish an entry: requests={seed_requests:?}, events={seed_events:?}"
        );

        fs::remove_dir_all(workspace.path.join("target"))?;
        let import_home = tempfile::tempdir()?;
        let import_setup = rail(
            &workspace.path,
            import_home.path(),
            &[
                "rail",
                "cache",
                "setup",
                "--remote",
                &remote_url,
                "--remote-mode",
                "read",
            ],
        )?;
        assert!(
            import_setup.status.success(),
            "remote import setup failed: {import_setup:?}"
        );
        let import_coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        fs::set_permissions(import_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let imported = cargo_check_installed_remote(&workspace.path, import_home.path(), import_coverage.path())?;
        assert!(
            imported.status.success(),
            "automatic remote import failed: {imported:?}"
        );
        let imported_events = coverage_events(import_coverage.path())?;
        assert!(
            imported_events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_remote_result"),
            "ordinary Cargo did not restore the setup-owned remote result: {imported_events:?}"
        );
        assert_setup_owned_remote_transport(&imported_events, "import");

        let local_only = rail(
            &workspace.path,
            import_home.path(),
            &["rail", "cache", "setup", "--local-only", "-f", "json"],
        )?;
        assert!(local_only.status.success(), "local-only setup failed: {local_only:?}");
        assert!(json(&local_only)?["remote"].is_null());
        let status = rail(
            &workspace.path,
            import_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert!(status.status.success(), "local-only status failed: {status:?}");
        assert!(json(&status)?["status"]["remote"].is_null());
        let remove = rail(&workspace.path, import_home.path(), &["rail", "cache", "uninstall"])?;
        assert!(
            remove.status.success(),
            "coordinator installation removal failed: {remove:?}"
        );
        let installation = import_home.path().join("cargo-rail/compiler-cache-v1");
        let residue = fs::read_dir(&installation)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            !installation.exists(),
            "coordinator state survived exact installation removal: {residue:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn one_global_wrapper_isolates_three_concurrent_workspace_profiles_and_an_unenrolled_workspace() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspaces = [
            TestWorkspace::new_single_crate("profile-alpha", "0.1.0")?,
            TestWorkspace::new_single_crate("profile-beta", "0.1.0")?,
            TestWorkspace::new_single_crate("profile-gamma", "0.1.0")?,
        ];
        let cargo_home = tempfile::tempdir()?;
        let remotes = [LoopbackS3::start()?, LoopbackS3::start()?, LoopbackS3::start()?];
        let remote_urls = remotes.each_ref().map(|remote| remote.remote_url());

        let first = rail(
            &workspaces[0].path,
            cargo_home.path(),
            &[
                "rail",
                "cache",
                "setup",
                "--remote",
                &remote_urls[0],
                "--remote-mode",
                "read-write",
                "--root-portability",
                "remap",
            ],
        )?;
        assert!(first.status.success(), "first profile setup failed: {first:?}");

        let mut setup_jobs = Vec::new();
        for index in 1..3 {
            let workspace = workspaces[index].path.clone();
            let cargo_home = cargo_home.path().to_path_buf();
            let remote = remote_urls[index].clone();
            setup_jobs.push(thread::spawn(move || {
                rail(
                    &workspace,
                    &cargo_home,
                    &[
                        "rail",
                        "cache",
                        "setup",
                        "--remote",
                        &remote,
                        "--remote-mode",
                        "read-write",
                        "--root-portability",
                        "remap",
                    ],
                )
            }));
        }
        for job in setup_jobs {
            let output = job
                .join()
                .map_err(|_| anyhow::anyhow!("concurrent profile setup panicked"))??;
            assert!(output.status.success(), "concurrent profile setup failed: {output:?}");
        }

        let mut profile_ids = BTreeSet::new();
        let mut trust_domains = BTreeSet::new();
        let mut cache_roots = BTreeSet::new();
        let mut remote_authorities = BTreeSet::new();
        let mut initial_statuses = Vec::new();
        for workspace in &workspaces {
            let status = selected_profile_status(&workspace.path, cargo_home.path())?;
            assert_eq!(
                status["status"]["installation"]["selection_source"],
                "installed_profile"
            );
            assert_eq!(status["status"]["remote"]["selection_source"], "installed_profile");
            assert_eq!(status["status"]["local"]["profile_scoped"], true);
            profile_ids.insert(
                status["status"]["installation"]["profile_id"]
                    .as_str()
                    .context("profile ID")?
                    .to_string(),
            );
            trust_domains.insert(
                status["status"]["installation"]["trust_domain"]
                    .as_str()
                    .context("profile trust domain")?
                    .to_string(),
            );
            cache_roots.insert(
                status["status"]["local"]["cache"]["root"]
                    .as_str()
                    .context("profile cache root")?
                    .to_string(),
            );
            remote_authorities.insert(
                status["status"]["remote"]["authority"]
                    .as_str()
                    .context("profile remote authority")?
                    .to_string(),
            );
            initial_statuses.push(profile_authority_projection(&status));
        }
        assert_eq!(profile_ids.len(), 3);
        assert_eq!(trust_domains.len(), 3);
        assert_eq!(cache_roots.len(), 3);
        assert_eq!(remote_authorities.len(), 3);

        let profiles = rail(
            &workspaces[0].path,
            cargo_home.path(),
            &["rail", "cache", "profiles", "-f", "json"],
        )?;
        assert!(profiles.status.success(), "profile inspection failed: {profiles:?}");
        assert_eq!(json(&profiles)?["profiles"].as_array().map(Vec::len), Some(3));

        let coverage = [tempfile::tempdir()?, tempfile::tempdir()?, tempfile::tempdir()?];
        for directory in &coverage {
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        }
        let request_counts = remotes.each_ref().map(|remote| remote.request_count());
        let mut cargo_jobs = Vec::new();
        for index in 0..3 {
            let workspace = workspaces[index].path.clone();
            let cargo_home = cargo_home.path().to_path_buf();
            let coverage = coverage[index].path().to_path_buf();
            cargo_jobs.push(thread::spawn(move || {
                cargo_check_installed_remote(&workspace, &cargo_home, &coverage)
            }));
        }
        for job in cargo_jobs {
            let output = job
                .join()
                .map_err(|_| anyhow::anyhow!("concurrent Cargo profile use panicked"))??;
            assert!(output.status.success(), "profile-selected Cargo failed: {output:?}");
        }
        for index in 0..3 {
            assert!(
                remotes[index].request_count() > request_counts[index],
                "workspace profile {} did not contact only its installed remote: {:?}",
                index,
                remotes[index].requests()
            );
            assert_setup_owned_remote_transport(&coverage_events(coverage[index].path())?, "isolated profile");
            assert_eq!(
                profile_authority_projection(&selected_profile_status(&workspaces[index].path, cargo_home.path(),)?),
                initial_statuses[index],
                "ordinary Cargo replaced profile {index}"
            );
        }

        let unenrolled = TestWorkspace::new_single_crate("profile-unenrolled", "0.1.0")?;
        let before_unenrolled = remotes.each_ref().map(|remote| remote.request_count());
        let cold = cargo_check(&unenrolled.path, cargo_home.path(), None, None)?;
        assert!(
            cold.status.success(),
            "unenrolled workspace did not compile normally: {cold:?}"
        );
        assert_eq!(
            remotes.each_ref().map(|remote| remote.request_count()),
            before_unenrolled,
            "unenrolled workspace contacted an installed profile remote"
        );
        let unenrolled_status = selected_profile_status(&unenrolled.path, cargo_home.path())?;
        assert!(unenrolled_status["status"]["installation"]["profile_id"].is_null());
        assert!(unenrolled_status["status"]["remote"].is_null());

        fs::remove_dir_all(workspaces[0].path.join("target"))?;
        let clean = rail(
            &workspaces[0].path,
            cargo_home.path(),
            &["rail", "cache", "clean", "--scope", "local"],
        )?;
        assert!(clean.status.success(), "transient fixture cleanup failed: {clean:?}");
        let repair = rail(&workspaces[0].path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "transient fixture repair failed: {repair:?}");
        let transient = LoopbackS3::start()?;
        let transient_coverage = tempfile::tempdir()?;
        fs::set_permissions(transient_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let installed_before = remotes[0].request_count();
        let transient_output = cargo_check_remote(
            &workspaces[0].path,
            cargo_home.path(),
            &transient.remote_url(),
            "read-write",
            None,
            Some(transient_coverage.path()),
        )?;
        assert!(
            transient_output.status.success(),
            "transient profile override failed: {transient_output:?}"
        );
        assert!(transient.request_count() > 0);
        assert_eq!(remotes[0].request_count(), installed_before);
        assert_eq!(
            profile_authority_projection(&selected_profile_status(&workspaces[0].path, cargo_home.path(),)?),
            initial_statuses[0],
            "transient policy rewrote the installed profile"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn profile_detach_rebind_cleanup_and_global_uninstall_have_disjoint_scopes() {
    let result: Result<()> = (|| {
        let first = TestWorkspace::new_single_crate("profile-lifecycle-first", "0.1.0")?;
        let second = TestWorkspace::new_single_crate("profile-lifecycle-second", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        for workspace in [&first, &second] {
            let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
            assert!(setup.status.success(), "profile setup failed: {setup:?}");
            let seed = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
            assert!(seed.status.success(), "profile seed failed: {seed:?}");
        }
        let first_status = selected_profile_status(&first.path, cargo_home.path())?;
        let first_id = first_status["status"]["installation"]["profile_id"]
            .as_str()
            .context("first profile ID")?
            .to_string();
        let first_cache = selected_profile_cache_root(&first.path, cargo_home.path())?;
        let second_cache = selected_profile_cache_root(&second.path, cargo_home.path())?;
        let additional_root = TestWorkspace::new_single_crate("profile-lifecycle-additional-root", "0.1.0")?;
        let bind_additional = rail(
            &additional_root.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--profile", &first_id],
        )?;
        assert!(
            bind_additional.status.success(),
            "explicit additional-root binding failed: {bind_additional:?}"
        );
        let profiles = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "profiles", "-f", "json"],
        )?;
        let profiles = json(&profiles)?;
        let first_profile = profiles["profiles"]
            .as_array()
            .context("profile list")?
            .iter()
            .find(|profile| profile["profile_id"] == first_id)
            .context("first profile")?;
        assert_eq!(first_profile["roots"].as_array().map(Vec::len), Some(2));

        let clean = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "clean", "--scope", "local"],
        )?;
        assert!(clean.status.success(), "first profile cleanup failed: {clean:?}");
        assert!(!first_cache.exists());
        assert!(
            second_cache.exists(),
            "first profile cleanup removed the second profile CAS"
        );
        let repair = rail(&first.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "first profile repair failed: {repair:?}");

        let detached_check = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "detach", "--check", "-f", "json"],
        )?;
        assert_eq!(detached_check.status.code(), Some(1));
        assert!(first_cache.exists(), "detach preview removed profile data");
        let detached = rail(&first.path, cargo_home.path(), &["rail", "cache", "detach"])?;
        assert!(detached.status.success(), "profile detach failed: {detached:?}");
        assert!(first_cache.exists(), "detach removed profile data");

        let rebound = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--profile", &first_id],
        )?;
        assert!(rebound.status.success(), "explicit profile rebind failed: {rebound:?}");
        assert_eq!(
            selected_profile_status(&first.path, cargo_home.path())?["status"]["installation"]["profile_id"],
            first_id
        );

        let detached = rail(&first.path, cargo_home.path(), &["rail", "cache", "detach"])?;
        assert!(detached.status.success(), "second profile detach failed: {detached:?}");
        let detached_additional = rail(&additional_root.path, cargo_home.path(), &["rail", "cache", "detach"])?;
        assert!(
            detached_additional.status.success(),
            "additional root detach failed: {detached_additional:?}"
        );
        let drop_check = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "drop-profile", "--profile", &first_id, "--check"],
        )?;
        assert_eq!(drop_check.status.code(), Some(1));
        assert!(first_cache.exists(), "profile removal preview removed the CAS");
        let dropped = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "drop-profile", "--profile", &first_id],
        )?;
        assert!(dropped.status.success(), "profile removal failed: {dropped:?}");
        assert!(!first_cache.exists());
        assert!(second_cache.exists());

        let uninstall = rail(&second.path, cargo_home.path(), &["rail", "cache", "uninstall"])?;
        assert!(uninstall.status.success(), "global uninstall failed: {uninstall:?}");
        assert!(second_cache.exists(), "global uninstall removed a profile CAS");
        assert!(
            cargo_home.path().join("cargo-rail/cache-profiles-v1").exists(),
            "global uninstall removed the profile registry"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_profile_replace_retries_to_one_canonical_result() {
    let result: Result<()> = (|| {
        let first = TestWorkspace::new_single_crate("profile-transaction-first", "0.1.0")?;
        let second = TestWorkspace::new_single_crate("profile-transaction-second", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&first.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "initial profile setup failed: {setup:?}");

        let interrupted = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(&first.path)
            .args(["rail", "cache", "setup", "--max-size", "32MiB"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_RAIL_TEST_PROFILE_TRANSACTION_FAULT", "after_journal")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert_eq!(interrupted.status.code(), Some(2));
        assert!(
            cargo_home
                .path()
                .join("cargo-rail/cache-profiles-v1/transaction.json")
                .is_file()
        );

        let check = rail(&second.path, cargo_home.path(), &["rail", "cache", "setup", "--check"])?;
        assert_eq!(check.status.code(), Some(1));
        let retry = rail(&second.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(
            retry.status.success(),
            "another profile setup did not recover the interrupted profile transaction: {retry:?}"
        );
        assert!(
            !cargo_home
                .path()
                .join("cargo-rail/cache-profiles-v1/transaction.json")
                .exists()
        );
        let profiles = rail(
            &first.path,
            cargo_home.path(),
            &["rail", "cache", "profiles", "-f", "json"],
        )?;
        assert_eq!(json(&profiles)?["profiles"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            selected_profile_status(&first.path, cargo_home.path())?["status"]["installation"]["max_bytes"],
            32 * 1024 * 1024
        );
        let repeated = rail(&second.path, cargo_home.path(), &["rail", "cache", "setup", "--check"])?;
        assert!(repeated.status.success(), "repeated setup was not clean: {repeated:?}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn v0_25_policy_migrates_to_unbound_pre_profile_state_and_cleans_up_idempotently() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("profile-v0-25-migration", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();
        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &[
                "rail",
                "cache",
                "setup",
                "--remote",
                &remote_url,
                "--remote-mode",
                "read-write",
                "--root-portability",
                "remap",
            ],
        )?;
        assert!(setup.status.success(), "migration fixture setup failed: {setup:?}");
        let old_cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        let old_status = selected_profile_status(&workspace.path, cargo_home.path())?;
        let old_remote_authority = old_status["status"]["remote"]["authority"].clone();

        let store = cargo_home.path().join("cargo-rail/cache-profiles-v1");
        let profile_path = fs::read_dir(store.join("profiles"))?
            .next()
            .transpose()?
            .context("migration fixture profile record")?
            .path();
        let profile: serde_json::Value = serde_json::from_slice(&fs::read(profile_path)?)?;
        let receipt_path = cargo_home.path().join("cargo-rail/compiler-cache-v1/setup.json");
        let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt["version"] = serde_json::json!(3);
        receipt["cache"] = profile["cache"].clone();
        receipt["remote"] = profile["remote"].clone();
        receipt["root_portability"] = profile["root_portability"].clone();
        let mut receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
        receipt_bytes.push(b'\n');
        fs::write(&receipt_path, receipt_bytes)?;
        fs::remove_dir_all(&store)?;
        assert!(old_cache_root.exists());

        let check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--local-only", "--check", "-f", "json"],
        )?;
        assert_eq!(check.status.code(), Some(1));
        assert!(!store.exists(), "migration preview created private profile state");
        assert!(old_cache_root.exists(), "migration preview removed the old CAS");

        let migrated = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--local-only", "-f", "json"],
        )?;
        assert!(migrated.status.success(), "v0.25 migration failed: {migrated:?}");
        let migrated_status = selected_profile_status(&workspace.path, cargo_home.path())?;
        assert!(migrated_status["status"]["remote"].is_null());
        assert_eq!(
            migrated_status["status"]["installation"]["unbound_pre_profile_state"]["remote_authority"],
            old_remote_authority
        );
        let new_cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        assert_ne!(new_cache_root, old_cache_root);
        assert!(old_cache_root.exists(), "migration did not preserve the old CAS");

        let profiles = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "profiles", "-f", "json"],
        )?;
        let profiles = json(&profiles)?;
        assert_eq!(profiles["profiles"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            profiles["unbound_pre_profile_state"]["remote_authority"],
            old_remote_authority
        );
        let repeated = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check"],
        )?;
        assert!(
            repeated.status.success(),
            "migration retry did not converge: {repeated:?}"
        );

        let cleanup_check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "drop-unbound", "--check", "-f", "json"],
        )?;
        assert_eq!(cleanup_check.status.code(), Some(1));
        assert!(
            old_cache_root.exists(),
            "pre-profile cleanup preview removed the old CAS"
        );
        let cleanup = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "drop-unbound", "-f", "json"],
        )?;
        assert!(cleanup.status.success(), "pre-profile cleanup failed: {cleanup:?}");
        assert!(!old_cache_root.exists());
        assert!(new_cache_root.exists());
        let cleanup_repeated = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "drop-unbound", "--check"],
        )?;
        assert!(cleanup_repeated.status.success());
        assert!(
            selected_profile_status(&workspace.path, cargo_home.path())?["status"]["installation"]
                ["unbound_pre_profile_state"]
                .is_null()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn hostile_or_corrupt_profile_state_fails_closed_without_touching_an_external_target() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let workspace = TestWorkspace::new_single_crate("profile-hostile-state", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();
        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &[
                "rail",
                "cache",
                "setup",
                "--remote",
                &remote_url,
                "--remote-mode",
                "read-write",
            ],
        )?;
        assert!(setup.status.success(), "hostile-state fixture setup failed: {setup:?}");
        let store = cargo_home.path().join("cargo-rail/cache-profiles-v1");
        let binding = fs::read_dir(store.join("bindings"))?
            .next()
            .transpose()?
            .context("profile binding")?
            .path();
        let profile = fs::read_dir(store.join("profiles"))?
            .next()
            .transpose()?
            .context("profile record")?
            .path();
        let binding_bytes = fs::read(&binding)?;
        let profile_bytes = fs::read(&profile)?;
        let external = tempfile::tempdir()?;
        let external_target = external.path().join("outside.json");
        fs::write(&external_target, &binding_bytes)?;
        fs::set_permissions(&external_target, fs::Permissions::from_mode(0o600))?;
        let expected_external = fs::read(&external_target)?;

        let assert_cold_without_remote = |label: &str| -> Result<()> {
            let target = workspace.path.join("target");
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            let before = remote.request_count();
            let output = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
            anyhow::ensure!(
                output.status.success(),
                "{label} blocked normal compilation: {output:?}"
            );
            anyhow::ensure!(
                remote.request_count() == before,
                "{label} selected the installed remote through invalid profile state"
            );
            anyhow::ensure!(
                fs::read(&external_target)? == expected_external,
                "{label} modified the external target"
            );
            Ok(())
        };

        fs::remove_file(&binding)?;
        symlink(&external_target, &binding)?;
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(!status.status.success());
        assert_cold_without_remote("symlinked binding")?;

        fs::remove_file(&binding)?;
        fs::hard_link(&external_target, &binding)?;
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(!status.status.success());
        assert_cold_without_remote("hard-linked binding")?;

        fs::remove_file(&binding)?;
        fs::write(&binding, &binding_bytes)?;
        fs::set_permissions(&binding, fs::Permissions::from_mode(0o600))?;
        fs::write(&profile, b"{malformed}\n")?;
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(!status.status.success());
        assert_cold_without_remote("corrupt profile")?;

        fs::write(&profile, &profile_bytes)?;
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o600))?;
        fs::remove_file(&profile)?;
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(!status.status.success());
        assert_cold_without_remote("missing profile")?;

        fs::write(&profile, &profile_bytes)?;
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o600))?;
        let alias_parent = tempfile::tempdir()?;
        let unicode_alias = alias_parent.path().join("cafe\u{301}-workspace");
        symlink(&workspace.path, &unicode_alias)?;
        let alias_setup = rail(&unicode_alias, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(
            alias_setup.status.success(),
            "canonical alias setup failed: {alias_setup:?}"
        );
        let profiles = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "profiles", "-f", "json"],
        )?;
        assert_eq!(json(&profiles)?["profiles"].as_array().map(Vec::len), Some(1));

        let bindings = store.join("bindings");
        let quarantined_bindings = store.join("bindings-real");
        fs::rename(&bindings, &quarantined_bindings)?;
        symlink(external.path(), &bindings)?;
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local"],
        )?;
        assert!(!status.status.success());
        assert_cold_without_remote("replaced binding directory")?;
        assert_eq!(fs::read(&external_target)?, expected_external);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn remap_authority_restores_a_verified_l2_result_across_checkout_roots() {
    let result: Result<()> = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;

        let first = TestWorkspace::new_single_crate("portable-root-fixture", "0.1.0")?;
        let second = TestWorkspace::new_single_crate("portable-root-fixture", "0.1.0")?;
        for workspace in [&first.path, &second.path] {
            fs::create_dir_all(workspace.join(".config"))?;
            fs::write(workspace.join(".config/target-matrix.json"), "{\"target\":1}\n")?;
            fs::write(
                workspace.join("src/lib.rs"),
                "const MATRIX: &str = include_str!(\"../.config/target-matrix.json\");\nfn unused() { let _ = MATRIX; }\n",
            )?;
        }
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();
        let first_home = tempfile::tempdir()?;
        let second_home = tempfile::tempdir()?;

        let unbacked = rail(
            &first.path,
            first_home.path(),
            &["rail", "cache", "setup", "--root-portability", "remap"],
        )?;
        assert_eq!(unbacked.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&unbacked.stderr)
                .contains("root portability remapping requires an installed remote cache authority")
        );

        for (workspace, cargo_home, mode) in [
            (&first.path, first_home.path(), "read-write"),
            (&second.path, second_home.path(), "read"),
        ] {
            let setup = rail(
                workspace,
                cargo_home,
                &[
                    "rail",
                    "cache",
                    "setup",
                    "--remote",
                    &remote_url,
                    "--remote-mode",
                    mode,
                    "--root-portability",
                    "remap",
                    "-f",
                    "json",
                ],
            )?;
            assert!(setup.status.success(), "portable setup failed: {setup:?}");
            assert_eq!(json(&setup)?["root_portability"], "remap");
            let status = rail(
                workspace,
                cargo_home,
                &["rail", "cache", "status", "--scope", "local", "-f", "json"],
            )?;
            assert_eq!(json(&status)?["status"]["installation"]["root_portability"], "remap");
        }

        let first_coverage = tempfile::tempdir()?;
        let second_coverage = tempfile::tempdir()?;
        let external_targets = tempfile::tempdir()?;
        let producer_target = external_targets.path().join("producer-target");
        let consumer_target = external_targets.path().join("consumer-target");
        let mutation_target = external_targets.path().join("mutation-target");
        assert!(!producer_target.exists() && !consumer_target.exists() && !mutation_target.exists());
        #[cfg(unix)]
        {
            fs::set_permissions(first_coverage.path(), fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(second_coverage.path(), fs::Permissions::from_mode(0o700))?;
        }
        let seeded = cargo_check_installed_remote_in_target(
            &first.path,
            first_home.path(),
            first_coverage.path(),
            &producer_target,
        )?;
        assert!(seeded.status.success(), "portable seed failed: {seeded:?}");
        let seed_events = coverage_events(first_coverage.path())?;
        let writes_before_consumer = remote.requests().iter().filter(|(method, _)| method == "PUT").count();
        let restored = cargo_check_installed_remote_in_target(
            &second.path,
            second_home.path(),
            second_coverage.path(),
            &consumer_target,
        )?;
        assert!(restored.status.success(), "portable restore failed: {restored:?}");
        let events = coverage_events(second_coverage.path())?;
        assert!(
            events.iter().any(|event| {
                event["status"] == "hit"
                    && event["reason"].as_str().is_some_and(|reason| {
                        reason.starts_with("verified_remote_result")
                            && reason.contains("root_portability_remap_eligible")
                    })
            }),
            "second checkout did not report a verified L2 hit: producer_stderr={}, consumer_stderr={}, seed={seed_events:?}, restored={events:?}, requests={:?}",
            String::from_utf8_lossy(&seeded.stderr),
            String::from_utf8_lossy(&restored.stderr),
            remote.requests()
        );
        assert_eq!(
            remote.requests().iter().filter(|(method, _)| method == "PUT").count(),
            writes_before_consumer,
            "read-only second-checkout restore wrote a remote object"
        );
        let seeded_diagnostics = String::from_utf8_lossy(&seeded.stderr);
        let restored_diagnostics = String::from_utf8_lossy(&restored.stderr);
        assert!(seeded_diagnostics.contains("function `unused` is never used"));
        assert!(restored_diagnostics.contains("function `unused` is never used"));
        assert!(!restored_diagnostics.contains(first.path.to_string_lossy().as_ref()));

        let first_root = first.path.to_string_lossy().into_owned().into_bytes();
        let mut pending = vec![consumer_target];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    let bytes = fs::read(entry.path())?;
                    assert!(
                        !bytes.windows(first_root.len()).any(|window| window == first_root),
                        "restored output leaked the producer checkout root: {}",
                        entry.path().display()
                    );
                }
            }
        }

        fs::write(second.path.join(".config/target-matrix.json"), "{\"target\":2}\n")?;
        let mutated_coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        fs::set_permissions(mutated_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let mutated = cargo_check_installed_remote_in_target(
            &second.path,
            second_home.path(),
            mutated_coverage.path(),
            &mutation_target,
        )?;
        assert!(
            mutated.status.success(),
            "same-size dynamic-input rebuild failed: {mutated:?}"
        );
        let mutated_events = coverage_events(mutated_coverage.path())?;
        assert!(
            mutated_events.iter().any(|event| event["status"] == "miss")
                && mutated_events.iter().all(|event| event["status"] != "hit"),
            "same-size selected-input mutation did not produce a clean miss: stderr={}, events={mutated_events:?}",
            String::from_utf8_lossy(&mutated.stderr)
        );
        assert_eq!(
            remote.requests().iter().filter(|(method, _)| method == "PUT").count(),
            writes_before_consumer,
            "read-only same-size miss wrote a remote object"
        );
        assert_eq!(
            fs::read_dir(external_targets.path())?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|entry| entry.file_name())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                std::ffi::OsString::from("consumer-target"),
                std::ffi::OsString::from("mutation-target"),
                std::ffi::OsString::from("producer-target"),
            ]),
            "remote qualification wrote outside its exact external target roots"
        );

        let ambiguous_coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        fs::set_permissions(ambiguous_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let ambiguous = cargo_check_installed_remote_with_rustflags(
            &first.path,
            first_home.path(),
            ambiguous_coverage.path(),
            Some("--remap-path-prefix=/tmp=/ambiguous"),
        )?;
        assert!(
            ambiguous.status.success(),
            "ambiguous remap fallback failed: {ambiguous:?}"
        );
        let ambiguous_events = coverage_events(ambiguous_coverage.path())?;
        assert!(
            ambiguous_events.iter().any(|event| {
                event["status"] == "bypassed" && event["reason"] == "remapped_path_observation_unavailable"
            }),
            "an existing path remap did not fail closed with its named reason: {ambiguous_events:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

fn assert_setup_owned_remote_transport(events: &[serde_json::Value], phase: &str) {
    let coordinated = events
        .iter()
        .filter_map(|event| event["remote_coordinator_requests"].as_u64())
        .sum::<u64>()
        > 0;
    #[cfg(not(windows))]
    assert!(
        coordinated,
        "setup-owned remote {phase} bypassed coordination: {events:?}"
    );
    #[cfg(windows)]
    {
        let explicit_direct_fallback = events.iter().any(|event| {
            event["remote_request_attempts"]
                .as_u64()
                .is_some_and(|attempts| attempts > 0)
                && event["remote_error"].as_str().is_some_and(|error| !error.is_empty())
        });
        assert!(
            coordinated || explicit_direct_fallback,
            "setup-owned remote {phase} used neither coordination nor an evidenced direct fallback: {events:?}"
        );
    }
}

#[test]
fn cache_normalize_is_network_free_canonical_and_rejects_credentials() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("remote-normalize", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let normalized = rail(
            &workspace.path,
            cargo_home.path(),
            &[
                "rail",
                "cache",
                "normalize",
                "s3://Rail-Cache//team/%61?region=us-east-1&owner=123456789012",
                "--mode",
                "read",
                "-f",
                "json",
            ],
        )?;
        assert!(normalized.status.success(), "normalization failed: {normalized:?}");
        let normalized = json(&normalized)?;
        assert_eq!(
            normalized["normalized_url"],
            "s3://rail-cache/team/a?owner=123456789012&region=us-east-1"
        );
        assert_eq!(normalized["remote"]["mode"], "read");

        let rejected = rail(
            &workspace.path,
            cargo_home.path(),
            &[
                "rail",
                "cache",
                "normalize",
                "s3://user:top-secret@rail-cache/team?owner=123456789012&region=us-east-1",
            ],
        )?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&rejected.stderr).contains("top-secret"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cache_probe_authenticates_and_reports_protocol_marker_state() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("remote-probe", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();

        let initialized = remote_probe(&workspace.path, cargo_home.path(), &remote_url, "read-write")?;
        assert!(
            initialized.status.success(),
            "marker initialization failed: {initialized:?}"
        );
        let initialized_json = json(&initialized)?;
        assert_eq!(initialized_json["result"], "ready");
        assert_eq!(initialized_json["ready"], true);
        assert_eq!(initialized_json["protocol_marker"], "initialized");
        assert_eq!(initialized_json["remote"]["provider"], "s3-compatible");
        assert_eq!(initialized_json["remote"]["mode"], "read-write");
        assert!(
            remote
                .requests()
                .iter()
                .any(|(method, path)| method == "PUT" && path.ends_with("/protocol")),
            "probe did not initialize the protocol marker"
        );

        let puts_before_existing = remote.requests().iter().filter(|(method, _)| method == "PUT").count();
        let existing = remote_probe(&workspace.path, cargo_home.path(), &remote_url, "read")?;
        assert!(existing.status.success(), "existing marker probe failed: {existing:?}");
        assert_eq!(json(&existing)?["protocol_marker"], "existing");
        assert_eq!(
            remote.requests().iter().filter(|(method, _)| method == "PUT").count(),
            puts_before_existing,
            "read-only probe wrote to the object store"
        );

        let missing_url = remote_url.replace("/team?", "/missing?");
        let missing = remote_probe(&workspace.path, cargo_home.path(), &missing_url, "read")?;
        assert_eq!(missing.status.code(), Some(2));
        let missing_json = json(&missing)?;
        assert_eq!(missing_json["result"], "probe_failed");
        assert_eq!(missing_json["ready"], false);
        assert_eq!(missing_json["failure"]["kind"], "configuration_failure");

        #[cfg(unix)]
        {
            remote.set_available(false);
            let unavailable = remote_probe(&workspace.path, cargo_home.path(), &remote_url, "read")?;
            remote.set_available(true);
            assert_eq!(unavailable.status.code(), Some(2));
            let unavailable_json = json(&unavailable)?;
            assert_eq!(unavailable_json["failure"]["kind"], "transport_failure");
            assert_eq!(unavailable_json["failure"]["cause"], "http");
            assert_eq!(
                unavailable_json["failure"]["retry"],
                "retry the probe after the remote service recovers"
            );
        }

        assert!(remote.corrupt_protocol_marker());
        let incompatible = remote_probe(&workspace.path, cargo_home.path(), &remote_url, "read")?;
        assert_eq!(incompatible.status.code(), Some(2));
        assert_eq!(json(&incompatible)?["failure"]["kind"], "integrity_failure");
        let output = String::from_utf8_lossy(&incompatible.stdout);
        assert!(!output.contains(&remote_url));
        assert!(!output.contains("fixture-access-key"));
        assert!(!output.contains("fixture-secret-key"));
        assert!(!output.contains("fixture-session-token"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn direct_s3_remote_is_l2_only_and_falls_back_cold_on_corruption_or_outage() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-remote", "0.1.0")?;
        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();
        let rustc = workspace.path.join("rustc-remote-proof");
        fs::write(
            &rustc,
            "#!/bin/sh\nprintf 'url=%s access=%s secret=%s token=%s config=%s args=%s\\n' \"${CARGO_RAIL_CACHE_REMOTE-unset}\" \"${AWS_ACCESS_KEY_ID-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"${AWS_SESSION_TOKEN-unset}\" \"${AWS_CONFIG_FILE-unset}\" \"$*\" >> \"$REMOTE_ENV_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&rustc, fs::Permissions::from_mode(0o700))?;

        let credential_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, credential_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "credential cache setup failed: {setup:?}");
        let credential_probe = cargo_check_remote(
            &workspace.path,
            credential_home.path(),
            &remote_url,
            "read-write",
            Some(&rustc),
            None,
        )?;
        assert!(
            credential_probe.status.success(),
            "credential scrub probe failed: {credential_probe:?}"
        );
        let compiler_environments = fs::read_to_string(workspace.path.join("remote-compiler-environment.log"))?;
        let controlled_compilers = compiler_environments
            .lines()
            .filter(|line| line.contains("--crate-name transparent_remote"))
            .collect::<Vec<_>>();
        assert!(
            !controlled_compilers.is_empty()
                && controlled_compilers.iter().all(|line| {
                    line.starts_with("url=unset access=unset secret=unset token=unset config=unset args=")
                }),
            "remote authority entered a Cargo-Rail-controlled compiler subprocess: {compiler_environments:?}"
        );
        fs::remove_dir_all(workspace.path.join("target"))?;

        let seed_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, seed_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "seed cache setup failed: {setup:?}");
        let seed_coverage = tempfile::tempdir()?;
        fs::set_permissions(seed_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let seed = cargo_check_remote(
            &workspace.path,
            seed_home.path(),
            &remote_url,
            "read-write",
            None,
            Some(seed_coverage.path()),
        )?;
        assert!(seed.status.success(), "remote seed compilation failed: {seed:?}");
        let requests = remote.requests();
        assert!(
            requests
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
            "remote seed did not publish a compressed entry: {requests:?}"
        );

        remote.set_available(false);
        fs::remove_dir_all(workspace.path.join("target"))?;
        let before_l1_hit = remote.request_count();
        let l1_coverage = tempfile::tempdir()?;
        fs::set_permissions(l1_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let l1_hit = cargo_check_remote(
            &workspace.path,
            seed_home.path(),
            &remote_url,
            "read-write",
            None,
            Some(l1_coverage.path()),
        )?;
        assert!(
            l1_hit.status.success(),
            "L1 reuse failed during remote outage: {l1_hit:?}"
        );
        assert_eq!(
            remote.request_count(),
            before_l1_hit,
            "a verified L1 hit performed an L2 request"
        );
        let l1_events = coverage_events(l1_coverage.path())?;
        assert!(
            !l1_events.is_empty()
                && l1_events
                    .iter()
                    .all(|event| event["remote_request_attempts"] == 0 && event["remote_coordinator_requests"] == 0),
            "a verified L1 hit reported an L2 request: {l1_events:?}"
        );

        remote.set_available(true);
        let import_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, import_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "import cache setup failed: {setup:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let import_coverage = tempfile::tempdir()?;
        fs::set_permissions(import_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let writes_before_import = remote.requests().iter().filter(|(method, _)| method == "PUT").count();
        let requests_before_import = remote.request_count();
        let imported = cargo_check_remote(
            &workspace.path,
            import_home.path(),
            &remote_url,
            "read",
            None,
            Some(import_coverage.path()),
        )?;
        assert!(imported.status.success(), "remote import failed: {imported:?}");
        let imported_events = coverage_events(import_coverage.path())?;
        assert!(
            imported_events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_remote_result"),
            "empty L1 did not import and verify the remote result: {imported_events:?}"
        );
        assert_eq!(
            remote.requests().iter().filter(|(method, _)| method == "PUT").count(),
            writes_before_import,
            "read-only remote import performed a write"
        );
        let import_request_attempts = imported_events
            .iter()
            .filter_map(|event| event["remote_request_attempts"].as_u64())
            .sum::<u64>();
        let coordinator_requests = imported_events
            .iter()
            .filter_map(|event| event["remote_coordinator_requests"].as_u64())
            .sum::<u64>();
        let coordinated_events = imported_events
            .iter()
            .filter(|event| {
                event["remote_coordinator_requests"]
                    .as_u64()
                    .is_some_and(|requests| requests > 0)
            })
            .count();
        let observed_remote_requests = u64::try_from(remote.request_count().saturating_sub(requests_before_import))?;
        assert!(
            observed_remote_requests > 0 && import_request_attempts >= observed_remote_requests,
            "coordinated import under-reported fixture-observed S3 requests: attempts={import_request_attempts}, \
     observed={observed_remote_requests}, events={imported_events:?}"
        );
        assert!(
            coordinator_requests > 0 && import_request_attempts > 0,
            "coordinated import performed no remote work: {imported_events:?}"
        );
        assert_eq!(
            coordinator_requests,
            u64::try_from(coordinated_events)?,
            "coordinated import performed control-plane requests beyond its cache lookup: {imported_events:?}"
        );
        assert!(
            imported_events
                .iter()
                .filter_map(|event| event["remote_payload_bytes_read"].as_u64())
                .sum::<u64>()
                > 0,
            "remote import reported no downloaded payload bytes: {imported_events:?}"
        );
        assert!(
            imported_events
                .iter()
                .filter_map(|event| event["remote_service_elapsed_ns"].as_u64())
                .sum::<u64>()
                > 0,
            "remote import reported no provider/coordinator service time: {imported_events:?}"
        );
        let remote_hits = imported_events
            .iter()
            .filter(|event| event["status"] == "hit" && event["reason"] == "verified_remote_result")
            .collect::<Vec<_>>();
        assert!(
            remote_hits.iter().all(|event| {
                event["timing"]["total"]["count"].as_u64() == Some(1)
                    && event["timing"]["lookup"]["count"].as_u64() == Some(1)
                    && event["timing"]["decode"]["count"].as_u64() == Some(1)
                    && event["timing"]["validation"]["count"].as_u64() == Some(1)
                    && event["timing"]["l1_admission"]["count"].as_u64() == Some(1)
                    && event["timing"]["output_restore"]["count"].as_u64() == Some(1)
            }),
            "remote-hit phase accounting is incomplete: {remote_hits:?}"
        );
        assert!(
            remote_hits
                .iter()
                .filter_map(|event| event["durability"]["l1_file_sync"]["count"].as_u64())
                .sum::<u64>()
                > 0,
            "remote-hit L1 durability accounting is empty: {remote_hits:?}"
        );
        assert!(
            imported_events
                .iter()
                .all(|event| event["remote_payload_bytes_written"] == 0),
            "read-only remote import reported uploaded payload bytes: {imported_events:?}"
        );

        remote.set_available(false);
        fs::remove_dir_all(workspace.path.join("target"))?;
        let requests_before_packed_hit = remote.request_count();
        let packed_coverage = tempfile::tempdir()?;
        fs::set_permissions(packed_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let packed_hit = cargo_check_remote(
            &workspace.path,
            import_home.path(),
            &remote_url,
            "read",
            None,
            Some(packed_coverage.path()),
        )?;
        assert!(
            packed_hit.status.success(),
            "packed L1 reuse failed during remote outage: {packed_hit:?}"
        );
        assert_eq!(
            remote.request_count(),
            requests_before_packed_hit,
            "a packed L1 hit performed an L2 request"
        );
        let packed_events = coverage_events(packed_coverage.path())?;
        assert!(
            packed_events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_local_result")
                && packed_events
                    .iter()
                    .all(|event| event["remote_request_attempts"] == 0 && event["remote_coordinator_requests"] == 0),
            "the imported packed authority did not serve an offline L1 hit: {packed_events:?}"
        );

        remote.set_available(true);
        assert!(remote.corrupt_result(), "fixture had no remote result to corrupt");
        let corrupt_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, corrupt_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "corrupt cache setup failed: {setup:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let corrupt_coverage = tempfile::tempdir()?;
        fs::set_permissions(corrupt_coverage.path(), fs::Permissions::from_mode(0o700))?;
        let corrupt = cargo_check_remote(
            &workspace.path,
            corrupt_home.path(),
            &remote_url,
            "read-write",
            None,
            Some(corrupt_coverage.path()),
        )?;
        assert!(
            corrupt.status.success(),
            "remote corruption blocked the cold compilation: {corrupt:?}"
        );
        let corrupt_events = coverage_events(corrupt_coverage.path())?;
        assert!(
            corrupt_events.iter().any(|event| event["status"] == "miss"
                && event["reason"].as_str().is_some_and(|reason| {
                    reason.starts_with("remote_entry_rejected;") && reason.ends_with("remote_publication_failed")
                })),
            "corrupt remote data was not rejected before cold fallback: {corrupt_events:?}"
        );

        remote.set_available(false);
        let outage_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, outage_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "outage cache setup failed: {setup:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let outage = cargo_check_remote(
            &workspace.path,
            outage_home.path(),
            &remote_url,
            "read-write",
            None,
            None,
        )?;
        assert!(
            outage.status.success(),
            "remote outage blocked the cold compilation: {outage:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn direct_cargo_reuses_verified_outputs_and_off_never_touches_l1() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-hit", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(cold.status.success(), "cold compilation failed: {cold:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;

        let real_rustc = Path::new("rustc");
        let shim = workspace.path.join("rustc-hit-proof");
        fs::write(
            &shim,
            "#!/bin/sh\nif [ -n \"$CACHE_ENV_LOG\" ]; then printf '%s\\n' \"${CARGO_RAIL_CACHE-unset}\" >> \"$CACHE_ENV_LOG\"; fi\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"transparent_hit\" ]; then exit 91; fi\ndone\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
        let hit = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("RUSTC", &shim)
            .env("REAL_RUSTC", real_rustc)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            hit.status.success(),
            "verified hit executed the rejecting rustc shim: {hit:?}"
        );

        let observed = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        let observed = json(&observed)?;
        let installed_wrapper = PathBuf::from(
            observed["status"]["installation"]["wrapper_path"]
                .as_str()
                .context("installed wrapper path")?,
        );
        assert!(
            observed["status"]["installation"]["usage"]["misses"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
        assert!(
            observed["status"]["installation"]["usage"]["hits"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );

        fs::remove_dir_all(workspace.path.join("target"))?;
        let cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        let before = directory_snapshot(&cache_root)?;
        #[cfg(not(windows))]
        let installed_worker = installed_wrapper.with_file_name("cargo-rail-native-rustc-worker");
        #[cfg(windows)]
        let installed_worker = installed_wrapper.with_file_name("cargo-rail-native-rustc-worker.exe");
        fs::remove_file(installed_worker)?;
        let off = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "off")
            .env("CACHE_ENV_LOG", workspace.path.join("cache-env.log"))
            .env("RUSTC", &shim)
            .env("REAL_RUSTC", real_rustc)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            !off.status.success(),
            "cache opt-out did not execute the original compiler"
        );
        assert_eq!(
            directory_snapshot(&cache_root)?,
            before,
            "cache opt-out touched L1 state"
        );
        let observed_environment = fs::read_to_string(workspace.path.join("cache-env.log"))?;
        assert!(
            observed_environment.lines().all(|value| value == "off"),
            "cache opt-out changed the compiler environment: {observed_environment:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn analysis_missing_binding_executes_despite_an_ordinary_native_result() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("fact-cache-guard", "0.1.0")?;
        fs::create_dir_all(workspace.path.join("helper/src"))?;
        fs::write(
            workspace.path.join("helper/Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::write(workspace.path.join("helper/src/lib.rs"), "pub fn helper() {}\n")?;
        let manifest = workspace.path.join("Cargo.toml");
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)?
                .replace("[dependencies]\n", "[dependencies]\nhelper = { path = \"helper\" }\n"),
        )?;
        let lock = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["generate-lockfile"])
            .output()?;
        assert!(lock.status.success(), "lockfile generation failed: {lock:?}");
        workspace.commit("Add unused path dependency")?;

        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "cache setup failed: {setup:?}");
        let cold = Command::new("cargo")
            .current_dir(&workspace.path)
            .args([
                "check",
                "--locked",
                "--all-targets",
                "--message-format=json",
                "--package",
                "fact-cache-guard",
            ])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(cold.status.success(), "ordinary cache seed failed: {cold:?}");
        let native_actions = selected_profile_cache_root(&workspace.path, cargo_home.path())?.join("native-actions-v2");
        assert!(
            fs::read_dir(&native_actions)?.next().transpose()?.is_some(),
            "ordinary cargo check did not publish a native result"
        );
        let metadata = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["metadata", "--format-version=1", "--no-deps"])
            .env("CARGO_HOME", cargo_home.path())
            .output()?;
        assert!(metadata.status.success(), "target discovery failed: {metadata:?}");
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
        let target_directory = PathBuf::from(
            metadata["target_directory"]
                .as_str()
                .context("Cargo metadata target directory")?,
        );
        fs::remove_dir_all(&target_directory)?;
        assert!(!target_directory.exists(), "Cargo target directory survived removal");

        let rustc_probe = tempfile::tempdir()?;
        let rustc_log = rustc_probe.path().join("fact-rustc.log");
        let rustc_shim = rustc_probe.path().join("fact-rustc-shim");
        fs::write(
            &rustc_shim,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CARGO_RAIL_TEST_RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&rustc_shim, fs::Permissions::from_mode(0o700))?;
        let analysis = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(&workspace.path)
            .args(["rail", "unify", "--check"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("RUSTC", &rustc_shim)
            .env("REAL_RUSTC", "rustc")
            .env("CARGO_RAIL_TEST_RUSTC_LOG", &rustc_log)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        let rustc_invocations =
            fs::read_to_string(&rustc_log).unwrap_or_else(|error| format!("<unavailable: {error}>"));
        assert_eq!(
            analysis.status.code(),
            Some(1),
            "unused dependency analysis lost required compiler facts\nstdout:\n{}\nstderr:\n{}\nrustc:\n{}",
            String::from_utf8_lossy(&analysis.stdout),
            String::from_utf8_lossy(&analysis.stderr),
            rustc_invocations
        );
        assert!(
            String::from_utf8_lossy(&analysis.stdout).contains("Dependencies: helper"),
            "unused path dependency was not planned\nstdout:\n{}",
            String::from_utf8_lossy(&analysis.stdout)
        );
        assert!(
            rustc_invocations.lines().any(|invocation| {
                invocation.contains("--crate-name fact_cache_guard") && invocation.contains("unused-crate-dependencies")
            }),
            "fact-required workspace compilation was restored from the ordinary native result:\n{rustc_invocations}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn compiler_analysis_reuses_native_result_only_after_an_exact_binding() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("bound-analysis-cache", "0.1.0")?;
        fs::create_dir_all(workspace.path.join("helper/src"))?;
        fs::write(
            workspace.path.join("helper/Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::write(workspace.path.join("helper/src/lib.rs"), "pub fn helper() {}\n")?;
        let manifest = workspace.path.join("Cargo.toml");
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)?
                .replace("[dependencies]\n", "[dependencies]\nhelper = { path = \"helper\" }\n"),
        )?;
        fs::write(workspace.path.join("build.rs"), "fn main() {}\n")?;
        let lock = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["generate-lockfile"])
            .output()?;
        assert!(lock.status.success(), "lockfile generation failed: {lock:?}");
        workspace.commit("Add bound analysis fixture")?;

        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "cache setup failed: {setup:?}");

        let probe = tempfile::tempdir()?;
        let rustc_log = probe.path().join("rustc.log");
        let rustc_shim = probe.path().join("rustc-shim");
        fs::write(
            &rustc_shim,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CARGO_RAIL_TEST_RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&rustc_shim, fs::Permissions::from_mode(0o700))?;
        let run = || -> Result<std::process::Output> {
            Ok(Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
                .current_dir(&workspace.path)
                .args(["rail", "unify", "--check"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("RUSTC", &rustc_shim)
                .env("REAL_RUSTC", "rustc")
                .env("CARGO_RAIL_TEST_RUSTC_LOG", &rustc_log)
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?)
        };

        let cold = run()?;
        assert_eq!(cold.status.code(), Some(1), "cold analysis failed: {cold:?}");
        let cold_log = fs::read_to_string(&rustc_log)?;
        assert!(
            cold_log
                .lines()
                .any(|line| line.contains("--crate-name bound_analysis_cache")),
            "cold analysis did not execute the selected compiler:\n{cold_log}"
        );
        fs::write(&rustc_log, "")?;

        let warm = run()?;
        assert_eq!(warm.status.code(), Some(1), "warm analysis failed: {warm:?}");
        let cache_status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert!(
            String::from_utf8_lossy(&warm.stdout).contains("Dependencies: helper"),
            "warm analysis lost exact diagnostic evidence: {warm:?}"
        );
        let warm_log = fs::read_to_string(&rustc_log)?;
        assert!(
            !warm_log
                .lines()
                .any(|line| line.contains("--crate-name bound_analysis_cache")),
            "warm analysis executed rustc despite its exact binding:\n{warm_log}\ncold stdout:\n{}\ncold stderr:\n{}\nwarm stdout:\n{}\nwarm stderr:\n{}\ncache status stdout:\n{}\ncache status stderr:\n{}",
            String::from_utf8_lossy(&cold.stdout),
            String::from_utf8_lossy(&cold.stderr),
            String::from_utf8_lossy(&warm.stdout),
            String::from_utf8_lossy(&warm.stderr),
            String::from_utf8_lossy(&cache_status.stdout),
            String::from_utf8_lossy(&cache_status.stderr),
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
#[ignore = "blocked by the known process-tree descendant ownership failure tracked in docs/tasks/improve.md"]
fn remote_analysis_imports_evidence_before_reusing_the_native_result() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("remote-bound-analysis", "0.1.0")?;
        fs::create_dir_all(workspace.path.join("helper/src"))?;
        fs::write(
            workspace.path.join("helper/Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        fs::write(workspace.path.join("helper/src/lib.rs"), "pub fn helper() {}\n")?;
        let manifest = workspace.path.join("Cargo.toml");
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)?
                .replace("[dependencies]\n", "[dependencies]\nhelper = { path = \"helper\" }\n"),
        )?;
        let lock = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["generate-lockfile"])
            .output()?;
        assert!(lock.status.success(), "lockfile generation failed: {lock:?}");
        workspace.commit("Add remote analysis fixture")?;

        let remote = LoopbackS3::start()?;
        let remote_url = remote.remote_url();
        let seed_home = tempfile::tempdir()?;
        let seed_setup = rail(&workspace.path, seed_home.path(), &["rail", "cache", "setup"])?;
        assert!(seed_setup.status.success(), "seed cache setup failed: {seed_setup:?}");

        let probe = tempfile::tempdir()?;
        let rustc_log = probe.path().join("rustc.log");
        let rustc_shim = probe.path().join("rustc-shim");
        fs::write(
            &rustc_shim,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CARGO_RAIL_TEST_RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&rustc_shim, fs::Permissions::from_mode(0o700))?;
        let run = |cargo_home: &Path, mode: &str| -> Result<Output> {
            Ok(Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
                .current_dir(&workspace.path)
                .args(["rail", "unify", "--check"])
                .env("CARGO_HOME", cargo_home)
                .env("CARGO_INCREMENTAL", "0")
                .env("RUSTC", &rustc_shim)
                .env("REAL_RUSTC", "rustc")
                .env("CARGO_RAIL_TEST_RUSTC_LOG", &rustc_log)
                .env("AWS_ACCESS_KEY_ID", "fixture-access-key")
                .env("AWS_SECRET_ACCESS_KEY", "fixture-secret-key")
                .env("AWS_SESSION_TOKEN", "fixture-session-token")
                .env("AWS_EC2_METADATA_DISABLED", "true")
                .env("AWS_CONFIG_FILE", workspace.path.join("missing-aws-config"))
                .env(
                    "AWS_SHARED_CREDENTIALS_FILE",
                    workspace.path.join("missing-aws-credentials"),
                )
                .env("CARGO_RAIL_CACHE_REMOTE", &remote_url)
                .env("CARGO_RAIL_CACHE_MODE", mode)
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("AWS_ENDPOINT_URL")
                .env_remove("AWS_ENDPOINT_URL_S3")
                .env_remove("AWS_PROFILE")
                .env_remove("AWS_DEFAULT_PROFILE")
                .output()?)
        };

        let seed = run(seed_home.path(), "read-write")?;
        assert_eq!(seed.status.code(), Some(1), "remote analysis seed failed: {seed:?}");
        let seed_requests = remote.requests();
        assert!(
            seed_requests
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/evidence-v1/objects/")),
            "analysis seed did not publish evidence objects: {seed_requests:?}"
        );
        assert!(
            seed_requests
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/evidence-v1/candidates/")),
            "analysis seed did not publish evidence candidate indexes: {seed_requests:?}"
        );

        fs::remove_dir_all(workspace.path.join("target"))?;
        fs::write(&rustc_log, "")?;
        let import_home = tempfile::tempdir()?;
        let import_setup = rail(&workspace.path, import_home.path(), &["rail", "cache", "setup"])?;
        assert!(
            import_setup.status.success(),
            "remote import setup failed: {import_setup:?}"
        );
        let requests_before_import = remote.request_count();
        let imported = run(import_home.path(), "read")?;
        assert_eq!(
            imported.status.code(),
            Some(1),
            "remote analysis import failed: {imported:?}"
        );
        assert!(
            String::from_utf8_lossy(&imported.stdout).contains("Dependencies: helper"),
            "remote analysis import lost exact fact evidence: {imported:?}"
        );
        let imported_rustc = fs::read_to_string(&rustc_log)?;
        assert!(
            !imported_rustc
                .lines()
                .any(|line| line.contains("--crate-name remote_bound_analysis")),
            "remote analysis reran the selected compiler despite its imported binding:\n{imported_rustc}"
        );
        let import_requests = remote.requests();
        assert!(
            import_requests[requests_before_import..]
                .iter()
                .any(|(method, path)| method == "GET" && path.contains("/evidence-v1/candidates/")),
            "analysis reuse did not query remote evidence candidates: {import_requests:?}"
        );
        assert!(
            import_requests[requests_before_import..]
                .iter()
                .any(|(method, path)| method == "GET" && path.contains("/evidence-v1/objects/")),
            "analysis reuse did not import remote evidence objects: {import_requests:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unsupported_shapes_bypass_before_acquisition_while_proc_macro_producers_remain_cacheable() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-early-bypass", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let installation = selected_profile_state_root(&workspace.path, cargo_home.path())?;
        let cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        let before = directory_snapshot(&cache_root)?;

        let incremental = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env_remove("CARGO_INCREMENTAL")
            .env_remove("RUSTC_FORCE_INCREMENTAL")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            incremental.status.success(),
            "incremental bypass failed: {incremental:?}"
        );
        assert!(!installation.join("session.json").exists());
        assert!(!installation.join("usage-v1.log").exists());
        assert!(installation.join("early-bypass-v1.log").is_file());
        assert_eq!(
            directory_snapshot(&cache_root)?,
            before,
            "incremental bypass touched L1"
        );
        let early_status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        let early_usage = &json(&early_status)?["status"]["installation"]["usage"];
        assert!(early_usage["early_bypasses"].as_u64().unwrap_or_default() >= 1);
        assert!(
            early_usage["early_bypass_reasons"]["incremental_work_product_observation_unavailable"]
                .as_u64()
                .unwrap_or_default()
                >= 1,
            "incremental bypass class was not observable: {early_usage}"
        );

        let clippy = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["clippy", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env_remove("CARGO_INCREMENTAL")
            .env_remove("RUSTC_FORCE_INCREMENTAL")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(clippy.status.success(), "clippy bypass failed: {clippy:?}");
        assert!(!installation.join("session.json").exists());
        assert!(!installation.join("usage-v1.log").exists());
        assert!(installation.join("early-bypass-v1.log").is_file());
        assert_eq!(directory_snapshot(&cache_root)?, before, "clippy bypass touched L1");

        fs::create_dir_all(workspace.path.join("fixture-macros/src"))?;
        fs::write(
            workspace.path.join("fixture-macros/Cargo.toml"),
            r#"[package]
name = "fixture-macros"
version = "0.1.0"
edition = "2024"

[lib]
proc-macro = true
"#,
        )?;
        fs::write(
            workspace.path.join("fixture-macros/src/lib.rs"),
            r#"extern crate proc_macro;

use proc_macro::TokenStream;

#[proc_macro_derive(Fixture)]
pub fn derive_fixture(_: TokenStream) -> TokenStream {
  TokenStream::new()
}
"#,
        )?;
        fs::write(
            workspace.path.join("Cargo.toml"),
            r#"[package]
name = "transparent-early-bypass"
version = "0.1.0"
edition = "2024"

[dependencies]
fixture-macros = { path = "fixture-macros" }

[workspace]
members = ["fixture-macros"]
resolver = "3"
"#,
        )?;
        fs::write(
            workspace.path.join("src/lib.rs"),
            "#[derive(fixture_macros::Fixture)]\npub struct Fixture;\n",
        )?;
        let coverage = tempfile::tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
        }
        let coverage_path = fs::canonicalize(coverage.path())?;
        let proc_macro = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--workspace", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage_path)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(proc_macro.status.success(), "proc-macro bypass failed: {proc_macro:?}");
        assert!(installation.join("session.json").is_file());
        assert!(installation.join("usage-v1.log").is_file());
        assert_ne!(
            directory_snapshot(&cache_root)?,
            before,
            "graduated proc-macro producer did not touch L1"
        );

        let mut producer_cached = false;
        let mut consumer_bypassed_before_acquisition = false;
        let mut event_summary = Vec::new();
        for entry in fs::read_dir(&coverage_path)? {
            let event: serde_json::Value = serde_json::from_slice(&fs::read(entry?.path())?)?;
            let arguments = event["arguments"].as_array().context("coverage arguments")?;
            let crate_name = arguments.windows(2).find_map(|pair| {
                (pair[0].as_str() == Some("--crate-name"))
                    .then(|| pair[1].as_str())
                    .flatten()
            });
            event_summary.push((
                crate_name.map(str::to_string),
                event["status"].as_str().map(str::to_string),
                event["reason"].as_str().map(str::to_string),
                event["action_key"].as_str().map(str::to_string),
            ));
            if crate_name == Some("fixture_macros") && matches!(event["status"].as_str(), Some("hit" | "miss")) {
                producer_cached |= event["action_key"].as_str().is_some();
            }
            let consumes_fixture_macro = crate_name == Some("transparent_early_bypass")
                && arguments.iter().any(|argument| {
                    argument
                        .as_str()
                        .is_some_and(|argument| argument.starts_with("fixture_macros="))
                });
            if consumes_fixture_macro {
                assert_eq!(event["status"], "bypassed");
                assert_eq!(event["reason"], "dynamic_dependency_execution_observation_unavailable");
                assert!(event.get("action_key").is_none());
                assert_eq!(event["bytes_hashed"], 0);
                assert_eq!(event["cache_bytes_read"], 0);
                consumer_bypassed_before_acquisition = true;
            }
        }
        assert!(
            producer_cached,
            "proc-macro producer did not enter verified L1: {event_summary:?}"
        );
        assert!(
            consumer_bypassed_before_acquisition,
            "native proc-macro consumer did not retain its acquisition-free bypass: {event_summary:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn custom_target_and_deterministic_flags_reuse_without_runtime_residue() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-custom-target", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");

        let coverage = tempfile::tempdir()?;
        fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
        let coverage_path = fs::canonicalize(coverage.path())?;
        let first_target_parent = tempfile::tempdir()?;
        let first_target = first_target_parent.path().join("producer-target");
        assert!(!first_target.exists(), "producer target existed before Cargo started");
        let first_runtime = tempfile::tempdir()?;
        let seed = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_TARGET_DIR", &first_target)
            .env("TMPDIR", first_runtime.path())
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage_path)
            .env(
                "RUSTFLAGS",
                "-Zcrate-attr=allow(unexpected_cfgs) -Ctarget-feature=+crt-static",
            )
            .env("RUSTC_BOOTSTRAP", "1")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(seed.status.success(), "custom-target cache seed failed: {seed:?}");
        assert!(first_target.is_dir(), "Cargo did not create the producer target");
        assert_eq!(
            fs::read_dir(first_target_parent.path())?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>(),
            [std::ffi::OsString::from("producer-target")],
            "producer wrote outside its exact external target root"
        );
        assert_no_native_runtime_residue(first_runtime.path())?;

        let second_target_parent = tempfile::tempdir()?;
        let second_target = second_target_parent.path().join("consumer-target");
        assert!(!second_target.exists(), "consumer target existed before Cargo started");
        let reused = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_TARGET_DIR", &second_target)
            .env("TMPDIR", first_runtime.path())
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage_path)
            .env(
                "RUSTFLAGS",
                "-Zcrate-attr=allow(unexpected_cfgs) -Ctarget-feature=+crt-static",
            )
            .env("RUSTC_BOOTSTRAP", "1")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            reused.status.success(),
            "the second physical target root did not reuse the verified action: {reused:?}; coverage: {:?}",
            native_coverage_summary(coverage.path())?
        );
        assert!(second_target.is_dir(), "Cargo did not create the consumer target");
        assert_eq!(
            fs::read_dir(second_target_parent.path())?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>(),
            [std::ffi::OsString::from("consumer-target")],
            "consumer restore wrote outside its exact external target root"
        );
        assert_no_native_runtime_residue(first_runtime.path())?;

        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert!(
            json(&status)?["status"]["installation"]["usage"]["hits"]
                .as_u64()
                .unwrap_or_default()
                >= 1,
            "custom-target compilation did not record a native cache hit: {:?}",
            (
                native_coverage_summary(coverage.path())?,
                String::from_utf8_lossy(&seed.stderr),
                String::from_utf8_lossy(&reused.stderr),
            )
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
fn assert_no_native_runtime_residue(directory: &Path) -> Result<()> {
    let residue = fs::read_dir(directory)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("cargo-rail-native-cargo-"))
        .collect::<Vec<_>>();
    anyhow::ensure!(residue.is_empty(), "native wrapper runtime residue: {residue:?}");
    Ok(())
}

#[cfg(unix)]
type NativeCoverageSummary = (Option<String>, String, Option<String>, Option<String>);

#[cfg(unix)]
fn native_coverage_summary(directory: &Path) -> Result<Vec<NativeCoverageSummary>> {
    let mut summary = fs::read_dir(directory)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| -> Result<_> {
            let event: serde_json::Value = serde_json::from_slice(&fs::read(entry.path())?)?;
            Ok((
                event["action"]["crate_name"].as_str().map(str::to_string),
                event["status"].as_str().unwrap_or("missing").to_string(),
                event["reason"].as_str().map(str::to_string),
                event["action_key"].as_str().map(str::to_string),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    summary.sort();
    Ok(summary)
}

#[cfg(unix)]
#[test]
fn local_cache_outage_executes_cold_and_setup_repairs_the_same_authority() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-outage", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(cold.status.success(), "cold seed failed: {cold:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        fs::remove_dir_all(&cache_root)?;

        let log = workspace.path.join("outage-rustc.log");
        let shim = workspace.path.join("rustc-outage-proof");
        fs::write(
            &shim,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
        let outage = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("RUSTC", &shim)
            .env("REAL_RUSTC", "rustc")
            .env("RUSTC_LOG", &log)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(outage.status.success(), "cache outage did not compile cold: {outage:?}");
        assert!(fs::read_to_string(&log)?.contains("transparent_outage"));

        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        assert_eq!(json(&status)?["status"]["installation"]["state"], "drifted");
        let check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--check"],
        )?;
        assert_eq!(check.status.code(), Some(1));
        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "cache authority repair failed: {repair:?}");
        assert!(cache_root.is_dir());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn workspace_wrapper_composes_by_bypassing_and_recursive_composition_is_rejected() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent-wrapper-chain", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let status = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
        )?;
        let wrapper = json(&status)?["status"]["installation"]["wrapper_path"]
            .as_str()
            .context("wrapper path")?
            .to_string();

        let chain = workspace.path.join("workspace-rustc-wrapper");
        let log = workspace.path.join("workspace-wrapper.log");
        fs::write(
            &chain,
            "#!/bin/sh\nprintf 'called\\n' >> \"$WRAPPER_LOG\"\nexec \"$@\"\n",
        )?;
        fs::set_permissions(&chain, fs::Permissions::from_mode(0o700))?;
        fs::create_dir_all(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!("[build]\nrustc-workspace-wrapper = '{}'\n", chain.display()),
        )?;
        let composed = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("WRAPPER_LOG", &log)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            composed.status.success(),
            "safe wrapper composition failed: {composed:?}"
        );
        assert!(fs::read_to_string(&log)?.contains("called"));

        fs::remove_dir_all(workspace.path.join("target"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!("[build]\nrustc-workspace-wrapper = '{wrapper}'\n"),
        )?;
        let recursive = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(!recursive.status.success(), "recursive wrapper unexpectedly ran");
        assert!(
            String::from_utf8_lossy(&recursive.stderr).contains("recursive transparent wrapper configuration"),
            "recursive wrapper failure was ambiguous: {recursive:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn removal_refuses_a_changed_wrapper_field_and_preserves_unowned_configuration() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-remove-drift", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let config = cargo_home.path().join("config.toml");
        fs::write(
            &config,
            "[build]\nrustc-wrapper = 'replacement-wrapper'\n[net]\noffline = true\n",
        )?;
        let remove = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "uninstall", "--check"],
        )?;
        assert_eq!(remove.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&remove.stderr).contains("removal refused"));
        assert!(fs::read_to_string(&config)?.contains("replacement-wrapper"));
        assert!(
            cargo_home
                .path()
                .join("cargo-rail/compiler-cache-v1/setup.json")
                .is_file()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn local_cleanup_uses_the_receipt_selected_custom_cache_and_is_repairable() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-custom-clean", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let cache_base = tempfile::tempdir()?;
        let cache_base_root = fs::canonicalize(cache_base.path())?;
        let cache_base_arg = cache_base.path().to_str().context("cache base path")?;
        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--local-dir", cache_base_arg],
        )?;
        assert!(setup.status.success(), "custom setup failed: {setup:?}");
        let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(cold.status.success(), "custom cache seed failed: {cold:?}");
        let custom_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        let installation = cargo_home.path().join("cargo-rail/compiler-cache-v1");
        #[cfg(not(windows))]
        let wrapper = installation.join("cargo-rail-native-rustc-wrapper");
        #[cfg(windows)]
        let wrapper = installation.join("cargo-rail-native-rustc-wrapper.exe");
        #[cfg(not(windows))]
        let worker = installation.join("cargo-rail-native-rustc-worker");
        #[cfg(windows)]
        let worker = installation.join("cargo-rail-native-rustc-worker.exe");
        let receipt = installation.join("setup.json");
        let config = cargo_home.path().join("config.toml");
        let wrapper_evidence = capture_unchanged_file(&wrapper)?;
        let worker_evidence = capture_unchanged_file(&worker)?;
        let receipt_evidence = capture_unchanged_file(&receipt)?;
        let config_evidence = capture_unchanged_file(&config)?;
        assert!(custom_root.is_dir());
        assert!(custom_root.starts_with(&cache_base_root));

        let check = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "clean", "--scope", "local", "--check"],
        )?;
        assert_eq!(check.status.code(), Some(1));
        assert!(custom_root.is_dir(), "cleanup preview mutated the selected cache");
        let clean = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "clean", "--scope", "local"],
        )?;
        assert!(clean.status.success(), "custom cache cleanup failed: {clean:?}");
        assert!(!custom_root.exists());
        assert!(custom_root.starts_with(&cache_base_root));

        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "custom cache repair failed: {repair:?}");
        assert!(
            custom_root.is_dir(),
            "repair changed or ignored the receipt-selected cache"
        );
        assert_unchanged_file(&wrapper, &wrapper_evidence, "installed compiler wrapper")?;
        assert_unchanged_file(&worker, &worker_evidence, "installed compiler worker")?;
        assert_unchanged_file(&receipt, &receipt_evidence, "installation receipt")?;
        assert_unchanged_file(&config, &config_evidence, "Cargo configuration")?;
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn ordinary_cargo_and_nextest_commands_receive_eligible_library_reuse() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("transparent_shapes", "0.1.0")?;
        fs::write(
            workspace.path.join("src/main.rs"),
            "fn main() { println!(\"{}\", transparent_shapes::hello()); }\n",
        )?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");

        let shim = workspace.path.join("rustc-command-shape-proof");
        fs::write(
            &shim,
            r#"#!/bin/sh
crate_name=
crate_type=
previous=
for argument in "$@"; do
  if [ "$previous" = "--crate-name" ]; then crate_name="$argument"; fi
  if [ "$previous" = "--crate-type" ]; then crate_type="$argument"; fi
  case "$argument" in
    --crate-name=*) crate_name="${argument#--crate-name=}" ;;
    --crate-type=*) crate_type="${argument#--crate-type=}" ;;
  esac
  previous="$argument"
done
if [ "$crate_name" = "transparent_shapes" ] && [ "$crate_type" = "lib" ]; then exit 91; fi
exec "$REAL_RUSTC" "$@"
"#,
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;

        let lanes: &[(&str, &[&str])] = &[
            ("check", &["check", "--quiet"]),
            ("build", &["build", "--quiet"]),
            ("test", &["test", "--quiet"]),
            ("run", &["run", "--quiet"]),
            ("bench", &["bench", "--quiet", "--no-run"]),
            ("nextest", &["nextest", "run"]),
        ];
        for (name, arguments) in lanes {
            let seed = Command::new("cargo")
                .current_dir(&workspace.path)
                .args(*arguments)
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?;
            assert!(seed.status.success(), "{name} seed failed: {seed:?}");
            fs::remove_dir_all(workspace.path.join("target"))?;
            let reused = Command::new("cargo")
                .current_dir(&workspace.path)
                .args(*arguments)
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("RUSTC", &shim)
                .env("REAL_RUSTC", "rustc")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?;
            assert!(
                reused.status.success(),
                "{name} executed an eligible library compiler instead of restoring it: {reused:?}"
            );
            fs::remove_dir_all(workspace.path.join("target"))?;
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

fn directory_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_unstable_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, snapshot)?;
            } else if metadata.is_file() {
                snapshot.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(path)?);
            }
        }
        Ok(())
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot)?;
    Ok(snapshot)
}
