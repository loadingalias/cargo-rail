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
#[cfg(windows)]
use crate::helpers::assert_native_driver_unavailable_bypass;

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

#[cfg(unix)]
#[test]
fn authenticated_compiler_components_follow_installation_ownership() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let manufactured = PathBuf::from(std::env::var_os("CARGO_RAIL_TEST_COMPONENT_BINARY").context(
            "set CARGO_RAIL_TEST_COMPONENT_BINARY to the cargo-rail binary from the authenticated component preparation route",
        )?);
        let source = manufactured.parent().context("manufactured component directory")?;
        let bundle = tempfile::tempdir()?;
        for name in [
            "cargo-rail",
            "cargo-rail-native-rustc-wrapper",
            "cargo-rail-native-rustc-worker",
            "cargo-rail-fact-driver",
            "cargo-rail-fact-driver-source-v1.json",
        ] {
            fs::copy(source.join(name), bundle.path().join(name))?;
        }
        let workspace = TestWorkspace::new_single_crate("installed-components", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let run = |arguments: &[&str]| -> Result<Output> {
            let mut command = Command::new(bundle.path().join("cargo-rail"));
            for (name, _) in std::env::vars_os() {
                if name.to_str().is_some_and(|name| name.starts_with("CARGO_RAIL_")) {
                    command.env_remove(name);
                }
            }
            command
                .args(arguments)
                .current_dir(&workspace.path)
                .env("CARGO_HOME", cargo_home.path())
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()
                .context("run authenticated component installation")
        };
        let source_bundle = bundle.path().join("cargo-rail-fact-driver-source-v1.json");
        let source_bytes = fs::read(&source_bundle)?;
        fs::remove_file(&source_bundle)?;
        let missing = run(&["rail", "cache", "setup"])?;
        anyhow::ensure!(
            !missing.status.success(),
            "missing authenticated source was accepted: {missing:?}"
        );
        anyhow::ensure!(
            fs::read_dir(cargo_home.path())?.next().is_none(),
            "rejected setup wrote Cargo state"
        );
        fs::write(&source_bundle, &source_bytes)?;

        let setup = run(&["rail", "cache", "setup"])?;
        anyhow::ensure!(setup.status.success(), "component setup failed: {setup:?}");
        let installation = fs::canonicalize(cargo_home.path())?.join("cargo-rail/compiler-cache-v1");
        let receipt_path = installation.join("setup.json");
        let receipt_bytes = fs::read(&receipt_path)?;
        let receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes)?;
        assert_eq!(receipt["version"], 5);
        let components = receipt["compiler_components"]
            .as_array()
            .context("required component inventory")?;
        assert_eq!(components.len(), 2);
        let names = components
            .iter()
            .map(|component| {
                let path = PathBuf::from(component["path"].as_str().context("component path")?);
                anyhow::ensure!(
                    path.parent() == Some(installation.as_path()),
                    "component escaped installation"
                );
                let name = path.file_name().context("component name")?.to_owned();
                anyhow::ensure!(
                    fs::read(&path)? == fs::read(bundle.path().join(&name))?,
                    "installed component bytes changed"
                );
                let mode = if name == "cargo-rail-fact-driver" { 0o700 } else { 0o600 };
                anyhow::ensure!(
                    fs::metadata(&path)?.permissions().mode() & 0o777 == mode,
                    "component permissions changed"
                );
                Ok(name)
            })
            .collect::<Result<BTreeSet<_>>>()?;
        assert_eq!(
            names,
            BTreeSet::from([
                "cargo-rail-fact-driver".into(),
                "cargo-rail-fact-driver-source-v1.json".into()
            ])
        );
        let before = components
            .iter()
            .map(|component| {
                let path = PathBuf::from(component["path"].as_str().context("component path")?);
                Ok((path.clone(), capture_unchanged_file(&path)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let repeated = run(&["rail", "cache", "setup"])?;
        anyhow::ensure!(
            repeated.status.success(),
            "repeated component setup failed: {repeated:?}"
        );
        assert_eq!(fs::read(&receipt_path)?, receipt_bytes);
        for (path, identity) in before {
            assert_unchanged_file(&path, &identity, "unchanged compiler component")?;
        }

        let installed_source = installation.join("cargo-rail-fact-driver-source-v1.json");
        fs::write(&installed_source, b"changed component bytes")?;
        let status = run(&["rail", "cache", "status", "--scope", "local", "-f", "json"])?;
        assert_eq!(json(&status)?["status"]["installation"]["healthy"], false);
        let config_before = fs::read(cargo_home.path().join("config.toml"))?;
        let retained_files = [
            "cargo-rail-native-rustc-wrapper",
            "cargo-rail-native-rustc-worker",
            "cargo-rail-fact-driver",
        ]
        .into_iter()
        .map(|name| {
            let path = installation.join(name);
            Ok((path.clone(), capture_unchanged_file(&path)?))
        })
        .collect::<Result<Vec<_>>>()?;
        let refused = run(&["rail", "cache", "uninstall"])?;
        anyhow::ensure!(
            !refused.status.success(),
            "changed component removal was accepted: {refused:?}"
        );
        assert_eq!(fs::read(cargo_home.path().join("config.toml"))?, config_before);
        assert_eq!(fs::read(&receipt_path)?, receipt_bytes);
        for (path, identity) in retained_files {
            assert_unchanged_file(&path, &identity, "compiler file after refused component removal")?;
        }
        assert_eq!(fs::read(&installed_source)?, b"changed component bytes");
        let repair = run(&["rail", "cache", "setup"])?;
        anyhow::ensure!(repair.status.success(), "component repair failed: {repair:?}");
        assert_eq!(fs::read(&installed_source)?, source_bytes);
        let status = run(&["rail", "cache", "status", "--scope", "local", "-f", "json"])?;
        assert_eq!(json(&status)?["status"]["installation"]["healthy"], true);
        let sentinel = installation.join("unowned-user-file");
        fs::write(&sentinel, b"retain this file")?;
        let removed = run(&["rail", "cache", "uninstall"])?;
        anyhow::ensure!(removed.status.success(), "component removal failed: {removed:?}");
        assert!(!installed_source.exists());
        assert!(!installation.join("cargo-rail-fact-driver").exists());
        assert!(!receipt_path.exists());
        assert_eq!(fs::read(sentinel)?, b"retain this file");
        Ok(())
    })();
    super::helpers::finish_test(result);
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
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, None, None, None)
}

fn cargo_check_installed_remote_in_target(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    target: &Path,
) -> Result<Output> {
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, None, Some(target), None)
}

fn cargo_check_installed_remote_with_rustflags(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    rustflags: Option<&str>,
) -> Result<Output> {
    cargo_check_installed_remote_with_options(workspace, cargo_home, coverage, rustflags, None, None)
}

fn cargo_check_installed_remote_with_options(
    workspace: &Path,
    cargo_home: &Path,
    coverage: &Path,
    rustflags: Option<&str>,
    target: Option<&Path>,
    artifact_target: Option<&str>,
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
    if let Some(artifact_target) = artifact_target {
        command.arg("--target").arg(artifact_target);
    }
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

fn explicit_target_cargo(
    workspace: &Path,
    cargo_home: &Path,
    workload: &str,
    target: &str,
    report: Option<&Path>,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .args([workload, "--offline", "--quiet", "--target", target])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_RAIL_CACHE")
        .env_remove("CARGO_RAIL_CACHE_REMOTE")
        .env_remove("CARGO_RAIL_CACHE_MODE")
        .env_remove("CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT")
        .env_remove("CARGO_RAIL_CACHE_REPORT")
        .env_remove("OUT_DIR");
    if let Some(report) = report {
        command.env("CARGO_RAIL_CACHE_REPORT", report);
    }
    let output = command
        .output()
        .with_context(|| format!("run explicit-target cargo {workload} for {target}"))?;
    anyhow::ensure!(
        output.status.success(),
        "cargo {workload} --target {target} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn compiler_only_outputs(directory: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut outputs = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rmeta" | "rlib")
        ) {
            outputs.insert(path.strip_prefix(directory)?.to_path_buf(), fs::read(path)?);
        }
    }
    Ok(outputs)
}

fn explicit_target_reuse(target: &str, mixed_host_target: bool) -> Result<()> {
    for workload in ["check", "build"] {
        let workspace = TestWorkspace::new_single_crate("explicit-target", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let source = if mixed_host_target {
            fs::write(
                workspace.path.join("Cargo.toml"),
                "[package]\nname = \"explicit-target\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
                 [dependencies]\nhost-target-support = { path = \"support\" }\n\
                 [build-dependencies]\nhost-target-support = { path = \"support\" }\n",
            )?;
            fs::create_dir_all(workspace.path.join("support/src"))?;
            fs::write(
                workspace.path.join("support/Cargo.toml"),
                "[package]\nname = \"host-target-support\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )?;
            fs::write(workspace.path.join("support/src/lib.rs"), "pub const VALUE: u64 = 7;\n")?;
            fs::write(
                workspace.path.join("build.rs"),
                "fn main() { assert_eq!(host_target_support::VALUE, 7); \
                 println!(\"cargo::rerun-if-changed=build.rs\"); }\n",
            )?;
            "pub const VALUE: u64 = 41 + host_target_support::VALUE;\n"
        } else {
            "pub const VALUE: u64 = 41;\n"
        };
        fs::write(workspace.path.join("src/lib.rs"), source)?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), workload, target, None)
            .context("uncached upstream fixture must build before cache qualification")?;
        let target_directory = workspace.path.join("target").join(target);
        let outputs_directory = target_directory.join("debug/deps");
        let baseline = compiler_only_outputs(&outputs_directory)?;
        let expected_libraries = if mixed_host_target { 2 } else { 1 };
        let metadata_count = baseline
            .keys()
            .filter(|path| path.extension().is_some_and(|extension| extension == "rmeta"))
            .count();
        anyhow::ensure!(
            metadata_count == expected_libraries,
            "upstream {workload} produced {metadata_count} metadata files; expected {expected_libraries}"
        );
        let archive_count = baseline
            .keys()
            .filter(|path| path.extension().is_some_and(|extension| extension == "rlib"))
            .count();
        let expected_archives = if workload == "build" { expected_libraries } else { 0 };
        anyhow::ensure!(
            archive_count == expected_archives,
            "upstream {workload} produced {archive_count} archives; expected {expected_archives}"
        );
        let host_outputs = if mixed_host_target {
            let outputs = compiler_only_outputs(&workspace.path.join("target/debug/deps"))?;
            anyhow::ensure!(
                outputs
                    .keys()
                    .any(|path| path.extension().is_some_and(|extension| extension == "rlib")),
                "the shared build dependency was not compiled for the host"
            );
            Some(outputs)
        } else {
            None
        };
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        anyhow::ensure!(setup.status.success(), "explicit-target cache setup failed: {setup:?}");

        fs::remove_dir_all(&target_directory)?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), workload, target, None)?;
        let cold = selected_profile_status(&workspace.path, cargo_home.path())?;
        let cold_usage = &cold["status"]["installation"]["usage"];
        #[cfg(not(windows))]
        anyhow::ensure!(
            cold_usage["misses"] == expected_libraries,
            "cold {workload} did not cache every target library: {cold_usage}"
        );
        #[cfg(windows)]
        anyhow::ensure!(
            cold_usage["misses"] == 0 && cold_usage["failures"] == 0 && cold_usage["bypasses"] == expected_libraries,
            "unexpected Windows cold outcome: {cold_usage}"
        );
        anyhow::ensure!(
            cold_usage["hits"] == 0,
            "cold {workload} reused an unseeded result: {cold_usage}"
        );
        anyhow::ensure!(
            compiler_only_outputs(&outputs_directory)? == baseline,
            "cached cold {workload} changed upstream output bytes"
        );

        fs::remove_dir_all(&target_directory)?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), workload, target, None)?;
        let warm = selected_profile_status(&workspace.path, cargo_home.path())?;
        let warm_usage = &warm["status"]["installation"]["usage"];
        #[cfg(not(windows))]
        anyhow::ensure!(
            warm_usage["hits"] == expected_libraries,
            "warm {workload} did not restore every target library: {warm_usage}"
        );
        #[cfg(windows)]
        anyhow::ensure!(
            warm_usage["hits"] == 0 && warm_usage["failures"] == 0 && warm_usage["bypasses"] == expected_libraries * 2,
            "unexpected Windows warm outcome: {warm_usage}"
        );
        anyhow::ensure!(
            warm_usage["misses"] == cold_usage["misses"],
            "warm {workload} compiled a target library: cold {cold_usage}; warm {warm_usage}"
        );
        anyhow::ensure!(
            compiler_only_outputs(&outputs_directory)? == baseline,
            "warm {workload} restored different output bytes"
        );

        let changed_source = source.replace("41", "42");
        anyhow::ensure!(
            source.len() == changed_source.len(),
            "source replacement changed length from {} to {}",
            source.len(),
            changed_source.len()
        );
        fs::write(workspace.path.join("src/lib.rs"), changed_source)?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), workload, target, None)?;
        let changed = selected_profile_status(&workspace.path, cargo_home.path())?;
        let changed_usage = &changed["status"]["installation"]["usage"];
        #[cfg(not(windows))]
        anyhow::ensure!(
            changed_usage["misses"] == expected_libraries + 1,
            "same-size source replacement did not cause a miss: {changed_usage}"
        );
        #[cfg(windows)]
        anyhow::ensure!(
            changed_usage["misses"] == 0
                && changed_usage["failures"] == 0
                && changed_usage["bypasses"] == expected_libraries * 2 + 1,
            "unexpected Windows changed outcome: {changed_usage}"
        );
        anyhow::ensure!(
            changed_usage["hits"] == warm_usage["hits"],
            "same-size source replacement reused stale output: warm {warm_usage}; changed {changed_usage}"
        );
        anyhow::ensure!(
            compiler_only_outputs(&outputs_directory)? != baseline,
            "same-size source replacement restored the old library"
        );
        if let Some(host_outputs) = host_outputs {
            anyhow::ensure!(
                compiler_only_outputs(&workspace.path.join("target/debug/deps"))? == host_outputs,
                "target cache activity changed the host build dependency"
            );
        }
    }
    Ok(())
}

