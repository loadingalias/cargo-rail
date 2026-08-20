//! Front-door coverage for transparent local compiler-cache installation.

use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
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
    let coverage = fs::canonicalize(coverage).context("canonicalize native-cache coverage directory")?;
    Command::new("cargo")
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
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()
        .context("run cargo check with installed remote policy")
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
        assert_eq!(status["status"]["schema_version"], 11);
        assert_eq!(status["status"]["installation"]["state"], "installed");
        assert_eq!(status["status"]["installation"]["healthy"], true);
        let wrapper = PathBuf::from(
            status["status"]["installation"]["wrapper_path"]
                .as_str()
                .context("installed wrapper path")?,
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
            &["rail", "cache", "remove", "--check", "-f", "json"],
        )?;
        assert_eq!(remove_check.status.code(), Some(1));
        assert!(config.exists(), "removal preview mutated Cargo config");
        let remove = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "remove", "-f", "json"],
        )?;
        assert!(remove.status.success(), "removal failed: {remove:?}");
        assert_eq!(fs::read_to_string(&config)?, original);
        assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());
        assert!(
            cargo_home.path().join("cargo-rail/local-cas-v2").exists(),
            "removal deleted the compiler-result cache"
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

        let remove = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "remove"])?;
        assert!(remove.status.success(), "qualification removal failed: {remove:?}");
        assert!(
            !distributed_worker.exists(),
            "removal retained the receipt-owned worker"
        );
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
        assert_eq!(value["status"]["schema_version"], 11);
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
        assert!(
            remote
                .requests()
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
            "automatic remote seed did not publish an entry"
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
        let remove = rail(&workspace.path, import_home.path(), &["rail", "cache", "remove"])?;
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
        let cache_root = cargo_home.path().join("cargo-rail/local-cas-v2");
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
fn compiler_fact_collection_bypasses_an_ordinary_native_result_hit() {
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
        let native_actions = cargo_home.path().join("cargo-rail/local-cas-v2/native-actions-v2");
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
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
        )?;
        fs::set_permissions(&rustc_shim, fs::Permissions::from_mode(0o700))?;
        let analysis = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(&workspace.path)
            .args(["rail", "unify", "--check"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("RUSTC", &rustc_shim)
            .env("REAL_RUSTC", "rustc")
            .env("RUSTC_LOG", &rustc_log)
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
            String::from_utf8_lossy(&analysis.stdout).contains("unused deps removed: 1"),
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

#[test]
fn unsupported_shapes_bypass_before_acquisition_while_proc_macro_producers_remain_cacheable() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("transparent-early-bypass", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "setup failed: {setup:?}");
        let installation = cargo_home.path().join("cargo-rail/compiler-cache-v1");
        let cache_root = cargo_home.path().join("cargo-rail/local-cas-v2");
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
        assert_eq!(
            directory_snapshot(&cache_root)?,
            before,
            "incremental bypass touched L1"
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
        assert_eq!(directory_snapshot(&cache_root)?, before, "clippy bypass touched L1");

        let custom_target = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--quiet"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_TARGET_DIR", workspace.path.join("custom-target"))
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        assert!(
            custom_target.status.success(),
            "custom target-dir bypass failed: {custom_target:?}"
        );
        assert!(!installation.join("session.json").exists());
        assert!(!installation.join("usage-v1.log").exists());
        assert_eq!(
            directory_snapshot(&cache_root)?,
            before,
            "custom target-dir bypass touched L1"
        );

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
        fs::remove_dir_all(cargo_home.path().join("cargo-rail/local-cas-v2"))?;

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
        assert!(cargo_home.path().join("cargo-rail/local-cas-v2").is_dir());
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
            &["rail", "cache", "remove", "--check"],
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
        let cache_base_arg = cache_base.path().to_str().context("cache base path")?;
        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--local-dir", cache_base_arg],
        )?;
        assert!(setup.status.success(), "custom setup failed: {setup:?}");
        let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(cold.status.success(), "custom cache seed failed: {cold:?}");
        let custom_root = cache_base.path().join("cargo-rail/local-cas-v2");
        assert!(custom_root.is_dir());
        assert!(!cargo_home.path().join("cargo-rail/local-cas-v2").exists());

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
        assert!(!cargo_home.path().join("cargo-rail/local-cas-v2").exists());

        let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(repair.status.success(), "custom cache repair failed: {repair:?}");
        assert!(
            custom_root.is_dir(),
            "repair changed or ignored the receipt-selected cache"
        );
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