#[test]
fn explicit_host_target_check_and_build_restore_exact_compiler_outputs() {
    let result: Result<()> = (|| {
        let version = Command::new("rustc").arg("-vV").output()?;
        anyhow::ensure!(version.status.success(), "rustc -vV failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host target")?;
        explicit_target_reuse(host, false)
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn cross_target_check_and_build_restore_libraries_without_changing_host_dependencies() {
    let result: Result<()> = (|| {
        let target = "x86_64-unknown-linux-gnu";
        let installed = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()?;
        anyhow::ensure!(
            installed.status.success(),
            "cannot inspect installed targets: {installed:?}"
        );
        anyhow::ensure!(
            String::from_utf8(installed.stdout)?
                .lines()
                .any(|installed| installed == target),
            "cross-target cache qualification requires the {target} standard library declared in rust-toolchain.toml; run rustup target add {target}"
        );
        explicit_target_reuse(target, true)
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn explicit_default_linker_restores_the_exact_executable_and_mode() {
    super::helpers::finish_test(explicit_linker_reuse(false));
}

#[cfg(target_os = "macos")]
#[test]
fn direct_rust_lld_restores_the_exact_executable_and_mode() {
    super::helpers::finish_test(explicit_linker_reuse(true));
}

#[cfg(target_os = "macos")]
#[test]
fn dynamic_libraries_restore_after_rustc_removes_its_export_lists() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("export-lists", "0.1.0")?;
        fs::write(
            workspace.path.join("Cargo.toml"),
            "[workspace]\nmembers = [\"dylib\", \"cdylib\", \"macros\"]\nresolver = \"3\"\n\
             [profile.dev]\ndebug = 1\nsplit-debuginfo = \"unpacked\"\n",
        )?;
        for (name, crate_type, source) in [
            ("dylib", "dylib", "pub fn answer() -> u64 { 41 }\n"),
            (
                "cdylib",
                "cdylib",
                "#[unsafe(no_mangle)] pub extern \"C\" fn answer() -> u64 { 42 }\n",
            ),
            (
                "macros",
                "proc-macro",
                "use proc_macro::TokenStream;\n\
                 #[proc_macro] pub fn answer(_: TokenStream) -> TokenStream { \"43\".parse().unwrap() }\n",
            ),
        ] {
            let directory = workspace.path.join(name);
            fs::create_dir_all(directory.join("src"))?;
            fs::write(
                directory.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"export-{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                     [lib]\ncrate-type = [\"{crate_type}\"]\n"
                ),
            )?;
            fs::write(directory.join("src/lib.rs"), source)?;
        }
        let version = Command::new("rustc").arg("-vV").output()?;
        anyhow::ensure!(version.status.success(), "rustc prerequisite failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host")?;
        let cargo_home = tempfile::tempdir()?;
        let target = workspace.path.join("target");
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)?;
        let mut baseline = BTreeMap::new();
        for directory in [target.join("debug/deps"), target.join(host).join("debug/deps")] {
            if !directory.is_dir() {
                continue;
            }
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path.extension().is_some_and(|extension| extension == "dylib")
                    || path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(".rcgu.o"))
                {
                    baseline.insert(
                        path.strip_prefix(&target)?.to_path_buf(),
                        (fs::read(&path)?, fs::metadata(&path)?.permissions().mode() & 0o777),
                    );
                }
            }
        }
        assert_eq!(
            baseline
                .keys()
                .filter(|path| path.extension().is_some_and(|extension| extension == "dylib"))
                .count(),
            3,
            "upstream dynamic-library inventory"
        );
        assert!(
            baseline
                .keys()
                .any(|path| path.extension().is_some_and(|extension| extension == "o")),
            "upstream separate debug objects are absent"
        );
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "export-list cache setup failed: {setup:?}");
        let reports = tempfile::tempdir()?;
        for (phase, hits) in [("cold", 0), ("warm", 3)] {
            fs::remove_dir_all(&target)?;
            let recording = reports.path().join(format!("{phase}.json"));
            let report_path = recording.to_str().context("export-list report path")?;
            let start = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", report_path],
            )?;
            assert!(start.status.success(), "{phase} report start failed: {start:?}");
            explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, Some(&recording))?;
            let finish = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", report_path, "-f", "json"],
            )?;
            assert!(finish.status.success(), "{phase} report finish failed: {finish:?}");
            let report = json(&finish)?;
            assert_eq!(report["measurements"]["hits"], hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["misses"], 3 - hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["failures"], 0, "{phase}: {report}");
            for (path, (bytes, mode)) in &baseline {
                let output = target.join(path);
                assert!(fs::read(&output)? == *bytes, "{phase} changed {}", path.display());
                assert_eq!(
                    fs::metadata(&output)?.permissions().mode() & 0o777,
                    *mode,
                    "{phase} mode of {}",
                    path.display()
                );
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
fn explicit_linker_reuse(direct_lld: bool) -> Result<()> {
    (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let version = Command::new("rustc").arg("-vV").output()?;
        anyhow::ensure!(version.status.success(), "rustc -vV failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host target")?;
        let (linker, flags) = if direct_lld {
            let sysroot = Command::new("rustc").arg("--print=sysroot").output()?;
            anyhow::ensure!(sysroot.status.success(), "rustc sysroot query failed: {sysroot:?}");
            let sysroot = String::from_utf8(sysroot.stdout)?;
            (
                Path::new(sysroot.trim())
                    .join("lib/rustlib")
                    .join(host)
                    .join("bin/rust-lld"),
                "rustflags = [\"-Clinker-flavor=ld64.lld\"]\n",
            )
        } else {
            (PathBuf::from("/usr/bin/cc"), "")
        };
        let workspace = TestWorkspace::new_single_crate("explicit-linker", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        fs::remove_file(workspace.path.join("src/lib.rs"))?;
        fs::write(
            workspace.path.join("src/main.rs"),
            "fn main() { println!(\"explicit linker result\"); }\n",
        )?;
        fs::create_dir(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!(
                "[target.{host}]\nlinker = {}\n{flags}\n[profile.dev]\nsplit-debuginfo = \"off\"\n",
                serde_json::to_string(linker.to_str().context("linker path")?)?
            ),
        )?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)
            .context("uncached upstream explicit-linker fixture must build before cache qualification")?;
        let target_directory = workspace.path.join("target").join(host);
        let executable = target_directory.join("debug/explicit-linker");
        let baseline_bytes = fs::read(&executable)?;
        let baseline_mode = fs::metadata(&executable)?.permissions().mode() & 0o777;
        anyhow::ensure!(baseline_mode & 0o111 != 0, "upstream output has no executable mode");
        let baseline_output = Command::new(&executable).output()?;
        anyhow::ensure!(
            baseline_output.status.success(),
            "upstream executable failed: {baseline_output:?}"
        );
        anyhow::ensure!(
            baseline_output.stdout == b"explicit linker result\n",
            "upstream executable stdout changed: {baseline_output:?}"
        );
        anyhow::ensure!(baseline_output.stderr.is_empty(), "upstream executable emitted stderr");
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        anyhow::ensure!(setup.status.success(), "explicit-linker cache setup failed: {setup:?}");

        let reports = tempfile::tempdir()?;
        for (phase, expected_hits) in [("cold", 0), ("warm", 1)] {
            fs::remove_dir_all(&target_directory)?;
            let recording = reports.path().join(format!("{phase}.json"));
            let path = recording.to_str().context("explicit-linker report path")?;
            let started = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", path],
            )?;
            anyhow::ensure!(started.status.success(), "{phase} report start failed: {started:?}");
            explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, Some(&recording))?;
            let finished = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", path, "-f", "json"],
            )?;
            anyhow::ensure!(finished.status.success(), "{phase} report finish failed: {finished:?}");
            let report = json(&finished)?;
            let status = selected_profile_status(&workspace.path, cargo_home.path())?;
            let usage = &status["status"]["installation"]["usage"];
            anyhow::ensure!(
                usage["misses"] == 1,
                "{phase} explicit-linker build did not retain one verified miss: {usage}; report: {report}"
            );
            anyhow::ensure!(
                usage["hits"] == expected_hits,
                "{phase} explicit-linker build had the wrong reuse outcome: {usage}; report: {report}"
            );
            anyhow::ensure!(
                fs::read(&executable)? == baseline_bytes,
                "{phase} explicit-linker output differs from uncached bytes"
            );
            anyhow::ensure!(
                fs::metadata(&executable)?.permissions().mode() & 0o777 == baseline_mode,
                "{phase} explicit-linker executable mode changed"
            );
            let output = Command::new(&executable).output()?;
            anyhow::ensure!(
                output.status.success(),
                "{phase} explicit-linker executable failed: {output:?}"
            );
            anyhow::ensure!(
                output.stdout == baseline_output.stdout,
                "{phase} executable stdout changed"
            );
            anyhow::ensure!(
                output.stderr == baseline_output.stderr,
                "{phase} executable stderr changed"
            );
        }
        Ok(())
    })()
}

#[cfg(target_os = "macos")]
#[test]
fn cranelift_emitted_library_reuse_binds_backend_bytes_and_preserves_assembly_bypass() {
    let result: Result<()> = (|| {
        let toolchain = std::env::var("CARGO_RAIL_TEST_CRANELIFT_TOOLCHAIN")
            .context("select the matched Cranelift test toolchain")?;
        let workspace = TestWorkspace::new_single_crate("clif-fixture", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let copied = tempfile::tempdir()?;
        let original = Command::new("rustc")
            .env("RUSTUP_TOOLCHAIN", &toolchain)
            .args(["--print", "sysroot"])
            .output()?;
        anyhow::ensure!(original.status.success(), "Cranelift toolchain: {original:?}");
        let sysroot = copied.path().join("sysroot");
        let copied_output = Command::new("cp")
            .arg("-cR")
            .arg(String::from_utf8(original.stdout)?.trim())
            .arg(&sysroot)
            .output()?;
        anyhow::ensure!(
            copied_output.status.success(),
            "private sysroot clone: {copied_output:?}"
        );
        let rustc = sysroot.join("bin/rustc");
        let version = Command::new(&rustc).arg("-vV").output()?;
        anyhow::ensure!(version.status.success(), "copied compiler: {version:?}");
        let version = String::from_utf8(version.stdout)?;
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("compiler host")?;
        let backends = fs::read_dir(sysroot.join("lib/rustlib").join(host).join("codegen-backends"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let backend = backends
            .iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("rustc_codegen_cranelift"))
            })
            .collect::<Vec<_>>();
        let [backend] = backend.as_slice() else {
            anyhow::bail!("expected one Cranelift backend");
        };
        let mut backend_bytes = fs::read(backend)?;
        backend_bytes.push(0);
        fs::write(backend, &backend_bytes)?;
        fs::create_dir_all(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            "[build]\nrustflags = [\"-Zcodegen-backend=cranelift\"]\n",
        )?;
        let source = workspace.path.join("src/lib.rs");
        fs::write(&source, "pub fn value() -> u32 { 7 }\n")?;
        let target = workspace.path.join("target");
        let reports = tempfile::tempdir()?;
        let recording = reports.path().join("cranelift.json");
        let recording_path = recording.to_str().context("Cranelift recording path")?;
        let run = |program: &Path, args: &[&str], cached: bool| -> Result<Output> {
            let mut command = Command::new(program);
            command
                .current_dir(&workspace.path)
                .args(args)
                .env("RUSTUP_TOOLCHAIN", &toolchain)
                .env("RUSTC", &rustc)
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_TARGET_DIR", &target)
                .env("CARGO_INCREMENTAL", "0")
                .env_remove("CARGO_BUILD_TARGET")
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_RAIL_CACHE")
                .env_remove("CARGO_RAIL_CACHE_REMOTE")
                .env_remove("CARGO_RAIL_CACHE_MODE")
                .env_remove("CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT")
                .env_remove("CARGO_RAIL_CACHE_REPORT");
            if cached && program == Path::new("cargo") {
                command.env("CARGO_RAIL_CACHE_REPORT", &recording);
            }
            if cached {
                command.env_remove("RUSTC_WRAPPER");
            } else {
                command.env("RUSTC_WRAPPER", "");
            }
            Ok(command.output()?)
        };
        let compile = |cached| run(Path::new("cargo"), &["build", "--offline", "--quiet"], cached);
        let cli = Path::new(env!("CARGO_BIN_EXE_cargo-rail"));
        let baseline = compile(false)?;
        anyhow::ensure!(baseline.status.success(), "ordinary Cranelift: {baseline:?}");
        let mut expected = directory_snapshot(&target.join("debug/deps"))?;
        let modes = expected
            .keys()
            .map(|path| {
                Ok((
                    path.clone(),
                    fs::metadata(target.join("debug/deps").join(path))?.permissions(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let setup = run(cli, &["rail", "cache", "setup"], true)?;
        anyhow::ensure!(setup.status.success(), "Cranelift cache setup: {setup:?}");
        let started = run(cli, &["rail", "cache", "report", "--start", recording_path], true)?;
        anyhow::ensure!(started.status.success(), "Cranelift report: {started:?}");
        for (phase, misses, hits, value) in [
            ("cold", 1, 0, 7),
            ("warm", 1, 1, 7),
            ("source changed", 2, 1, 8),
            ("source warm", 2, 2, 8),
            ("backend changed", 3, 2, 8),
            ("backend warm", 3, 3, 8),
        ] {
            if phase == "source changed" {
                fs::write(&source, "pub fn value() -> u32 { 8 }\n")?;
            }
            if phase == "backend changed" {
                *backend_bytes.last_mut().context("backend trailer")? = 1;
                fs::write(backend, &backend_bytes)?;
            }
            fs::remove_dir_all(&target)?;
            let output = compile(true)?;
            assert_eq!(output.status.code(), baseline.status.code(), "{phase}: {output:?}");
            assert_eq!(output.stdout, baseline.stdout, "{phase}");
            assert_eq!(output.stderr, baseline.stderr, "{phase}: {output:?}");
            let status = run(
                cli,
                &["rail", "cache", "status", "--scope", "local", "-f", "json"],
                true,
            )?;
            anyhow::ensure!(status.status.success(), "cache status: {status:?}");
            let status = json(&status)?;
            let usage = &status["status"]["installation"]["usage"];
            if usage["misses"] != misses || usage["hits"] != hits {
                let report = run(
                    cli,
                    &["rail", "cache", "report", "--finish", recording_path, "-f", "json"],
                    true,
                )?;
                anyhow::bail!("{phase}: {status}; report: {}", String::from_utf8_lossy(&report.stdout));
            }
            assert_eq!(usage["misses"], misses, "{phase}: {status}");
            assert_eq!(usage["hits"], hits, "{phase}: {status}");
            let observed = directory_snapshot(&target.join("debug/deps"))?;
            if phase == "source changed" {
                assert_ne!(observed, expected, "source change must change emitted bytes");
                expected = observed;
            } else {
                assert_eq!(observed, expected, "{phase} exact outputs");
            }
            for (path, mode) in &modes {
                assert_eq!(
                    &fs::metadata(target.join("debug/deps").join(path))?.permissions(),
                    mode,
                    "{phase}"
                );
            }
            let consumer = copied.path().join("consumer.rs");
            fs::write(&consumer, "fn main() { println!(\"{}\", clif_fixture::value()); }\n")?;
            let executable = copied.path().join("consumer");
            let linked = Command::new(&rustc)
                .arg(&consumer)
                .arg("--extern")
                .arg(format!(
                    "clif_fixture={}",
                    target.join("debug/libclif_fixture.rlib").display()
                ))
                .arg("-o")
                .arg(&executable)
                .output()?;
            anyhow::ensure!(linked.status.success(), "consume emitted rlib: {linked:?}");
            let executed = Command::new(&executable).output()?;
            anyhow::ensure!(executed.status.success(), "execute rlib consumer: {executed:?}");
            assert_eq!(executed.stdout, format!("{value}\n").as_bytes());
            assert!(executed.stderr.is_empty());
        }
        fs::write(
            &source,
            "pub fn value() -> u32 { 8 }\ncore::arch::global_asm!(\".byte 0\");\n",
        )?;
        fs::remove_dir_all(&target)?;
        let assembly_baseline = compile(false)?;
        anyhow::ensure!(
            assembly_baseline.status.success(),
            "ordinary assembly: {assembly_baseline:?}"
        );
        let assembly_outputs = directory_snapshot(&target.join("debug/deps"))?;
        fs::remove_dir_all(&target)?;
        let assembly = compile(true)?;
        assert_eq!(assembly.status.code(), assembly_baseline.status.code(), "{assembly:?}");
        assert_eq!(assembly.stdout, assembly_baseline.stdout);
        assert_eq!(assembly.stderr, assembly_baseline.stderr);
        assert_eq!(directory_snapshot(&target.join("debug/deps"))?, assembly_outputs);
        let status = run(
            cli,
            &["rail", "cache", "status", "--scope", "local", "-f", "json"],
            true,
        )?;
        anyhow::ensure!(status.status.success(), "assembly cache status: {status:?}");
        let status = json(&status)?;
        let usage = &status["status"]["installation"]["usage"];
        assert_eq!(usage["misses"], 3, "{status}");
        assert_eq!(usage["hits"], 3, "{status}");
        assert_eq!(usage["bypasses"], 1, "{status}");
        let finished = run(
            cli,
            &["rail", "cache", "report", "--finish", recording_path, "-f", "json"],
            true,
        )?;
        anyhow::ensure!(finished.status.success(), "Cranelift report finish: {finished:?}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
#[ignore = "requires GNU BFD and a C compiler; run actual linker-runtime qualification explicitly"]
fn elf_link_adapter_rejects_a_runtime_loaded_only_during_linking() {
    let result: Result<()> = (|| {
        let root = tempfile::tempdir()?;
        let source = root.path().join("native.c");
        let object = root.path().join("native.o");
        let observer_source = root.path().join("observer.c");
        let observer = root.path().join("observer.so");
        let plugin = root.path().join("extra.so");
        fs::write(&source, "unsigned native_value(void) { return 7; }\n")?;
        fs::write(
            &observer_source,
            r#"#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
__attribute__((constructor)) static void observe_link(void) {
    char argv[16384];
    int fd = open("/proc/self/cmdline", O_RDONLY);
    if (fd < 0) return;
    ssize_t len = read(fd, argv, sizeof(argv));
    close(fd);
    if (len <= 0 || !memmem(argv, (size_t)len, "--dependency-file=", 18)) return;
    const char *marker = getenv("RAIL_LINK_EXECUTIONS");
    if (marker) {
        FILE *file = fopen(marker, "a");
        if (file) { fputs("link execution\n", file); fclose(file); }
    }
    const char *plugin = getenv("RAIL_LINK_RUNTIME_PLUGIN");
    if (plugin) dlopen(plugin, RTLD_NOW | RTLD_LOCAL);
}
"#,
        )?;
        for arguments in [
            vec![
                "-fPIC".into(),
                "-c".into(),
                source.as_os_str().to_owned(),
                "-o".into(),
                object.as_os_str().to_owned(),
            ],
            vec![
                "-fPIC".into(),
                "-shared".into(),
                source.as_os_str().to_owned(),
                "-o".into(),
                plugin.as_os_str().to_owned(),
            ],
            vec![
                "-fPIC".into(),
                "-shared".into(),
                observer_source.as_os_str().to_owned(),
                "-ldl".into(),
                "-o".into(),
                observer.as_os_str().to_owned(),
            ],
        ] {
            let compiled = Command::new("cc").args(arguments).output()?;
            anyhow::ensure!(compiled.status.success(), "C fixture compilation: {compiled:?}");
        }
        let selected = Command::new("sh").args(["-c", "command -v ld.bfd"]).output()?;
        anyhow::ensure!(selected.status.success(), "BFD prerequisite: {selected:?}");
        let linker = PathBuf::from(String::from_utf8(selected.stdout)?.trim());
        for late_runtime in [false, true] {
            let attempt = tempfile::tempdir_in(root.path())?;
            let output = attempt.path().join("libnative.so");
            let marker = attempt.path().join("executions");
            let certificate = attempt.path().join("elf-linker-dependencies.d");
            let evidence = attempt.path().join("elf-linker-driver-inputs.json");
            let configure = |command: &mut Command| {
                command
                    .arg("-shared")
                    .arg(&object)
                    .arg("-o")
                    .arg(&output)
                    .env("LD_PRELOAD", &observer)
                    .env("RAIL_LINK_EXECUTIONS", &marker)
                    .env_remove("LD_DEBUG")
                    .env_remove("LD_DEBUG_OUTPUT")
                    .env_remove("LD_TRACE_LOADED_OBJECTS")
                    .env_remove("RAIL_LINK_RUNTIME_PLUGIN");
                if late_runtime {
                    command.env("RAIL_LINK_RUNTIME_PLUGIN", &plugin);
                }
            };
            let mut ordinary = Command::new(&linker);
            configure(&mut ordinary);
            let ordinary = ordinary.output()?;
            anyhow::ensure!(ordinary.status.success(), "ordinary link: {ordinary:?}");
            let bytes = fs::read(&output)?;
            let mode = fs::metadata(&output)?.permissions();
            fs::remove_file(&output)?;
            let mut adapter = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
            configure(&mut adapter);
            adapter
                .env("CARGO_RAIL_ELF_LINK_ADAPTER", "1")
                .env("CARGO_RAIL_ELF_LINK_DRIVER", &linker)
                .env("CARGO_RAIL_ELF_LINK_DEPENDENCIES", &certificate)
                .env("CARGO_RAIL_ELF_LINK_DRIVER_INPUTS", &evidence);
            let observed = adapter.output()?;
            assert_eq!(observed.status.code(), ordinary.status.code(), "{observed:?}");
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            assert_eq!(fs::read(&output)?, bytes);
            assert_eq!(fs::metadata(&output)?.permissions(), mode);
            assert_eq!(fs::read(&marker)?, b"link execution\n", "linker must execute once");
            let record: serde_json::Value = serde_json::from_slice(&fs::read(&evidence)?)?;
            assert_eq!(record["completed"], !late_runtime, "{record}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires mold, a C compiler, ar and Python; run the direct ELF linker contract explicitly"]
fn direct_mold_reuse_tracks_selected_libraries_and_tool_bytes() {
    elf_linker_reuse("mold", false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires GNU BFD, a C compiler, ar and Python; run the direct ELF linker contract explicitly"]
fn direct_bfd_reuse_tracks_selected_libraries_and_tool_bytes() {
    elf_linker_reuse("ld.bfd", false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires GNU gold, a C compiler, ar and Python; run the direct ELF linker contract explicitly"]
fn direct_gold_reuse_tracks_selected_libraries_and_tool_bytes() {
    elf_linker_reuse("ld.gold", false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires GCC, collect2, GNU ld, liblto_plugin, ar and Python; run the GCC linker contract explicitly"]
fn gcc_driver_reuse_tracks_selected_tools_and_plugin_bytes() {
    elf_linker_reuse("gcc", false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires GCC, collect2, GNU ld, liblto_plugin, ar and Python; run the default GCC contract explicitly"]
fn default_gcc_driver_reuse_tracks_selected_tools_and_plugin_bytes() {
    elf_linker_reuse("gcc", true);
}

#[cfg(target_os = "linux")]
fn elf_linker_reuse(linker_name: &str, default_gcc: bool) {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("direct-elf", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let coverage = tempfile::tempdir()?;
        fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
        let selected_linker = Command::new("sh")
            .args(["-c", "command -v \"$1\"", "select-linker", linker_name])
            .output()?;
        anyhow::ensure!(
            selected_linker.status.success(),
            "{linker_name} prerequisite: {selected_linker:?}"
        );
        let mut tool_bytes = fs::read(String::from_utf8(selected_linker.stdout)?.trim())?;
        tool_bytes.push(0);
        let linker = if linker_name == "gcc" {
            let bin = workspace.path.join("gcc-installation/bin");
            fs::create_dir_all(&bin)?;
            bin.join(if default_gcc { "cc" } else { "gcc" })
        } else {
            workspace.path.join(linker_name)
        };
        fs::write(&linker, &tool_bytes)?;
        fs::set_permissions(&linker, fs::Permissions::from_mode(0o700))?;
        let mut tools = vec![(linker.clone(), tool_bytes)];
        if linker_name == "gcc" {
            let query = |argument: &str| -> Result<String> {
                let output = Command::new("gcc").arg(argument).output()?;
                anyhow::ensure!(output.status.success(), "GCC {argument} prerequisite: {output:?}");
                Ok(String::from_utf8(output.stdout)?.trim().to_string())
            };
            let triple = query("-dumpmachine")?;
            let version = query("-dumpversion")?;
            let installation = workspace.path.join("gcc-installation");
            let library = installation.join("lib/gcc").join(&triple).join(&version);
            let linker_directory = installation.join(&triple).join("bin");
            fs::create_dir_all(&library)?;
            fs::create_dir_all(&linker_directory)?;
            for (name, query_argument, directory, mutate) in [
                ("collect2", "-print-prog-name=collect2", &library, true),
                ("ld", "-print-prog-name=ld", &linker_directory, true),
                ("liblto_plugin.so", "-print-file-name=liblto_plugin.so", &library, true),
                ("lto-wrapper", "-print-prog-name=lto-wrapper", &library, false),
                ("crtbeginS.o", "-print-file-name=crtbeginS.o", &library, false),
                ("crtendS.o", "-print-file-name=crtendS.o", &library, false),
            ] {
                let selection = query(query_argument)?;
                let source = if Path::new(&selection).is_absolute() {
                    PathBuf::from(selection)
                } else {
                    let resolved = Command::new("sh")
                        .args(["-c", "command -v \"$1\"", "select-gcc-tool", &selection])
                        .output()?;
                    anyhow::ensure!(resolved.status.success(), "resolve GCC {name}: {resolved:?}");
                    PathBuf::from(String::from_utf8(resolved.stdout)?.trim())
                };
                let mut bytes = fs::read(source)?;
                if mutate {
                    bytes.push(0);
                }
                let path = directory.join(name);
                fs::write(&path, &bytes)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                let selected = Command::new(&linker).arg(query_argument).output()?;
                anyhow::ensure!(selected.status.success(), "private GCC selection: {selected:?}");
                assert_eq!(
                    fs::canonicalize(String::from_utf8(selected.stdout)?.trim())?,
                    fs::canonicalize(&path)?,
                    "GCC must select the private {name} before its mutation is exercised"
                );
                if mutate {
                    tools.push((path, bytes));
                }
            }
        }
        let early = workspace.path.join("early");
        let selected = workspace.path.join("selected");
        fs::create_dir(&selected)?;
        let build_library = |directory: &Path, value: u32| -> Result<()> {
            fs::create_dir_all(directory)?;
            let source = directory.join("native.c");
            let object = directory.join("native.o");
            fs::write(
                &source,
                format!("unsigned cache_native_value(void) {{ return {value}; }}\n"),
            )?;
            let compiled = Command::new("cc")
                .args(["-fPIC", "-c"])
                .arg(&source)
                .arg("-o")
                .arg(&object)
                .output()?;
            anyhow::ensure!(compiled.status.success(), "C prerequisite: {compiled:?}");
            let archived = Command::new("ar")
                .arg("crs")
                .arg(directory.join("libcache_native.a"))
                .arg(&object)
                .output()?;
            anyhow::ensure!(archived.status.success(), "archive prerequisite: {archived:?}");
            Ok(())
        };
        build_library(&selected, 7)?;
        fs::write(
            workspace.path.join("Cargo.toml"),
            "[package]\nname = \"direct-elf\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [lib]\ncrate-type = [\"cdylib\"]\n[profile.dev]\npanic = \"abort\"\ndebug = 0\n",
        )?;
        fs::write(
            workspace.path.join("src/lib.rs"),
            "#![no_std]\n#[link(name = \"cache_native\", kind = \"static\")]\n\
             unsafe extern \"C\" { fn cache_native_value() -> u32; }\n\
             #[unsafe(no_mangle)] pub extern \"C\" fn cache_linker_value() -> u32 { unsafe { cache_native_value() } }\n\
             #[panic_handler] fn panic(_: &core::panic::PanicInfo<'_>) -> ! { loop {} }\n",
        )?;
        fs::create_dir(workspace.path.join(".cargo"))?;
        let mut flags = vec![
            format!("-Clinker-flavor={}", if linker_name == "gcc" { "gcc" } else { "ld" }),
            format!("-Lnative={}", early.display()),
            format!("-Lnative={}", selected.display()),
        ];
        if !default_gcc {
            flags.push(format!("-Clinker={}", linker.display()));
        }
        let inherited_path = std::env::var_os("PATH").context("toolchain PATH")?;
        let path = if default_gcc {
            std::env::join_paths(
                std::iter::once(linker.parent().context("private GCC bin directory")?.to_path_buf())
                    .chain(std::env::split_paths(&inherited_path)),
            )?
        } else {
            inherited_path
        };
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!("[build]\nrustflags = {}\n", serde_json::to_string(&flags)?),
        )?;
        let target = workspace.path.join("target");
        let compile = || -> Result<Output> {
            Ok(Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["build", "--offline", "--quiet"])
                .env("PATH", &path)
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_TARGET_DIR", &target)
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
                .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", coverage.path())
                .env_remove("CARGO_BUILD_TARGET")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("CARGO_RAIL_CACHE_REMOTE")
                .env_remove("CARGO_RAIL_CACHE_MODE")
                .env_remove("CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT")
                .env_remove("CARGO_RAIL_CACHE_REPORT")
                .output()?)
        };
        let baseline = compile()?;
        anyhow::ensure!(baseline.status.success(), "ordinary {linker_name} build: {baseline:?}");
        let mut expected_outputs = directory_snapshot(&target.join("debug/deps"))?;
        let expected_modes = expected_outputs
            .keys()
            .map(|path| {
                Ok((
                    path.clone(),
                    fs::metadata(target.join("debug/deps").join(path))?.permissions().mode(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        anyhow::ensure!(setup.status.success(), "{linker_name} cache setup: {setup:?}");
        let mut phases = vec![
            ("cold".to_string(), 1, 0, 7, None),
            ("warm".to_string(), 1, 1, 7, None),
            ("library changed".to_string(), 2, 1, 8, None),
            ("library warm".to_string(), 2, 2, 8, None),
        ];
        let (mut misses, mut hits) = (2, 2);
        for (index, (path, _)) in tools.iter().enumerate() {
            misses += 1;
            phases.push((
                format!("tool changed: {}", path.display()),
                misses,
                hits,
                8,
                Some(index),
            ));
            hits += 1;
            phases.push((format!("tool warm: {}", path.display()), misses, hits, 8, None));
        }
        phases.push(("earlier candidate".to_string(), misses + 1, hits, 9, None));
        phases.push(("earlier warm".to_string(), misses + 1, hits + 1, 9, None));
        for (phase, misses, hits, value, changed_tool) in phases {
            if phase == "library changed" {
                let length = fs::metadata(selected.join("libcache_native.a"))?.len();
                build_library(&selected, value)?;
                assert_eq!(fs::metadata(selected.join("libcache_native.a"))?.len(), length);
            } else if let Some(index) = changed_tool {
                let (path, bytes) = &mut tools[index];
                *bytes.last_mut().context("tool trailer")? = 1;
                fs::write(path, bytes)?;
            } else if phase == "earlier candidate" {
                build_library(&early, value)?;
            }
            fs::remove_dir_all(&target)?;
            let output = compile()?;
            assert_eq!(output.status.code(), baseline.status.code(), "{phase}: {output:?}");
            assert_eq!(output.stdout, baseline.stdout, "{phase} compiler stdout");
            assert_eq!(
                output.stderr,
                baseline.stderr,
                "{phase} compiler stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let status = selected_profile_status(&workspace.path, cargo_home.path())?;
            let usage = &status["status"]["installation"]["usage"];
            let events = directory_snapshot(coverage.path())?
                .values()
                .map(|bytes| serde_json::from_slice::<serde_json::Value>(bytes))
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(usage["misses"], misses, "{phase}: {status}; events: {events:?}");
            assert_eq!(usage["hits"], hits, "{phase}: {status}");
            let outputs = directory_snapshot(&target.join("debug/deps"))?;
            if matches!(phase.as_str(), "library changed" | "earlier candidate") {
                assert_ne!(outputs, expected_outputs, "{phase} must affect linked bytes");
                expected_outputs = outputs;
            } else {
                assert_eq!(outputs, expected_outputs, "{phase} exact compiler output inventory");
            }
            for (path, mode) in &expected_modes {
                assert_eq!(
                    fs::metadata(target.join("debug/deps").join(path))?.permissions().mode(),
                    *mode,
                    "{phase}"
                );
            }
            let executed = Command::new("python3")
                .args([
                    "-c",
                    "import ctypes, sys; print(ctypes.CDLL(sys.argv[1]).cache_linker_value())",
                ])
                .arg(target.join("debug/libdirect_elf.so"))
                .output()?;
            assert!(executed.status.success(), "{phase}: {executed:?}");
            assert_eq!(executed.stdout, format!("{value}\n").as_bytes(), "{phase}");
            assert!(executed.stderr.is_empty(), "{phase}: {executed:?}");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn cache_setup_preserves_the_implicit_linker_selected_from_path() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("path-linker", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let tools = tempfile::tempdir()?;
        let driver = tools.path().join("cc");
        let selected_log = tools.path().join("selected.log");
        let recording = tools.path().join("report.json");
        let report_path = recording.to_str().context("PATH linker report path")?;
        fs::write(
            &driver,
            "#!/bin/sh\nprintf 'selected cc\\n' >> \"$SELECTED_CC_LOG\"\nexec /usr/bin/cc \"$@\"\n",
        )?;
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o755))?;
        let inherited_path = std::env::var_os("PATH").context("toolchain PATH")?;
        let path = std::env::join_paths(
            std::iter::once(tools.path().to_path_buf()).chain(std::env::split_paths(&inherited_path)),
        )?;
        fs::remove_file(workspace.path.join("src/lib.rs"))?;
        fs::write(
            workspace.path.join("src/main.rs"),
            "fn main() { println!(\"selected linker result\"); }\n",
        )?;

        for phase in ["upstream", "installed"] {
            if phase == "installed" {
                let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
                assert!(setup.status.success(), "PATH linker cache setup failed: {setup:?}");
                fs::remove_dir_all(workspace.path.join("target"))?;
                let started = rail(
                    &workspace.path,
                    cargo_home.path(),
                    &["rail", "cache", "report", "--start", report_path],
                )?;
                assert!(started.status.success(), "PATH linker report start failed: {started:?}");
            }
            fs::write(&selected_log, b"")?;
            let mut command = Command::new("cargo");
            for (name, _) in std::env::vars_os() {
                if name.to_str().is_some_and(|name| {
                    name.starts_with("CARGO_RAIL_") || name.starts_with("CARGO_TARGET_") && name.ends_with("_LINKER")
                }) {
                    command.env_remove(name);
                }
            }
            command
                .current_dir(&workspace.path)
                .args(["build", "--offline", "--quiet"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_TARGET_DIR", workspace.path.join("target"))
                .env("PATH", &path)
                .env("SELECTED_CC_LOG", &selected_log)
                .env_remove("CARGO_BUILD_TARGET")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("OUT_DIR");
            if phase == "installed" {
                command.env("CARGO_RAIL_CACHE_REPORT", &recording);
            }
            let built = command.output()?;
            assert!(built.status.success(), "{phase} PATH linker build failed: {built:?}");
            assert_eq!(
                fs::read(&selected_log)?,
                b"selected cc\n",
                "{phase} build did not execute the selected PATH linker exactly once"
            );
            let output = Command::new(workspace.path.join("target/debug/path-linker")).output()?;
            assert!(output.status.success(), "{phase} executable failed: {output:?}");
            assert_eq!(output.stdout, b"selected linker result\n");
            assert!(output.stderr.is_empty());
        }
        let finished = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "report", "--finish", report_path, "-f", "json"],
        )?;
        assert!(
            finished.status.success(),
            "PATH linker report finish failed: {finished:?}"
        );
        let report = json(&finished)?;
        assert_eq!(
            report["measurements"]["bypass_reasons"]["default_linker_execution_evidence_unavailable"], 1,
            "custom PATH linker must execute normally without cache admission: {report}"
        );
        assert_eq!(report["measurements"]["misses"], 0, "{report}");
        assert_eq!(report["measurements"]["hits"], 0, "{report}");
        assert_eq!(report["measurements"]["failures"], 0, "{report}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn apple_link_adapter_retry_certifies_only_the_successful_attempt() {
    let result: Result<()> = (|| {
        let fixture = tempfile::tempdir()?;
        let directory = fs::canonicalize(fixture.path())?;
        let source = directory.join("main.c");
        let object = directory.join("main.o");
        let missing = directory.join("missing.o");
        let executable = directory.join("linked");
        let certificate = directory.join("apple-linker-dependencies.bin");
        let driver_inputs = directory.join("apple-linker-driver-inputs.json");
        fs::write(
            &source,
            "#include <stdio.h>\nint main(void) { puts(\"adapter retry result\"); return 0; }\n",
        )?;
        let compiled = Command::new("/usr/bin/cc")
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()?;
        assert!(
            compiled.status.success(),
            "adapter object prerequisite failed: {compiled:?}"
        );

        let adapter = |inputs: &[&Path]| -> Result<Output> {
            let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
            for (name, _) in std::env::vars_os() {
                if name.to_str().is_some_and(|name| name.starts_with("CARGO_RAIL_")) {
                    command.env_remove(name);
                }
            }
            command
                .current_dir(&directory)
                .args(inputs)
                .arg("-o")
                .arg(&executable)
                .env("CARGO_RAIL_APPLE_LINK_ADAPTER", "1")
                .env("CARGO_RAIL_APPLE_LINK_DRIVER", "/usr/bin/cc")
                .env("CARGO_RAIL_APPLE_LINK_CERTIFICATE", &certificate)
                .env("CARGO_RAIL_APPLE_LINK_DRIVER_INPUTS", &driver_inputs)
                .output()
                .context("run the real Apple linker adapter")
        };
        let ordinary_failure = Command::new("/usr/bin/cc")
            .arg(&missing)
            .arg(&object)
            .arg("-o")
            .arg(&executable)
            .output()?;
        assert!(!ordinary_failure.status.success());
        let failed = adapter(&[&missing, &object])?;
        assert_eq!(failed.status.code(), ordinary_failure.status.code(), "{failed:?}");
        assert_eq!(failed.stdout, ordinary_failure.stdout);
        assert_eq!(failed.stderr, ordinary_failure.stderr);
        assert!(!executable.exists());
        assert!(
            fs::read(&driver_inputs)?.is_empty(),
            "failed driver probing must not certify an invocation"
        );

        let completed = adapter(&[&object])?;
        assert!(completed.status.success(), "adapter retry failed: {completed:?}");
        assert!(completed.stdout.is_empty(), "{completed:?}");
        assert!(completed.stderr.is_empty(), "{completed:?}");
        let evidence: serde_json::Value = serde_json::from_slice(&fs::read(&driver_inputs)?)?;
        assert_eq!(evidence["completed"], true, "{evidence}");
        assert_eq!(evidence["direct_inputs"], serde_json::json!([object]));
        assert!(fs::metadata(&certificate)?.len() > 0);
        let output = Command::new(&executable).output()?;
        assert!(output.status.success(), "retried link output failed: {output:?}");
        assert_eq!(output.stdout, b"adapter retry result\n");
        assert!(output.stderr.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
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
        assert!(stderr.contains("just build"), "{stderr}");
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
        assert_eq!(status["status"]["schema_version"], 16);
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
        #[cfg(not(windows))]
        assert!(
            json(&status)?["status"]["installation"]["usage"]["hits"]
                .as_u64()
                .unwrap_or_default()
                >= 1
        );
        #[cfg(windows)]
        {
            let status = json(&status)?;
            let usage = &status["status"]["installation"]["usage"];
            assert_eq!(usage["hits"], 0);
            assert_eq!(usage["misses"], 0);
            assert_eq!(usage["failures"], 0);
            assert!(usage["bypasses"].as_u64().is_some_and(|count| count >= 2));
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn distributed_worker_revalidates_a_cross_target_library_before_local_admission() {
    let result: Result<()> = (|| {
        let target = "x86_64-unknown-linux-gnu";
        let workspace = TestWorkspace::new_single_crate("distributed_cross_target", "0.1.0")?;
        fs::write(workspace.path.join("src/lib.rs"), "pub const VALUE: u64 = 41;\n")?;
        let cargo_home = tempfile::tempdir()?;
        let coverage = tempfile::tempdir()?;
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
        let coverage = fs::canonicalize(coverage.path())?;
        let build = || {
            Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["build", "--offline", "--release", "--lib", "--target", target])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
                .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
                .env_remove("OUT_DIR")
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()
        };
        let upstream = build()?;
        anyhow::ensure!(
            upstream.status.success(),
            "uncached cross-target compilation failed: {upstream:?}"
        );
        let outputs = workspace.path.join("target").join(target).join("release/deps");
        let baseline = compiler_only_outputs(&outputs)?;
        anyhow::ensure!(baseline.len() == 2, "uncached metadata/rlib contract is incomplete");
        let setup = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "setup", "--distributed-local"],
        )?;
        anyhow::ensure!(setup.status.success(), "worker fixture setup failed: {setup:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let seeded = build()?;
        anyhow::ensure!(seeded.status.success(), "target environment seed failed: {seeded:?}");
        anyhow::ensure!(
            compiler_only_outputs(&outputs)? == baseline,
            "local capture changed uncached output bytes"
        );
        let cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        fs::remove_dir_all(workspace.path.join("target"))?;
        for entry in fs::read_dir(cache_root.join("native-actions-v2"))? {
            fs::remove_file(entry?.path())?;
        }
        for entry in fs::read_dir(&coverage)? {
            fs::remove_file(entry?.path())?;
        }
        let distributed = build()?;
        let events = coverage_events(&coverage)?;
        anyhow::ensure!(
            distributed.status.success(),
            "cross-target worker compilation failed: {distributed:?}"
        );
        anyhow::ensure!(
            events
                .iter()
                .any(|event| event["reason"] == "verified_distributed_execution"),
            "cross-target operation never crossed worker admission: {:?}; {}",
            events
                .iter()
                .map(|event| (&event["crate_name"], &event["status"], &event["reason"]))
                .collect::<Vec<_>>(),
            String::from_utf8_lossy(&distributed.stderr)
        );
        anyhow::ensure!(
            compiler_only_outputs(&outputs)? == baseline,
            "worker changed uncached target bytes"
        );
        fs::write(workspace.path.join("src/lib.rs"), "pub const VALUE: u64 = 42;\n")?;
        let changed = build()?;
        anyhow::ensure!(
            changed.status.success(),
            "changed target compilation failed: {changed:?}"
        );
        anyhow::ensure!(
            compiler_only_outputs(&outputs)? != baseline,
            "same-size input change reused stale worker output"
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
        #[cfg(not(windows))]
        assert!(
            seeded_events.iter().any(|event| {
                event["status"] == "miss"
                    && event["reason"].as_str().is_some_and(|reason| {
                        reason.starts_with("environment_selector_not_found;stored_verified_result")
                    })
            }),
            "ordinary Cargo did not establish local compiler-environment authority: {seeded_events:?}"
        );
        #[cfg(windows)]
        assert_native_driver_unavailable_bypass(&seeded_events, "distributed seed");
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
        #[cfg(not(windows))]
        assert!(
            events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution"),
            "ordinary Cargo never crossed the distributed admission boundary\nstderr:\n{}\nevents: {events:?}",
            String::from_utf8_lossy(&built.stderr)
        );
        #[cfg(windows)]
        assert_native_driver_unavailable_bypass(&events, "distributed rebuild");

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
                .windows(b"/cargo-rail/exec/v5".len())
                .any(|window| window == b"/cargo-rail/exec/v5"),
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
            #[cfg(not(windows))]
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
        for (reason, count) in &expected {
            if *count == 0 {
                continue;
            }
            assert!(
                human.contains(&format!("Cache failure {reason}: {count}")),
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
        assert_eq!(value["status"]["schema_version"], 16);
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
        #[cfg(not(windows))]
        assert!(
            seed_requests
                .iter()
                .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
            "automatic remote seed did not publish an entry: requests={seed_requests:?}, events={seed_events:?}"
        );
        #[cfg(windows)]
        {
            assert_native_driver_unavailable_bypass(&seed_events, "remote seed");
            assert!(
                !seed_requests
                    .iter()
                    .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
                "unobserved Windows compilation published a remote entry: {seed_requests:?}"
            );
        }

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
        #[cfg(not(windows))]
        assert!(
            imported_events
                .iter()
                .any(|event| event["status"] == "hit" && event["reason"] == "verified_remote_result"),
            "ordinary Cargo did not restore the setup-owned remote result: {imported_events:?}"
        );
        #[cfg(windows)]
        assert_native_driver_unavailable_bypass(&imported_events, "remote import");
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
fn pre_profile_receipt_is_refused_without_changing_installation_or_cache() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("pre-profile-refusal", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        anyhow::ensure!(setup.status.success(), "fixture setup failed: {setup:?}");
        let cache_root = selected_profile_cache_root(&workspace.path, cargo_home.path())?;
        let sentinel = cache_root.join("retained-user-data");
        fs::write(&sentinel, b"preserve exact bytes")?;
        let store = cargo_home.path().join("cargo-rail/cache-profiles-v1");
        let profile_path = fs::read_dir(store.join("profiles"))?
            .next()
            .transpose()?
            .context("fixture profile")?
            .path();
        let profile: serde_json::Value = serde_json::from_slice(&fs::read(profile_path)?)?;
        let receipt_path = cargo_home.path().join("cargo-rail/compiler-cache-v1/setup.json");
        let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt["version"] = serde_json::json!(3);
        receipt["cache"] = profile["cache"].clone();
        receipt["root_portability"] = profile["root_portability"].clone();
        let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
        fs::write(&receipt_path, &receipt_bytes)?;
        let config_path = cargo_home.path().join("config.toml");
        let config_before = fs::read(&config_path)?;
        fs::remove_dir_all(&store)?;
        for args in [
            vec!["rail", "cache", "setup", "--check"],
            vec!["rail", "cache", "setup"],
            vec!["rail", "cache", "uninstall"],
        ] {
            let refused = rail(&workspace.path, cargo_home.path(), &args)?;
            anyhow::ensure!(
                !refused.status.success(),
                "pre-profile receipt was adopted: {refused:?}"
            );
            let diagnostic = String::from_utf8_lossy(&refused.stderr);
            anyhow::ensure!(
                diagnostic.contains("unsupported compiler-cache installation receipt version 3")
                    && diagnostic.contains("originating release version is unknown"),
                "missing explicit transition: {diagnostic}"
            );
            anyhow::ensure!(
                fs::read(&receipt_path)? == receipt_bytes && fs::read(&config_path)? == config_before,
                "refusal changed installation authority"
            );
            anyhow::ensure!(
                fs::read(&sentinel)? == b"preserve exact bytes" && !store.exists(),
                "refusal changed retained data or created profile state"
            );
        }
        let compiled = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        anyhow::ensure!(
            compiled.status.success(),
            "old receipt prevented ordinary Cargo compilation: {compiled:?}"
        );
        anyhow::ensure!(
            fs::read(&receipt_path)? == receipt_bytes && !store.exists(),
            "compiler fallback adopted old installation state"
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

#[cfg(target_os = "macos")]
#[test]
fn cross_target_l2_reuse_preserves_physical_and_remapped_root_authority() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;
        let target = "x86_64-unknown-linux-gnu";
        for portability in ["physical", "remap"] {
            let first = TestWorkspace::new_single_crate("target-remote", "0.1.0")?;
            let second = TestWorkspace::new_single_crate("target-remote", "0.1.0")?;
            for workspace in [&first.path, &second.path] {
                fs::write(workspace.join("src/lib.rs"), "pub const VALUE: u64 = 41;\n")?;
            }
            let consumer = if portability == "physical" {
                &first.path
            } else {
                &second.path
            };
            let remote = LoopbackS3::start()?;
            let remote_url = remote.remote_url();
            let seed_home = tempfile::tempdir()?;
            let import_home = tempfile::tempdir()?;
            let seed_coverage = tempfile::tempdir()?;
            let import_coverage = tempfile::tempdir()?;
            for directory in [seed_coverage.path(), import_coverage.path()] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            }
            for (workspace, home, mode) in [
                (&first.path, seed_home.path(), "read-write"),
                (consumer, import_home.path(), "read"),
            ] {
                let setup = rail(
                    workspace,
                    home,
                    &[
                        "rail",
                        "cache",
                        "setup",
                        "--remote",
                        &remote_url,
                        "--remote-mode",
                        mode,
                        "--root-portability",
                        portability,
                    ],
                )?;
                anyhow::ensure!(setup.status.success(), "target L2 setup failed: {setup:?}");
            }
            let seeded = cargo_check_installed_remote_with_options(
                &first.path,
                seed_home.path(),
                seed_coverage.path(),
                None,
                None,
                Some(target),
            )?;
            anyhow::ensure!(seeded.status.success(), "target L2 seed failed: {seeded:?}");
            let baseline = compiler_only_outputs(&first.path.join("target").join(target).join("debug/deps"))?;
            anyhow::ensure!(baseline.len() == 1, "target metadata seed contract incomplete");
            if consumer == &first.path {
                fs::remove_dir_all(first.path.join("target"))?;
            }
            let writes = remote.requests().iter().filter(|(method, _)| method == "PUT").count();
            let imported = cargo_check_installed_remote_with_options(
                consumer,
                import_home.path(),
                import_coverage.path(),
                None,
                None,
                Some(target),
            )?;
            let events = coverage_events(import_coverage.path())?;
            anyhow::ensure!(imported.status.success(), "target L2 import failed: {imported:?}");
            anyhow::ensure!(
                events.iter().any(|event| event["status"] == "hit"
                    && event["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.starts_with("verified_remote_result"))),
                "{portability} target result did not cross L2 verification: {:?}; {}",
                events
                    .iter()
                    .map(|event| (&event["crate_name"], &event["status"], &event["reason"]))
                    .collect::<Vec<_>>(),
                String::from_utf8_lossy(&imported.stderr)
            );
            anyhow::ensure!(
                compiler_only_outputs(&consumer.join("target").join(target).join("debug/deps"))? == baseline,
                "{portability} L2 restore changed target metadata bytes"
            );
            anyhow::ensure!(
                remote.requests().iter().filter(|(method, _)| method == "PUT").count() == writes,
                "read-only target import wrote remote objects"
            );
            fs::write(consumer.join("src/lib.rs"), "pub const VALUE: u64 = 42;\n")?;
            let mutated = cargo_check_installed_remote_with_options(
                consumer,
                import_home.path(),
                import_coverage.path(),
                None,
                None,
                Some(target),
            )?;
            anyhow::ensure!(mutated.status.success(), "target L2 input mutation failed: {mutated:?}");
            anyhow::ensure!(
                compiler_only_outputs(&consumer.join("target").join(target).join("debug/deps"))? != baseline,
                "same-size input mutation restored stale target metadata"
            );
        }
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
        #[cfg(windows)]
        {
            assert_native_driver_unavailable_bypass(&seed_events, "remapped seed");
            assert!(
                !remote
                    .requests()
                    .iter()
                    .any(|(method, path)| method == "PUT" && path.contains("/entries/")),
                "unobserved remapped compilation published a remote entry"
            );
        }
        let writes_before_consumer = remote.requests().iter().filter(|(method, _)| method == "PUT").count();
        let restored = cargo_check_installed_remote_in_target(
            &second.path,
            second_home.path(),
            second_coverage.path(),
            &consumer_target,
        )?;
        assert!(restored.status.success(), "portable restore failed: {restored:?}");
        let events = coverage_events(second_coverage.path())?;
        #[cfg(not(windows))]
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
        #[cfg(windows)]
        assert_native_driver_unavailable_bypass(&events, "remapped consumer");
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
        #[cfg(not(windows))]
        assert!(
            mutated_events.iter().any(|event| event["status"] == "miss")
                && mutated_events.iter().all(|event| event["status"] != "hit"),
            "same-size selected-input mutation did not produce a clean miss: stderr={}, events={mutated_events:?}",
            String::from_utf8_lossy(&mutated.stderr)
        );
        #[cfg(windows)]
        assert_native_driver_unavailable_bypass(&mutated_events, "remapped mutation");
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
        let source_path = workspace.path.join("src/lib.rs");
        let original_source = fs::read_to_string(&source_path)?;
        let mut credential_source = original_source.clone();
        for name in [
            "CARGO_RAIL_CACHE_REMOTE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_CONFIG_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
        ] {
            credential_source.push_str(&format!(
                "\nconst _: () = assert!(option_env!(\"{name}\").is_none(), \"compiler inherited {name}\");\n"
            ));
        }
        fs::write(&source_path, credential_source)?;
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
            .filter(|line| line.contains("--crate-name transparent_remote") || line.ends_with("args=--print=sysroot"))
            .collect::<Vec<_>>();
        assert!(
            !controlled_compilers.is_empty()
                && controlled_compilers.iter().all(|line| {
                    line.starts_with("url=unset access=unset secret=unset token=unset config=unset args=")
                }),
            "remote authority entered a Cargo-Rail-controlled compiler subprocess: {compiler_environments:?}"
        );
        fs::write(source_path, original_source)?;
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
        let hit = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
        assert!(hit.status.success(), "verified hit failed: {hit:?}");

        fs::remove_dir_all(workspace.path.join("target"))?;
        let replaced = Command::new("cargo")
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
            !replaced.status.success(),
            "cache skipped the selected compiler: {replaced:?}"
        );
        assert!(String::from_utf8_lossy(&replaced.stderr).contains("exit status: 91"));

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
fn selected_compiler_wrapper_preserves_its_flags_after_cache_setup() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("selected-compiler-wrapper", "0.1.0")?;
        fs::write(
            workspace.path.join("src/lib.rs"),
            "#[cfg(not(wrapper_authorized))]\ncompile_error!(\"selected compiler flag was lost\");\npub fn value() -> u8 { 7 }\n",
        )?;
        let probe = tempfile::tempdir()?;
        let compiler = probe.path().join("selected-rustc");
        fs::write(&compiler, "#!/bin/sh\nexec rustc \"$@\" --cfg wrapper_authorized\n")?;
        fs::set_permissions(&compiler, fs::Permissions::from_mode(0o700))?;
        let cargo_home = tempfile::tempdir()?;
        let run = || {
            Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["check", "--quiet"])
                .env("CARGO_HOME", cargo_home.path())
                .env("RUSTC", &compiler)
                .env("CARGO_INCREMENTAL", "0")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .output()
        };
        let uncached = run()?;
        assert!(uncached.status.success(), "uncached compiler failed: {uncached:?}");
        let outputs = compiler_only_outputs(&workspace.path.join("target/debug/deps"))?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "cache setup failed: {setup:?}");
        fs::remove_dir_all(workspace.path.join("target"))?;
        let installed = run()?;
        assert!(
            installed.status.success(),
            "cache changed selected compiler behavior: {installed:?}"
        );
        assert_eq!(installed.stdout, uncached.stdout);
        assert_eq!(installed.stderr, uncached.stderr);
        assert_eq!(
            compiler_only_outputs(&workspace.path.join("target/debug/deps"))?,
            outputs
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

        let wrapper_probe = tempfile::tempdir()?;
        let wrapper_log = wrapper_probe.path().join("wrapper.log");
        let run = |wrapper: Option<&Path>| -> Result<std::process::Output> {
            let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
            command
                .current_dir(&workspace.path)
                .args(["rail", "unify", "--check"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER");
            if let Some(wrapper) = wrapper {
                command
                    .env("RUSTC_WORKSPACE_WRAPPER", wrapper)
                    .env("CARGO_RAIL_TEST_WORKSPACE_WRAPPER_LOG", &wrapper_log);
            }
            Ok(command.output()?)
        };

        let cold = run(None)?;
        assert_eq!(cold.status.code(), Some(1), "cold analysis failed: {cold:?}");
        let usage = || -> Result<serde_json::Value> {
            let status = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "status", "--scope", "local", "-f", "json"],
            )?;
            assert!(status.status.success(), "cache status failed: {status:?}");
            Ok(json(&status)?["status"]["installation"]["usage"].clone())
        };
        let cold_usage = usage()?;
        assert!(cold_usage["misses"].as_u64().unwrap() >= 1, "{cold_usage}");
        assert_eq!(cold_usage["hits"], 0, "{cold_usage}");
        assert!(String::from_utf8_lossy(&cold.stdout).contains("Dependencies: helper"));

        let warm = run(None)?;
        assert_eq!(warm.status.code(), Some(1), "warm analysis failed: {warm:?}");
        assert!(
            String::from_utf8_lossy(&warm.stdout).contains("Dependencies: helper"),
            "warm analysis lost exact diagnostic evidence: {warm:?}"
        );
        let warm_usage = usage()?;
        assert!(warm_usage["hits"].as_u64().unwrap() >= 1, "{warm_usage}");
        assert_eq!(warm_usage["misses"], cold_usage["misses"], "{warm_usage}");
        fs::write(workspace.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
        let changed = run(None)?;
        assert_eq!(changed.status.code(), Some(1), "changed analysis failed: {changed:?}");
        assert!(String::from_utf8_lossy(&changed.stdout).contains("Dependencies: helper"));
        let changed_usage = usage()?;
        assert!(
            changed_usage["misses"].as_u64().unwrap() > warm_usage["misses"].as_u64().unwrap(),
            "{changed_usage}"
        );
        fs::write(
            workspace.path.join("src/lib.rs"),
            "pub fn used() { helper::helper(); }\n",
        )?;
        let used = run(None)?;
        assert!(used.status.success(), "used dependency analysis failed: {used:?}");
        assert!(!String::from_utf8_lossy(&used.stdout).contains("Dependencies: helper"));
        use std::os::unix::fs::PermissionsExt as _;
        let wrapper = wrapper_probe.path().join("workspace-wrapper");
        fs::write(
            &wrapper,
            "#!/bin/sh\nprintf '%s\n' \"$*\" >> \"$CARGO_RAIL_TEST_WORKSPACE_WRAPPER_LOG\"\nexec \"$@\"\n",
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        fs::write(workspace.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
        let wrapped = run(Some(&wrapper))?;
        assert_eq!(wrapped.status.code(), Some(1), "wrapped analysis failed: {wrapped:?}");
        assert!(String::from_utf8_lossy(&wrapped.stdout).contains("Dependencies: helper"));
        assert!(fs::read_to_string(&wrapper_log)?.contains("--crate-name bound_analysis_cache"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn remote_diagnostics_reuse_revalidates_source_inputs() {
    let result: Result<()> = (|| {
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
        let cargo_home = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "remote fixture setup: {setup:?}");

        let run = |cache_root: &Path, mode: &str| -> Result<Output> {
            Ok(Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
                .current_dir(&workspace.path)
                .args(["rail", "--diagnostics-file"])
                .arg(cache_root.join("diagnostics.json"))
                .args(["unify", "--check"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_RAIL_CACHE_DIR", cache_root)
                .env("CARGO_INCREMENTAL", "0")
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

        let seed_cache = tempfile::tempdir()?;
        let seed = run(seed_cache.path(), "read-write")?;
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

        let import_cache = tempfile::tempdir()?;
        let requests_before_import = remote.request_count();
        let imported = run(import_cache.path(), "read")?;
        assert_eq!(
            imported.status.code(),
            Some(1),
            "remote analysis import failed: {imported:?}"
        );
        assert!(
            String::from_utf8_lossy(&imported.stdout).contains("Dependencies: helper"),
            "remote analysis import lost exact fact evidence: {imported:?}"
        );
        let imported_counters: serde_json::Value =
            serde_json::from_slice(&fs::read(import_cache.path().join("diagnostics.json"))?)?;
        assert_eq!(
            imported_counters["compiler_acquisition"]["cargo_views"], 0,
            "unchanged remote diagnostics must avoid Cargo acquisition: {imported_counters}; {imported:?}"
        );
        assert_eq!(imported_counters["compiler_acquisition"]["compiler_actions"], 0);
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
        assert!(
            import_requests[requests_before_import..]
                .iter()
                .all(|(method, _)| method != "PUT"),
            "read-only diagnostics import published remote data"
        );

        fs::remove_dir_all(workspace.path.join("target"))?;
        let cold_cache = tempfile::tempdir()?;
        fs::write(workspace.path.join("src/lib.rs"), "pub fn changed() -> u8 { 7 }\n")?;
        let cold = run(cold_cache.path(), "read")?;
        assert_eq!(cold.status.code(), Some(1), "changed-input acquisition: {cold:?}");
        assert!(String::from_utf8_lossy(&cold.stdout).contains("Dependencies: helper"));
        let cold_counters: serde_json::Value =
            serde_json::from_slice(&fs::read(cold_cache.path().join("diagnostics.json"))?)?;
        assert_eq!(
            cold_counters["compiler_acquisition"]["cargo_views"], 1,
            "{cold_counters}"
        );
        assert!(
            cold_counters["compiler_acquisition"]["compiler_actions"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "source change must execute the compiler: {cold_counters}"
        );
        let removed = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "uninstall"])?;
        assert!(removed.status.success(), "stop fixture coordinator: {removed:?}");
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
        #[cfg(windows)]
        let mut producer_bypassed = false;
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
            #[cfg(windows)]
            if crate_name == Some("fixture_macros") {
                assert_eq!(event["status"], "bypassed");
                assert_eq!(event["reason"], "compiler_native_input_driver_unavailable");
                producer_bypassed = true;
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
        #[cfg(not(windows))]
        assert!(
            producer_cached,
            "proc-macro producer did not enter verified L1: {event_summary:?}"
        );
        #[cfg(windows)]
        assert!(
            producer_bypassed && !producer_cached,
            "proc-macro producer did not bypass unavailable native capture: {event_summary:?}"
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

        let lanes: &[(&str, &[&str])] = &[
            ("check", &["check", "--quiet"]),
            ("build", &["build", "--quiet"]),
            ("test", &["test", "--quiet"]),
            ("run", &["run", "--quiet"]),
            ("bench", &["bench", "--quiet", "--no-run"]),
            ("nextest", &["nextest", "run", "--profile", "default"]),
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
            let coverage = tempfile::tempdir()?;
            fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
            let coverage_path = fs::canonicalize(coverage.path())?;
            let reused = Command::new("cargo")
                .current_dir(&workspace.path)
                .args(*arguments)
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
                .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage_path)
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?;
            assert!(
                reused.status.success(),
                "{name} executed an eligible library compiler instead of restoring it: {reused:?}"
            );
            let events = coverage_events(&coverage_path)?;
            let libraries = events
                .iter()
                .filter(|event| {
                    event["action"]["crate_name"] == "transparent_shapes"
                        && event["action"]["action_class"] == "rust_library"
                })
                .collect::<Vec<_>>();
            assert!(
                !libraries.is_empty(),
                "{name} did not observe library reuse: {events:?}"
            );
            assert!(
                libraries.iter().all(|event| event["status"] == "hit"),
                "{name}: {libraries:?}"
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

#[test]
fn cache_reporting_intervals_capture_cold_and_warm_production_outcomes() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("cache-reporting", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let reports = tempfile::tempdir()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "{setup:?}");
        let mut measurements = Vec::new();
        for name in ["cold", "warm"] {
            let recording = reports.path().join(format!("{name}.json"));
            let path = recording.to_str().context("report path")?;
            let started = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", path, "-f", "json"],
            )?;
            assert!(started.status.success(), "{started:?}");
            if name == "warm" {
                fs::remove_dir_all(workspace.path.join("target"))?;
            }
            let compiled = Command::new("cargo")
                .current_dir(&workspace.path)
                .args(["check", "--quiet"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_RAIL_CACHE_REPORT", &recording)
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .output()?;
            assert!(compiled.status.success(), "{compiled:?}");
            let finished = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", path, "-f", "json"],
            )?;
            assert!(finished.status.success(), "{finished:?}");
            let output = json(&finished)?;
            let schema: serde_json::Value =
                serde_json::from_str(include_str!("../../schemas/cache-report-v1.schema.json"))?;
            assert!(
                jsonschema::validator_for(&schema)
                    .map_err(|error| anyhow::anyhow!("invalid report schema: {error}"))?
                    .is_valid(&output)
            );
            assert_eq!(output["measurements"]["incomplete"], false, "{output}");
            assert!(fs::metadata(&recording)?.len() < 4096);
            measurements.push(output["measurements"].clone());
        }
        #[cfg(not(windows))]
        {
            assert!(
                measurements[0]["misses"].as_u64().unwrap_or_default() >= 1,
                "cold: {}",
                measurements[0]
            );
            assert!(
                measurements[1]["hits"].as_u64().unwrap_or_default() >= 1,
                "warm: {}",
                measurements[1]
            );
        }
        #[cfg(windows)]
        for measurement in &measurements {
            assert_eq!(measurement["hits"], 0);
            assert_eq!(measurement["misses"], 0);
            assert_eq!(measurement["failures"], 0);
            assert_eq!(
                measurement["bypass_reasons"]["compiler_native_input_driver_unavailable"],
                1
            );
        }
        assert_eq!(measurements[1]["misses"], 0, "report path changed cache identity");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn cross_windows_link_restores_the_exact_dll_pdb_and_import_library() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("coff-outputs", "0.1.0")?;
        let cargo_home = tempfile::tempdir()?;
        let reports = tempfile::tempdir()?;
        fs::write(
            workspace.path.join("Cargo.toml"),
            "[package]\nname = \"coff-outputs\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n",
        )?;
        let source = workspace.path.join("src/lib.rs");
        fs::write(
            &source,
            "#[unsafe(no_mangle)] pub extern \"C\" fn answer() -> u32 { 42 }\n",
        )?;
        let target = "x86_64-pc-windows-msvc";
        let target_directory = workspace.path.join("target").join(target);
        let build = |recording: Option<&Path>| -> Result<()> {
            let mut command = Command::new("cargo");
            command
                .current_dir(&workspace.path)
                .args(["xwin", "build", "--offline", "--quiet", "--target", target])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env("SOURCE_DATE_EPOCH", "0")
                .env("CARGO_TARGET_DIR", workspace.path.join("target"))
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("CARGO_RAIL_CACHE")
                .env_remove("CARGO_RAIL_CACHE_REPORT")
                .env_remove("LLD_REPRODUCE");
            if let Some(recording) = recording {
                command.env("CARGO_RAIL_CACHE_REPORT", recording);
            }
            let output = command
                .output()
                .context("run cargo-xwin using the already provisioned MSVC SDK")?;
            anyhow::ensure!(
                output.status.success(),
                "cross-Windows Cargo build failed; provision cargo-xwin, lld-link and its cached MSVC SDK before this lane: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        };
        let outputs = || -> Result<BTreeMap<PathBuf, (Vec<u8>, u32)>> {
            let mut result = BTreeMap::new();
            for entry in fs::read_dir(target_directory.join("debug/deps"))? {
                let entry = entry?;
                let path = entry.path();
                if matches!(
                    path.extension().and_then(std::ffi::OsStr::to_str),
                    Some("dll" | "pdb" | "lib")
                ) {
                    result.insert(
                        PathBuf::from(entry.file_name()),
                        (fs::read(&path)?, fs::metadata(&path)?.permissions().mode() & 0o777),
                    );
                }
            }
            anyhow::ensure!(
                result.len() == 3,
                "expected exactly the DLL, PDB and import library, observed {:?}",
                result.keys().collect::<Vec<_>>()
            );
            Ok(result)
        };
        build(None).context("uncached upstream Windows link must succeed before cache qualification")?;
        let baseline = outputs()?;
        assert!(
            baseline
                .keys()
                .any(|path| path.extension().is_some_and(|extension| extension == "pdb"))
        );
        assert!(baseline.keys().any(|path| path.to_string_lossy().ends_with(".dll.lib")));
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "COFF cache setup failed: {setup:?}");
        let mut cold_outputs = None;
        for (phase, hits, misses) in [("cold", 0, 1), ("warm", 1, 0), ("source-change", 0, 1)] {
            if phase == "source-change" {
                fs::write(
                    &source,
                    "#[unsafe(no_mangle)] pub extern \"C\" fn answer() -> u32 { 43 }\n",
                )?;
            }
            fs::remove_dir_all(&target_directory)?;
            let recording = reports.path().join(format!("{phase}.json"));
            let path = recording.to_str().context("COFF report path")?;
            let start = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", path],
            )?;
            assert!(start.status.success(), "{phase} recording failed: {start:?}");
            build(Some(&recording))?;
            let finish = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", path, "-f", "json"],
            )?;
            assert!(finish.status.success(), "{phase} reporting failed: {finish:?}");
            let report = json(&finish)?;
            assert_eq!(report["measurements"]["hits"], hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["misses"], misses, "{phase}: {report}");
            assert!(
                report["measurements"]["bypass_reasons"]
                    .as_object()
                    .context("COFF bypass reasons")?
                    .keys()
                    .all(|reason| matches!(
                        reason.as_str(),
                        "compiler_information_request" | "compiler_stdin_observation_unavailable"
                    )),
                "{phase} bypassed a compiler action: {report}"
            );
            assert_eq!(report["measurements"]["failures"], 0, "{phase}: {report}");
            let current = outputs()?;
            if phase == "cold" {
                assert_eq!(
                    current.keys().collect::<Vec<_>>(),
                    baseline.keys().collect::<Vec<_>>(),
                    "instrumentation changed the ordinary output set"
                );
                for (name, (_, mode)) in &baseline {
                    assert_eq!(
                        current.get(name).map(|(_, current_mode)| current_mode),
                        Some(mode),
                        "instrumentation changed mode for {}",
                        name.display()
                    );
                    if name.to_string_lossy().ends_with(".dll.lib") {
                        assert_eq!(
                            current.get(name),
                            baseline.get(name),
                            "instrumentation changed the deterministic import library"
                        );
                    }
                }
                cold_outputs = Some(current);
            } else if phase == "warm" {
                assert_eq!(
                    Some(current),
                    cold_outputs,
                    "warm restore changed COFF output bytes or modes"
                );
            } else {
                assert_ne!(
                    Some(current),
                    cold_outputs,
                    "same-size source mutation did not change linked outputs"
                );
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn packed_apple_debug_outputs_run_the_compiler_until_the_complete_tree_is_owned() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("apple-debug-outputs", "0.1.0")?;
        let manifest = workspace.path.join("Cargo.toml");
        fs::write(
            &manifest,
            format!(
                "{}\n[profile.dev]\nsplit-debuginfo = \"packed\"\n",
                fs::read_to_string(&manifest)?
            ),
        )?;
        fs::remove_file(workspace.path.join("src/lib.rs"))?;
        fs::write(
            workspace.path.join("src/main.rs"),
            "fn main() { println!(\"debug outputs\"); }\n",
        )?;
        let version = Command::new("rustc").arg("-vV").output()?;
        assert!(version.status.success(), "rustc prerequisite failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host")?;
        let cargo_home = tempfile::tempdir()?;
        let reports = tempfile::tempdir()?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)?;
        let target = workspace.path.join("target").join(host);
        let baseline = directory_snapshot(&target.join("debug/deps"))?;
        let auxiliary_names = baseline
            .keys()
            .filter(|path| {
                path.extension().is_some_and(|extension| extension == "o")
                    || path
                        .components()
                        .any(|part| part.as_os_str().to_string_lossy().ends_with(".dSYM"))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            !auxiliary_names.is_empty(),
            "upstream packed debug build emitted no separate debug artifacts: {:?}",
            baseline.keys().collect::<Vec<_>>()
        );
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "debug cache setup failed: {setup:?}");
        fs::remove_dir_all(&target)?;
        let recording = reports.path().join("debug.json");
        let path = recording.to_str().context("debug report path")?;
        let start = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "report", "--start", path],
        )?;
        assert!(start.status.success(), "debug report start failed: {start:?}");
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, Some(&recording))?;
        let finish = rail(
            &workspace.path,
            cargo_home.path(),
            &["rail", "cache", "report", "--finish", path, "-f", "json"],
        )?;
        assert!(finish.status.success(), "debug report finish failed: {finish:?}");
        let report = json(&finish)?;
        assert_eq!(report["measurements"]["hits"], 0, "{report}");
        assert_eq!(report["measurements"]["misses"], 0, "{report}");
        assert!(
            report["measurements"]["bypass_reasons"]
                .as_object()
                .context("debug bypass reasons")?
                .keys()
                .all(|reason| matches!(
                    reason.as_str(),
                    "compiler_information_request"
                        | "compiler_stdin_observation_unavailable"
                        | "compiler_debug_output_evidence_unavailable"
                )),
            "unexpected debug bypass: {report}"
        );
        assert_eq!(
            report["measurements"]["bypass_reasons"]["compiler_debug_output_evidence_unavailable"], 1,
            "{report}"
        );
        let current = directory_snapshot(&target.join("debug/deps"))?;
        for name in auxiliary_names {
            assert!(
                current.get(&name).is_some_and(|bytes| !bytes.is_empty()),
                "ordinary compiler lost debug output {}",
                name.display()
            );
        }
        let output = Command::new(target.join("debug/apple-debug-outputs")).output()?;
        assert!(output.status.success(), "debug executable failed: {output:?}");
        assert_eq!(output.stdout, b"debug outputs\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn coff_adapter_preserves_exact_outputs_with_windows_response_arguments() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = tempfile::tempdir()?;
        let directory = fs::canonicalize(fixture.path())?.join("COFF response fixture");
        fs::create_dir(&directory)?;
        let driver = std::env::split_paths(&std::env::var_os("PATH").context("linker PATH")?)
            .map(|directory| directory.join("lld-link"))
            .find(|path| path.is_file())
            .context("lld-link must be provisioned before this explicit local contract")?;
        let source = directory.join("source.c");
        let object = directory.join("source object.obj");
        fs::write(&source, "__declspec(dllexport) int answer(void) { return 42; }\n")?;
        let compiled = Command::new("clang")
            .args(["--target=x86_64-pc-windows-msvc", "-gcodeview", "-c"])
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()?;
        assert!(
            compiled.status.success(),
            "COFF object prerequisite failed: {compiled:?}"
        );
        let dll = directory.join("fixture.dll");
        let pdb = directory.join("fixture.pdb");
        let library = directory.join("fixture.dll.lib");
        let response = directory.join("original arguments.rsp");
        fs::write(
            &response,
            format!(
                "/dll\n/noentry\n/debug\n/out:\"{}\"\n/pdb:\"{}\"\n/implib:\"{}\"\n\"{}\"\n",
                dll.display(),
                pdb.display(),
                library.display(),
                object.display()
            ),
        )?;
        let argument = format!("@{}", response.display());
        let baseline = Command::new(&driver)
            .arg(&argument)
            .current_dir(&directory)
            .env("SOURCE_DATE_EPOCH", "0")
            .env_remove("LLD_REPRODUCE")
            .env_remove("LINK")
            .env_remove("_LINK_")
            .output()?;
        assert!(baseline.status.success(), "ordinary COFF linker failed: {baseline:?}");
        let mut expected = BTreeMap::new();
        for path in [&dll, &pdb, &library] {
            expected.insert(
                path.clone(),
                (fs::read(path)?, fs::metadata(path)?.permissions().mode() & 0o777),
            );
            fs::remove_file(path)?;
        }
        let evidence = directory.join("coff-linker-driver-inputs.json");
        let archive = directory.join("coff-linker-inputs.tar");
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|name| name.starts_with("CARGO_RAIL_")) {
                command.env_remove(name);
            }
        }
        let observed = command
            .arg(argument)
            .current_dir(&directory)
            .env("SOURCE_DATE_EPOCH", "0")
            .env("CARGO_RAIL_COFF_LINK_ADAPTER", "1")
            .env("CARGO_RAIL_COFF_LINK_DRIVER", &driver)
            .env("CARGO_RAIL_COFF_LINK_ARCHIVE", &archive)
            .env("CARGO_RAIL_COFF_LINK_EVIDENCE", &evidence)
            .env_remove("LLD_REPRODUCE")
            .env_remove("LINK")
            .env_remove("_LINK_")
            .output()?;
        assert_eq!(observed.status.code(), baseline.status.code(), "{observed:?}");
        assert_eq!(observed.stdout, baseline.stdout);
        assert_eq!(observed.stderr, baseline.stderr);
        for (path, (bytes, mode)) in expected {
            assert!(fs::read(&path)? == bytes, "adapter changed {}", path.display());
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, mode);
        }
        let evidence: serde_json::Value = serde_json::from_slice(&fs::read(evidence)?)?;
        assert_eq!(evidence["completed"], true, "{evidence}");
        assert_eq!(
            evidence["response_files"].as_array().map(Vec::len),
            Some(1),
            "{evidence}"
        );
        assert!(fs::metadata(archive)?.len() > fs::metadata(object)?.len());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn unpacked_debug_objects_restore_exactly_and_remain_bound_to_the_output_directory() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let workspace = TestWorkspace::new_single_crate("unpacked-debug", "0.1.0")?;
        let manifest = workspace.path.join("Cargo.toml");
        fs::write(
            &manifest,
            format!(
                "{}\n[profile.dev]\nsplit-debuginfo = \"unpacked\"\n",
                fs::read_to_string(&manifest)?
            ),
        )?;
        fs::write(workspace.path.join("src/lib.rs"), "pub fn answer() -> u64 { 41 }\n")?;
        let version = Command::new("rustc").arg("-vV").output()?;
        assert!(version.status.success(), "rustc prerequisite failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host")?;
        let cargo_home = tempfile::tempdir()?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)?;
        let target = workspace.path.join("target").join(host);
        let outputs = target.join("debug/deps");
        let baseline = directory_snapshot(&outputs)?;
        let objects = baseline
            .keys()
            .filter(|name| name.to_string_lossy().ends_with(".rcgu.o"))
            .collect::<Vec<_>>();
        assert_eq!(
            objects.len(),
            1,
            "upstream separate object inventory: {:?}",
            baseline.keys()
        );
        let modes = baseline
            .keys()
            .map(|name| {
                Ok((
                    name.clone(),
                    fs::metadata(outputs.join(name))?.permissions().mode() & 0o777,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "unpacked cache setup failed: {setup:?}");
        let reports = tempfile::tempdir()?;
        for (phase, hits) in [("cold", 0), ("warm", 1)] {
            fs::remove_dir_all(&target)?;
            let recording = reports.path().join(format!("{phase}.json"));
            let path = recording.to_str().context("report path")?;
            let start = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", path],
            )?;
            assert!(start.status.success(), "report start failed: {start:?}");
            explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, Some(&recording))?;
            let finish = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", path, "-f", "json"],
            )?;
            assert!(finish.status.success(), "report finish failed: {finish:?}");
            let report = json(&finish)?;
            assert_eq!(report["measurements"]["hits"], hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["misses"], 1 - hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["failures"], 0, "{phase}: {report}");
            assert_eq!(
                directory_snapshot(&outputs)?,
                baseline,
                "{phase} changed or omitted an upstream compiler output"
            );
            for (name, mode) in &modes {
                assert_eq!(
                    fs::metadata(outputs.join(name))?.permissions().mode() & 0o777,
                    *mode,
                    "{phase} mode of {}",
                    name.display()
                );
            }
        }
        let moved_target = workspace.path.join("target/moved");
        let moved = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["build", "--offline", "--quiet", "--target", host])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_TARGET_DIR", &moved_target)
            .env("CARGO_INCREMENTAL", "0")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .output()?;
        assert!(moved.status.success(), "moved unpacked build failed: {moved:?}");
        let moved_status = selected_profile_status(&workspace.path, cargo_home.path())?;
        let usage = &moved_status["status"]["installation"]["usage"];
        assert_eq!(
            usage["hits"], 1,
            "separate debug objects crossed output directories: {usage}"
        );
        assert_eq!(
            usage["misses"], 2,
            "moved debug build did not compile its own objects: {usage}"
        );
        assert_eq!(
            directory_snapshot(&outputs)?,
            baseline,
            "moved build changed the original debug objects"
        );
        fs::remove_dir_all(&target)?;
        fs::write(workspace.path.join("src/lib.rs"), "pub fn answer() -> u64 { 42 }\n")?;
        explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)?;
        let changed = selected_profile_status(&workspace.path, cargo_home.path())?;
        let usage = &changed["status"]["installation"]["usage"];
        assert_eq!(usage["hits"], 1, "changed source restored stale debug objects: {usage}");
        assert_eq!(usage["misses"], 3, "changed source did not cause a miss: {usage}");
        assert_ne!(
            fs::read(outputs.join(objects[0]))?,
            baseline[objects[0]],
            "changed code retained stale separate debug bytes"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(target_os = "macos")]
#[test]
fn external_macho_order_file_invalidates_reuse_without_changing_source() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let linker = std::env::split_paths(&std::env::var_os("PATH").context("linker PATH")?)
            .map(|directory| directory.join("ld64.lld"))
            .find(|path| path.is_file())
            .context("install ld64.lld and put it on PATH before running the Mach-O linker contract")?;
        let workspace = TestWorkspace::new_single_crate("order-control", "0.1.0")?;
        let external = tempfile::tempdir()?;
        let external_root = fs::canonicalize(external.path())?;
        let order_file = external_root.join("symbols.order");
        let clang_config = external_root.join("clang.cfg");
        fs::write(
            &clang_config,
            format!(
                "-fuse-ld={}\n-Wl,-order_file,{}\n",
                linker.display(),
                order_file.display()
            ),
        )?;
        fs::remove_file(workspace.path.join("src/lib.rs"))?;
        fs::write(
            workspace.path.join("src/main.rs"),
            "#[no_mangle]\n#[inline(never)]\npub extern \"C\" fn first() -> u64 { 41 }\n\
             #[no_mangle]\n#[inline(never)]\npub extern \"C\" fn second() -> u64 { 42 }\n\
             fn main() { println!(\"{} {}\", first(), second()); }\n",
        )?;
        let version = Command::new("rustc").arg("-vV").output()?;
        assert!(version.status.success(), "rustc prerequisite failed: {version:?}");
        let verbose = String::from_utf8(version.stdout)?;
        let host = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc host")?;
        fs::create_dir(workspace.path.join(".cargo"))?;
        fs::write(
            workspace.path.join(".cargo/config.toml"),
            format!(
                "[target.{host}]\nlinker = \"/usr/bin/clang\"\nrustflags = [{}]\n[profile.dev]\nsplit-debuginfo = \"off\"\n",
                serde_json::to_string(&format!("-Clink-arg=--config={}", clang_config.display()))?
            ),
        )?;
        let cargo_home = tempfile::tempdir()?;
        let target = workspace.path.join("target").join(host);
        let executable = target.join("debug/order-control");
        let orders = ["_first\n_second\n", "_second\n_first\n"];
        assert_eq!(orders[0].len(), orders[1].len(), "order mutation must retain file size");
        let mut baselines = Vec::new();
        for order in orders {
            fs::write(&order_file, order)?;
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, None)?;
            let run = Command::new(&executable).output()?;
            assert!(run.status.success(), "uncached executable failed: {run:?}");
            assert_eq!(run.stdout, b"41 42\n");
            assert!(run.stderr.is_empty());
            baselines.push((
                fs::read(&executable)?,
                fs::metadata(&executable)?.permissions().mode() & 0o777,
            ));
        }
        assert_ne!(
            baselines[0].0, baselines[1].0,
            "order-file fixture did not change the independently linked executable"
        );
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "order-file cache setup failed: {setup:?}");
        let reports = tempfile::tempdir()?;
        for (phase, order_index, hits, misses) in [("cold", 0, 0, 1), ("warm", 0, 1, 0), ("changed-order", 1, 0, 1)] {
            if phase != "warm" {
                fs::write(&order_file, orders[order_index])?;
            }
            fs::remove_dir_all(&target)?;
            let recording = reports.path().join(format!("{phase}.json"));
            let path = recording.to_str().context("report path")?;
            let start = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--start", path],
            )?;
            assert!(start.status.success(), "report start failed: {start:?}");
            explicit_target_cargo(&workspace.path, cargo_home.path(), "build", host, Some(&recording))?;
            let finish = rail(
                &workspace.path,
                cargo_home.path(),
                &["rail", "cache", "report", "--finish", path, "-f", "json"],
            )?;
            assert!(finish.status.success(), "report finish failed: {finish:?}");
            let report = json(&finish)?;
            assert_eq!(report["measurements"]["hits"], hits, "{phase}: {report}");
            assert_eq!(report["measurements"]["misses"], misses, "{phase}: {report}");
            assert_eq!(report["measurements"]["failures"], 0, "{phase}: {report}");
            assert_eq!(
                fs::read(&executable)?,
                baselines[order_index].0,
                "{phase} did not preserve uncached linker output"
            );
            assert_eq!(
                fs::metadata(&executable)?.permissions().mode() & 0o777,
                baselines[order_index].1,
                "{phase} changed executable permissions"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Qualify native reuse with tiny, dependency-free Cargo workloads on each host.
#[cfg(unix)]
#[test]
fn native_host_restores_and_executes_small_cargo_outputs() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;
        let workspace = TestWorkspace::new_single_crate("cache_host", "0.1.0")?;
        fs::write(
            workspace.path.join("Cargo.toml"),
            r#"[package]
name = "cache_host"
version = "0.1.0"
edition = "2024"
[workspace]
members = ["macros", "consumer"]
resolver = "3"
"#,
        )?;
        fs::write(
            workspace.path.join("build.rs"),
            r#"fn main() {
    std::fs::write(std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("value.rs"),
                   "pub fn value() -> u32 { 42 }").unwrap();
}"#,
        )?;
        fs::write(
            workspace.path.join("src/lib.rs"),
            r#"include!(concat!(env!("OUT_DIR"), "/value.rs"));"#,
        )?;
        fs::write(
            workspace.path.join("src/main.rs"),
            r#"fn main() { println!("{}", cache_host::value()); }"#,
        )?;
        for directory in ["macros/src", "consumer/src"] {
            fs::create_dir_all(workspace.path.join(directory))?;
        }
        fs::write(
            workspace.path.join("macros/Cargo.toml"),
            r#"[package]
name = "host_macros"
version = "0.1.0"
edition = "2024"
[lib]
proc-macro = true
"#,
        )?;
        fs::write(
            workspace.path.join("macros/src/lib.rs"),
            r#"extern crate proc_macro;
#[proc_macro]
pub fn answer(_: proc_macro::TokenStream) -> proc_macro::TokenStream { "42".parse().unwrap() }
"#,
        )?;
        fs::write(
            workspace.path.join("consumer/Cargo.toml"),
            r#"[package]
name = "host_consumer"
version = "0.1.0"
edition = "2024"
[dependencies]
host_macros = { path = "../macros" }
"#,
        )?;
        fs::write(
            workspace.path.join("consumer/src/lib.rs"),
            "pub fn value() -> u32 { host_macros::answer!() }\n",
        )?;
        let cargo_home = tempfile::tempdir()?;
        let target = workspace.path.join("target");
        let compile = |coverage: Option<&Path>| -> Result<Output> {
            let mut command = Command::new("cargo");
            command
                .current_dir(&workspace.path)
                .args(["build", "--workspace", "--quiet"])
                .env("CARGO_HOME", cargo_home.path())
                .env("CARGO_INCREMENTAL", "0")
                .env_remove("RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER");
            if let Some(coverage) = coverage {
                command
                    .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
                    .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", coverage);
            } else {
                command.env("CARGO_RAIL_CACHE", "off");
            }
            Ok(command.output()?)
        };
        let baseline = compile(None)?;
        assert!(baseline.status.success(), "uncached build failed: {baseline:?}");
        let executable = target.join("debug/cache_host");
        let artifacts = || -> Result<BTreeMap<PathBuf, (Vec<u8>, u32)>> {
            directory_snapshot(&target)?
                .into_iter()
                .filter(|(path, _)| {
                    matches!(
                        path.extension().and_then(|value| value.to_str()),
                        Some("rlib" | "rmeta" | "so" | "dylib")
                    ) || path == Path::new("debug/cache_host")
                        || path.file_name().is_some_and(|name| name == "build-script-build")
                })
                .map(|(path, bytes)| {
                    let mode = fs::metadata(target.join(&path))?.permissions().mode();
                    Ok((path, (bytes, mode)))
                })
                .collect()
        };
        let expected = artifacts()?;
        let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
        assert!(setup.status.success(), "cache setup failed: {setup:?}");
        for (phase, expected_status) in [("cold", "miss"), ("warm", "hit")] {
            fs::remove_dir_all(&target)?;
            let coverage = tempfile::tempdir()?;
            fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
            let coverage_path = fs::canonicalize(coverage.path())?;
            let output = compile(Some(&coverage_path))?;
            assert!(output.status.success(), "{phase} build failed: {output:?}");
            eprintln!(
                "{phase} compiler diagnostics:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let events = coverage_events(&coverage_path)?;
            for (crate_name, action_class) in [
                ("cache_host", "rust_library"),
                ("cache_host", "binary"),
                ("host_macros", "proc_macro_producer"),
                ("build_script_build", "build_script"),
            ] {
                let actions = events
                    .iter()
                    .filter(|event| {
                        event["action"]["crate_name"] == crate_name && event["action"]["action_class"] == action_class
                    })
                    .collect::<Vec<_>>();
                assert!(
                    !actions.is_empty(),
                    "{phase}: missing {crate_name}/{action_class}: {events:?}"
                );
                assert!(
                    actions.iter().all(|event| event["status"] == expected_status),
                    "{phase}: incorrect {crate_name}/{action_class} reuse: {actions:?}"
                );
            }
            assert!(
                events.iter().any(|event| event["status"] == "bypassed"
                    && event["reason"] == "dynamic_dependency_execution_observation_unavailable"),
                "{phase}: proc-macro consumer must retain safe bypass: {events:?}"
            );
            assert!(
                artifacts()? == expected,
                "{phase}: output inventory, bytes or modes changed"
            );
            let executed = Command::new(&executable).output()?;
            assert!(
                executed.status.success(),
                "{phase}: restored executable failed: {executed:?}"
            );
            assert_eq!(executed.stdout, b"42\n");
            assert!(executed.stderr.is_empty());
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}
