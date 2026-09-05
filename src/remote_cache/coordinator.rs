//! Short-lived private loopback coordination for direct remote-cache transport.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek as _, Write as _};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};

use super::{RemoteCacheSelection, RemoteStoreError, RemoteStoreResult, object};
use crate::cache::installation::InstallationReceipt;

pub(super) const MARKER_ENV: &str = "CARGO_RAIL_REMOTE_CACHE_COORDINATOR";

const STATE_VERSION: u32 = 1;
const STATE_MAX_BYTES: u64 = (super::url::MAX_URL_BYTES + 1024) as u64;
const IPC_MAGIC: &[u8; 8] = b"CRRIPC6\0";
const IPC_RESPONSE_MAGIC: &[u8; 8] = b"CRRRES6\0";
const IPC_RESPONSE_TRAILER_MAGIC: &[u8; 8] = b"CRREND6\0";
const IPC_MAX_TOKEN_BYTES: usize = 128;
const IPC_MAX_IDENTITY_BYTES: usize = 128;
const IPC_MAX_AUTHORITY_BYTES: usize = super::url::MAX_URL_BYTES;
const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IPC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_POLL: Duration = Duration::from_millis(1);
#[cfg(debug_assertions)]
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(debug_assertions))]
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const WORKERS: usize = 4;

const OP_LOOKUP: u8 = 1;
const OP_PUBLISH: u8 = 2;
const OP_STOP: u8 = 3;

const RESPONSE_OK: u8 = 0;
const RESPONSE_MISS: u8 = 1;
const RESPONSE_CONFLICT: u8 = 2;
const RESPONSE_PACK: u8 = 3;

const CREDENTIAL_IDENTITY_ENVIRONMENT: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_PROFILE",
    "AWS_DEFAULT_PROFILE",
    "AWS_ROLE_ARN",
    "AWS_ROLE_SESSION_NAME",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_METADATA_SERVICE_TIMEOUT",
    "AWS_METADATA_SERVICE_NUM_ATTEMPTS",
    "AWS_SDK_LOAD_CONFIG",
    "AWS_LOGIN_CACHE_DIRECTORY",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoordinatorState {
    version: u32,
    identity: String,
    endpoint: String,
    read_token: String,
    publish_token: String,
    authority: String,
}

impl CoordinatorState {
    fn validate(&self, identity: &str, authority: &str) -> RemoteStoreResult<()> {
        let endpoint = self
            .endpoint
            .parse::<SocketAddr>()
            .map_err(|_| RemoteStoreError::integrity("remote coordinator endpoint is invalid"))?;
        if self.version != STATE_VERSION
            || self.identity != identity
            || self.authority != authority
            || self.authority.is_empty()
            || self.authority.len() > IPC_MAX_AUTHORITY_BYTES
            || !endpoint.ip().is_loopback()
            || !valid_token(&self.read_token)
            || !valid_token(&self.publish_token)
            || self.read_token == self.publish_token
        {
            return Err(RemoteStoreError::integrity(
                "remote coordinator state does not match its authority",
            ));
        }
        Ok(())
    }

    fn encode(&self) -> RemoteStoreResult<Vec<u8>> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| RemoteStoreError::integrity("remote coordinator state could not be encoded"))?;
        if bytes.len() as u64 > STATE_MAX_BYTES {
            return Err(RemoteStoreError::integrity(
                "remote coordinator state exceeds its byte bound",
            ));
        }
        Ok(bytes)
    }
}

#[derive(Debug, Default)]
struct ClientMetrics {
    requests: AtomicU64,
    request_attempts: AtomicU64,
    payload_bytes_read: AtomicU64,
    payload_bytes_written: AtomicU64,
    service_elapsed_ns: AtomicU64,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Metrics {
    pub(super) requests: u64,
    pub(super) request_attempts: u64,
    pub(super) payload_bytes_read: u64,
    pub(super) payload_bytes_written: u64,
    pub(super) service_elapsed_ns: u64,
}

pub(super) struct Client {
    state: CoordinatorState,
    metrics: Arc<ClientMetrics>,
}

impl Client {
    fn stop(&self) -> RemoteStoreResult<()> {
        let mut stream = self.request(OP_STOP, true)?;
        match self.read_response_prelude(&mut stream)? {
            RESPONSE_OK => Ok(()),
            code => Err(response_error(code)),
        }
    }

    fn request(&self, operation: u8, publish: bool) -> RemoteStoreResult<TcpStream> {
        let mut stream = connect_loopback(&self.state.endpoint)?;
        stream.write_all(IPC_MAGIC).map_err(io_unavailable)?;
        write_string(
            &mut stream,
            if publish {
                &self.state.publish_token
            } else {
                &self.state.read_token
            },
            IPC_MAX_TOKEN_BYTES,
        )?;
        stream.write_all(&[operation]).map_err(io_unavailable)?;
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        Ok(stream)
    }

    fn read_response_prelude(&self, stream: &mut TcpStream) -> RemoteStoreResult<u8> {
        let (status, metrics) = read_response_prelude(stream, &self.state)?;
        self.metrics
            .request_attempts
            .fetch_add(metrics.request_attempts, Ordering::Relaxed);
        self.metrics
            .payload_bytes_read
            .fetch_add(metrics.payload_bytes_read, Ordering::Relaxed);
        self.metrics
            .payload_bytes_written
            .fetch_add(metrics.payload_bytes_written, Ordering::Relaxed);
        self.metrics
            .service_elapsed_ns
            .fetch_add(metrics.service_elapsed_ns, Ordering::Relaxed);
        Ok(status)
    }

    pub(super) fn lookup(&self, base_action_key: &str) -> RemoteStoreResult<Lookup> {
        crate::compiler::native_cache::validate_base_action_key(base_action_key)
            .map_err(|_| RemoteStoreError::integrity("remote entry identity is invalid"))?;
        let mut stream = self.request(OP_LOOKUP, false)?;
        write_string(&mut stream, base_action_key, IPC_MAX_IDENTITY_BYTES)?;
        match self.read_response_prelude(&mut stream)? {
            RESPONSE_MISS => Ok(Lookup::Miss),
            RESPONSE_CONFLICT => Ok(Lookup::Conflict),
            RESPONSE_PACK => {
                let selector = read_dynamic_input_selector(&mut stream)?;
                let action_key = read_string(&mut stream, IPC_MAX_IDENTITY_BYTES)?;
                crate::compiler::native_cache::validate_action_key(&action_key)
                    .map_err(|_| RemoteStoreError::integrity("remote action identity is invalid"))?;
                let result_key = read_string(&mut stream, IPC_MAX_IDENTITY_BYTES)?;
                crate::compiler::native_cache::validate_result_key(&result_key)
                    .map_err(|_| RemoteStoreError::integrity("remote result identity is invalid"))?;
                let bytes = read_u64(&mut stream)?;
                if bytes > crate::compiler::native_cache::pack::MAX_PACK_BYTES {
                    return Err(RemoteStoreError::integrity(
                        "remote pack exceeds its absolute byte bound",
                    ));
                }
                let compressed_bytes = read_u64(&mut stream)?;
                if compressed_bytes == 0 || compressed_bytes > crate::compiler::native_cache::pack::MAX_PACK_BYTES {
                    return Err(RemoteStoreError::integrity(
                        "remote compressed pack exceeds its absolute byte bound",
                    ));
                }
                Ok(Lookup::Unique {
                    selector,
                    action_key,
                    result_key,
                    body: PackReader {
                        stream,
                        remaining: compressed_bytes,
                        metrics: Arc::clone(&self.metrics),
                        finished: false,
                    },
                    bytes,
                    compressed_bytes,
                })
            }
            code => Err(response_error(code)),
        }
    }

    pub(super) fn publish(
        &self,
        association: &crate::compiler::native_cache::pack::NativeAssociation,
        base_action_key: &str,
        selector: &crate::compiler::native_cache::NativeDynamicInputSelector,
        mut pack: File,
    ) -> RemoteStoreResult<object::Publication> {
        crate::compiler::native_cache::validate_action_key(association.action_key())
            .map_err(|_| RemoteStoreError::integrity("remote action identity is invalid"))?;
        crate::compiler::native_cache::validate_result_key(association.result_key())
            .map_err(|_| RemoteStoreError::integrity("remote result identity is invalid"))?;
        crate::compiler::native_cache::validate_base_action_key(base_action_key)
            .map_err(|_| RemoteStoreError::integrity("remote selector identity is invalid"))?;
        selector
            .validate()
            .map_err(|_| RemoteStoreError::integrity("remote dynamic-input selector is invalid"))?;
        let metadata = pack.metadata().map_err(io_unavailable)?;
        if !metadata.is_file() || metadata.len() != association.pack_length() {
            return Err(RemoteStoreError::integrity(
                "remote publication pack does not match its verified association",
            ));
        }
        pack.rewind().map_err(io_unavailable)?;
        let mut stream = self.request(OP_PUBLISH, true)?;
        write_string(&mut stream, association.action_key(), IPC_MAX_IDENTITY_BYTES)?;
        write_string(&mut stream, association.result_key(), IPC_MAX_IDENTITY_BYTES)?;
        write_string(&mut stream, base_action_key, IPC_MAX_IDENTITY_BYTES)?;
        write_dynamic_input_selector(&mut stream, selector)?;
        stream
            .write_all(&association.pack_length().to_le_bytes())
            .map_err(io_unavailable)?;
        let written = std::io::copy(&mut pack, &mut stream).map_err(io_unavailable)?;
        if written != association.pack_length() {
            return Err(RemoteStoreError::integrity(
                "remote publication stream changed before coordination",
            ));
        }
        stream.shutdown(Shutdown::Write).map_err(io_unavailable)?;
        match self.read_response_prelude(&mut stream)? {
            RESPONSE_OK => Ok(object::Publication::Unique),
            RESPONSE_CONFLICT => Ok(object::Publication::Conflict),
            code => Err(response_error(code)),
        }
    }

    pub(super) fn metrics(&self) -> Metrics {
        Metrics {
            requests: self.metrics.requests.load(Ordering::Relaxed),
            request_attempts: self.metrics.request_attempts.load(Ordering::Relaxed),
            payload_bytes_read: self.metrics.payload_bytes_read.load(Ordering::Relaxed),
            payload_bytes_written: self.metrics.payload_bytes_written.load(Ordering::Relaxed),
            service_elapsed_ns: self.metrics.service_elapsed_ns.load(Ordering::Relaxed),
        }
    }
}

pub(super) enum Lookup {
    Miss,
    Conflict,
    Unique {
        selector: crate::compiler::native_cache::NativeDynamicInputSelector,
        action_key: String,
        result_key: String,
        body: PackReader,
        bytes: u64,
        compressed_bytes: u64,
    },
}

pub(crate) struct PackReader {
    stream: TcpStream,
    remaining: u64,
    metrics: Arc<ClientMetrics>,
    finished: bool,
}

impl PackReader {
    pub(super) fn finish(&mut self) -> std::io::Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.remaining != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "remote coordinator pack was not consumed before its trailer",
            ));
        }
        let service_elapsed_ns =
            read_response_trailer(&mut self.stream).map_err(|error| std::io::Error::other(error.to_string()))?;
        self.metrics
            .service_elapsed_ns
            .fetch_add(service_elapsed_ns, Ordering::Relaxed);
        self.finished = true;
        Ok(())
    }
}

impl Read for PackReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() || self.finished {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.finish()?;
            return Ok(0);
        }
        let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
        let read = self.stream.read(&mut output[..maximum])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "remote coordinator ended a compressed pack before its declared length",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

pub(super) fn connect(
    selection: &RemoteCacheSelection,
    receipt: &InstallationReceipt,
) -> RemoteStoreResult<Option<Client>> {
    let Some(identity) = coordinator_identity(selection, receipt)? else {
        return Ok(None);
    };
    if let Some(client) = load_client(selection, receipt, &identity)? {
        return Ok(Some(client));
    }
    let lock_path = crate::cache::installation::coordinator_lock_file(receipt, &identity)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator lock path is unavailable"))?;
    let lock = crate::utils::open_cache_lock_file(&lock_path, true).map_err(io_unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_unavailable)?;
    }
    if !crate::utils::private_file_matches_path(&lock, &lock_path, 0).map_err(io_unavailable)? {
        return Err(RemoteStoreError::integrity(
            "remote coordinator lock is not a private regular file",
        ));
    }
    lock.lock().map_err(io_unavailable)?;
    if let Some(client) = load_client(selection, receipt, &identity)? {
        return Ok(Some(client));
    }

    let mut coordinator = Command::new(receipt.worker_path());
    coordinator
        .env(MARKER_ENV, &identity)
        .env(
            crate::cache::profile::COORDINATOR_PROFILE_ENV,
            receipt
                .profile()
                .map_err(|_| RemoteStoreError::integrity("remote coordinator has no cache profile"))?
                .coordinator_capability(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // The coordinator outlives the compiler wrapper that starts it. Inheriting
        // that wrapper's stderr keeps Cargo's capture pipe open until the idle
        // timeout and serializes later dependency work behind a detached process.
        // Client-side cache events retain the authoritative failure evidence.
        .stderr(Stdio::null());
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};

        // Cargo waits for every descendant in its Windows job. The coordinator is
        // intentionally longer-lived than the compiler invocation, so keeping it
        // in that job deadlocks Cargo at the end of an otherwise complete build.
        // A job-controlled runner may deny breakaway; the caller retains that
        // error and uses the ordinary direct transport instead of spawning a
        // coordinator that cannot satisfy its lifetime contract.
        coordinator.creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW);
        coordinator.spawn()
    };
    #[cfg(not(windows))]
    let child = coordinator.spawn();
    let mut child = child.map_err(io_unavailable)?;
    let started = Instant::now();
    loop {
        if let Some(client) = load_client(selection, receipt, &identity)? {
            return Ok(Some(client));
        }
        if child.try_wait().map_err(io_unavailable)?.is_some() {
            retire_coordinator_state(receipt, &identity)?;
            return Err(RemoteStoreError::unavailable("remote coordinator did not become ready"));
        }
        if started.elapsed() >= START_TIMEOUT {
            break;
        }
        std::thread::sleep(ACCEPT_POLL);
    }
    if let Err(error) = child.kill()
        && child.try_wait().map_err(io_unavailable)?.is_none()
    {
        return Err(io_unavailable(error));
    }
    child.wait().map_err(io_unavailable)?;
    retire_coordinator_state(receipt, &identity)?;
    Err(RemoteStoreError::unavailable("remote coordinator did not become ready"))
}

fn retire_coordinator_state(receipt: &InstallationReceipt, identity: &str) -> RemoteStoreResult<()> {
    if let Some(bytes) = crate::cache::installation::read_coordinator_state(receipt, identity, STATE_MAX_BYTES)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator state is unavailable"))?
    {
        crate::cache::installation::remove_coordinator_state_if(receipt, identity, &bytes)
            .map_err(|_| RemoteStoreError::unavailable("remote coordinator state could not be retired"))?;
    }
    Ok(())
}

fn load_client(
    selection: &RemoteCacheSelection,
    receipt: &InstallationReceipt,
    identity: &str,
) -> RemoteStoreResult<Option<Client>> {
    let Some(bytes) = crate::cache::installation::read_coordinator_state(receipt, identity, STATE_MAX_BYTES)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator state is unavailable"))?
    else {
        return Ok(None);
    };
    let state = serde_json::from_slice::<CoordinatorState>(&bytes)
        .map_err(|_| RemoteStoreError::integrity("remote coordinator state is malformed"))?;
    if state.encode()? != bytes {
        return Err(RemoteStoreError::integrity(
            "remote coordinator state is not canonically encoded",
        ));
    }
    state.validate(identity, selection.authority().as_str())?;
    Ok(Some(Client {
        state,
        metrics: Arc::new(ClientMetrics::default()),
    }))
}

pub(crate) fn run_if_requested() -> Option<i32> {
    let identity = std::env::var(MARKER_ENV).ok()?;
    Some(match run_server(&identity) {
        Ok(()) => 0,
        Err(_) => 2,
    })
}

pub(super) fn stop_all(receipt: &InstallationReceipt) {
    let Ok(directory) = receipt.profile_state_directory() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let identities = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let name = name.strip_prefix("remote-coordinator-v1-")?;
            name.strip_suffix(".json")
                .or_else(|| name.strip_suffix(".lock"))
                .filter(|identity| identity.len() == 64)
                .map(str::to_string)
        })
        .collect::<BTreeSet<_>>();
    for identity in identities {
        if let Ok(Some(bytes)) = crate::cache::installation::read_coordinator_state(receipt, &identity, STATE_MAX_BYTES)
            && let Ok(state) = serde_json::from_slice::<CoordinatorState>(&bytes)
            && state.encode().ok().as_deref() == Some(bytes.as_slice())
            && state.validate(&identity, &state.authority).is_ok()
        {
            let client = Client {
                state,
                metrics: Arc::new(ClientMetrics::default()),
            };
            drop(client.stop());
            drop(crate::cache::installation::remove_coordinator_state_if(
                receipt, &identity, &bytes,
            ));
        }
        remove_coordinator_lock(receipt, &identity, LOCK_RELEASE_TIMEOUT);
    }
}

fn remove_coordinator_lock(receipt: &InstallationReceipt, identity: &str, timeout: Duration) {
    let Ok(lock_path) = crate::cache::installation::coordinator_lock_file(receipt, identity) else {
        return;
    };
    let Ok(lock) = crate::utils::open_cache_lock_file(&lock_path, false) else {
        return;
    };
    let started = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => {
                if crate::utils::private_file_matches_path(&lock, &lock_path, 0).unwrap_or(false) {
                    // Windows lock handles deliberately deny delete sharing so their
                    // path cannot be replaced while held. Release the exclusive handle
                    // before removing the empty lock from the private installation
                    // directory; Unix can remove the still-open inode directly.
                    #[cfg(windows)]
                    drop(lock);
                    drop(std::fs::remove_file(lock_path));
                }
                return;
            }
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < timeout => std::thread::sleep(ACCEPT_POLL),
            Err(_) => return,
        }
    }
}

fn run_server(identity: &str) -> RemoteStoreResult<()> {
    let invoked = std::env::current_exe()
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator executable is unavailable"))?;
    let receipt = crate::cache::installation::load_for_coordinator(&invoked)
        .map_err(|_| RemoteStoreError::integrity("remote coordinator installation is invalid"))?;
    let selection = RemoteCacheSelection::from_environment_or_installed(receipt.remote())?
        .filter(RemoteCacheSelection::direct_transport_supported)
        .ok_or_else(|| RemoteStoreError::configuration("remote coordinator has no qualified authority"))?;
    if coordinator_identity(&selection, &receipt)?.as_deref() != Some(identity) {
        return Err(RemoteStoreError::integrity(
            "remote coordinator identity does not match its process environment",
        ));
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(io_unavailable)?;
    listener.set_nonblocking(true).map_err(io_unavailable)?;
    let state = CoordinatorState {
        version: STATE_VERSION,
        identity: identity.to_string(),
        endpoint: listener.local_addr().map_err(io_unavailable)?.to_string(),
        read_token: random_token()?,
        publish_token: random_token()?,
        authority: selection.authority().as_str().to_string(),
    };
    state.validate(identity, selection.authority().as_str())?;
    let encoded = state.encode()?;
    crate::cache::installation::write_coordinator_state(&receipt, identity, &encoded)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator state could not be published"))?;

    let shared = Arc::new(ServerShared {
        selection,
        authority: state.authority.clone(),
        read_token: state.read_token.clone(),
        publish_token: state.publish_token,
        store: OnceLock::new(),
        stop: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        started: Instant::now(),
        last_activity_millis: AtomicU64::new(0),
    });
    let mut workers = Vec::with_capacity(WORKERS);
    for index in 0..WORKERS {
        let worker_listener = listener.try_clone().map_err(io_unavailable)?;
        let worker_shared = Arc::clone(&shared);
        workers.push(
            std::thread::Builder::new()
                .name(format!("cargo-rail-remote-{index}"))
                .spawn(move || server_loop(worker_listener, worker_shared))
                .map_err(io_unavailable)?,
        );
    }
    drop(listener);
    let mut worker_panicked = false;
    for worker in workers {
        worker_panicked |= worker.join().is_err();
    }
    crate::cache::installation::remove_coordinator_state_if(&receipt, identity, &encoded)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator state could not be retired"))?;
    remove_coordinator_lock(&receipt, identity, Duration::ZERO);
    if worker_panicked {
        return Err(RemoteStoreError::unavailable("remote coordinator worker panicked"));
    }
    Ok(())
}

struct ServerShared {
    selection: RemoteCacheSelection,
    authority: String,
    read_token: String,
    publish_token: String,
    store: OnceLock<RemoteStoreResult<object::ObjectStore>>,
    stop: AtomicBool,
    active: AtomicUsize,
    started: Instant,
    last_activity_millis: AtomicU64,
}

impl ServerShared {
    fn store(&self) -> RemoteStoreResult<&object::ObjectStore> {
        match self.store.get_or_init(|| object::connect(&self.selection)) {
            Ok(store) => Ok(store),
            Err(error) => Err(error.clone()),
        }
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn touch(&self) {
        self.last_activity_millis
            .store(self.elapsed_millis(), Ordering::Release);
    }

    fn idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
            && self
                .elapsed_millis()
                .saturating_sub(self.last_activity_millis.load(Ordering::Acquire))
                >= u64::try_from(IDLE_TIMEOUT.as_millis()).unwrap_or(u64::MAX)
    }
}

fn server_loop(listener: TcpListener, shared: Arc<ServerShared>) {
    while !shared.stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                shared.active.fetch_add(1, Ordering::AcqRel);
                shared.touch();
                if configure_stream(&stream).is_ok() {
                    handle_connection(&mut stream, &shared);
                }
                shared.touch();
                shared.active.fetch_sub(1, Ordering::AcqRel);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if shared.idle() {
                    shared.stop.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Release);
                break;
            }
        }
    }
}

fn handle_connection(stream: &mut TcpStream, shared: &ServerShared) {
    if let Err(error) = handle_request(stream, shared)
        && std::env::var_os("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY").is_some()
    {
        eprintln!("cargo-rail remote coordinator: {error}");
    }
}

fn handle_request(stream: &mut TcpStream, shared: &ServerShared) -> RemoteStoreResult<()> {
    let mut magic = [0_u8; 8];
    stream.read_exact(&mut magic).map_err(io_unavailable)?;
    if &magic != IPC_MAGIC {
        return Err(RemoteStoreError::integrity(
            "remote coordinator request has invalid magic",
        ));
    }
    let token = read_string(stream, IPC_MAX_TOKEN_BYTES)?;
    let mut operation = [0_u8; 1];
    stream.read_exact(&mut operation).map_err(io_unavailable)?;
    let expected = if matches!(operation[0], OP_PUBLISH | OP_STOP) {
        &shared.publish_token
    } else {
        &shared.read_token
    };
    if !capability_matches(&token, expected) {
        return Err(RemoteStoreError::integrity("remote coordinator capability is invalid"));
    }
    match operation[0] {
        OP_STOP => {
            write_response_prelude(
                stream,
                &shared.authority,
                RESPONSE_OK,
                object::TransferMetrics::default(),
            )?;
            shared.stop.store(true, Ordering::Release);
            Ok(())
        }
        OP_LOOKUP => {
            let base_action_key = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
            crate::compiler::native_cache::validate_base_action_key(&base_action_key)
                .map_err(|_| RemoteStoreError::integrity("remote entry identity is invalid"))?;
            let started = Instant::now();
            let store = shared.store()?;
            let lookup = store.lookup(&base_action_key)?;
            let mut metrics = store.take_metrics();
            match lookup {
                object::Lookup::Miss => {
                    metrics.service_elapsed_ns = elapsed_nanos(started);
                    write_response_prelude(stream, &shared.authority, RESPONSE_MISS, metrics)
                }
                object::Lookup::Conflict => {
                    metrics.service_elapsed_ns = elapsed_nanos(started);
                    write_response_prelude(stream, &shared.authority, RESPONSE_CONFLICT, metrics)
                }
                object::Lookup::Unique {
                    selector,
                    action_key,
                    result_key,
                    mut body,
                    bytes,
                    compressed_bytes,
                } => {
                    write_response_prelude(stream, &shared.authority, RESPONSE_PACK, metrics)?;
                    write_dynamic_input_selector(stream, &selector)?;
                    write_string(stream, &action_key, IPC_MAX_IDENTITY_BYTES)?;
                    write_string(stream, &result_key, IPC_MAX_IDENTITY_BYTES)?;
                    stream.write_all(&bytes.to_le_bytes()).map_err(io_unavailable)?;
                    stream
                        .write_all(&compressed_bytes.to_le_bytes())
                        .map_err(io_unavailable)?;
                    let copied = body.copy_compressed_to(stream).map_err(io_unavailable)?;
                    if copied != compressed_bytes {
                        return Err(RemoteStoreError::integrity(
                            "remote coordinator streamed the wrong compressed pack length",
                        ));
                    }
                    write_response_trailer(stream, elapsed_nanos(started))
                }
            }
        }
        OP_PUBLISH => handle_publication(stream, shared),
        _ => Err(RemoteStoreError::integrity("remote coordinator operation is invalid")),
    }
}

fn handle_publication(stream: &mut TcpStream, shared: &ServerShared) -> RemoteStoreResult<()> {
    let action_key = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
    let result_key = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
    let base_action_key = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
    crate::compiler::native_cache::validate_action_key(&action_key)
        .map_err(|_| RemoteStoreError::integrity("remote action identity is invalid"))?;
    crate::compiler::native_cache::validate_result_key(&result_key)
        .map_err(|_| RemoteStoreError::integrity("remote result identity is invalid"))?;
    crate::compiler::native_cache::validate_base_action_key(&base_action_key)
        .map_err(|_| RemoteStoreError::integrity("remote selector identity is invalid"))?;
    let selector = read_dynamic_input_selector(stream)?;
    if !shared.selection.approves_environment_names(&selector.environment_names) {
        return Err(RemoteStoreError::integrity(
            "remote publication environment exceeds its configured authority",
        ));
    }
    let bytes = read_u64(stream)?;
    if bytes > crate::compiler::native_cache::pack::MAX_PACK_BYTES {
        return Err(RemoteStoreError::integrity("remote publication exceeds its pack bound"));
    }
    let mut pack = tempfile::tempfile().map_err(io_unavailable)?;
    let copied = std::io::copy(&mut stream.take(bytes), &mut pack).map_err(io_unavailable)?;
    if copied != bytes {
        return Err(RemoteStoreError::integrity(
            "remote publication ended before its declared length",
        ));
    }
    pack.flush().map_err(io_unavailable)?;
    pack.rewind().map_err(io_unavailable)?;
    let validation = pack.try_clone().map_err(io_unavailable)?;
    let (_decoded, association) =
        crate::compiler::native_cache::pack::decode_for_action(validation, &action_key, Some(bytes), None)
            .map_err(|_| RemoteStoreError::integrity("remote publication pack is malformed"))?;
    if association.result_key() != result_key || association.pack_length() != bytes {
        return Err(RemoteStoreError::integrity(
            "remote publication pack does not match its requested identity",
        ));
    }
    pack.rewind().map_err(io_unavailable)?;
    let started = Instant::now();
    let store = shared.store()?;
    let publication = store.publish(&association, &base_action_key, &selector, pack)?;
    let mut metrics = store.take_metrics();
    metrics.service_elapsed_ns = elapsed_nanos(started);
    match publication {
        object::Publication::Unique => write_response_prelude(stream, &shared.authority, RESPONSE_OK, metrics),
        object::Publication::Conflict => write_response_prelude(stream, &shared.authority, RESPONSE_CONFLICT, metrics),
    }
}

fn coordinator_identity(
    selection: &RemoteCacheSelection,
    receipt: &InstallationReceipt,
) -> RemoteStoreResult<Option<String>> {
    let cache_base = receipt
        .cache()
        .map_err(|_| RemoteStoreError::configuration("remote coordinator has no local cache profile"))?
        .base()
        .to_str()
        .ok_or_else(|| RemoteStoreError::configuration("local cache path is not valid UTF-8"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-remote-coordinator-v6\0");
    hash_field(&mut hasher, selection.authority().as_str());
    hash_field(&mut hasher, selection.mode().as_str());
    hash_field(&mut hasher, receipt.authority());
    let profile = receipt
        .profile()
        .map_err(|_| RemoteStoreError::configuration("remote coordinator has no cache profile"))?;
    hash_field(&mut hasher, profile.profile_id());
    hash_field(&mut hasher, profile.generation());
    hash_field(&mut hasher, profile.selected_root_identity());
    hash_field(&mut hasher, receipt.worker_digest());
    hash_field(&mut hasher, cache_base);
    for name in selection.approved_environment_names() {
        hash_field(&mut hasher, name);
    }
    // Credential values are consumed only by this digest. The private live state
    // stores the digest, never a source value, and uses it solely to prevent two
    // credential authorities from sharing one transport process.
    for name in CREDENTIAL_IDENTITY_ENVIRONMENT {
        hash_field(&mut hasher, name);
        let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) else {
            hash_field(&mut hasher, "");
            continue;
        };
        let Some(value) = value.to_str() else {
            return Ok(None);
        };
        hash_field(&mut hasher, value);
    }
    let digest: [u8; 32] = hasher.finalize();
    Ok(Some(
        crate::source::ContentDigest::from_sha256_bytes(digest).to_string(),
    ))
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn random_token() -> RemoteStoreResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| RemoteStoreError::unavailable("remote coordinator capability could not be created"))?;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;

        write!(encoded, "{byte:02x}")
            .map_err(|_| RemoteStoreError::unavailable("remote coordinator capability could not be encoded"))?;
    }
    Ok(encoded)
}

fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn capability_matches(actual: &str, expected: &str) -> bool {
    let mut difference = actual.len() ^ expected.len();
    for (actual, expected) in actual.bytes().zip(expected.bytes()) {
        difference |= usize::from(actual ^ expected);
    }
    difference == 0
}

fn configure_stream(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IPC_TRANSFER_TIMEOUT))?;
    stream.set_write_timeout(Some(IPC_TRANSFER_TIMEOUT))
}

fn connect_loopback(endpoint: &str) -> RemoteStoreResult<TcpStream> {
    let address = endpoint
        .parse::<SocketAddr>()
        .map_err(|_| RemoteStoreError::integrity("remote coordinator endpoint is invalid"))?;
    if !address.ip().is_loopback() {
        return Err(RemoteStoreError::integrity(
            "remote coordinator endpoint is not loopback",
        ));
    }
    let stream = TcpStream::connect_timeout(&address, IPC_CONNECT_TIMEOUT).map_err(io_unavailable)?;
    configure_stream(&stream).map_err(io_unavailable)?;
    Ok(stream)
}

fn write_response_prelude(
    stream: &mut TcpStream,
    authority: &str,
    status: u8,
    metrics: object::TransferMetrics,
) -> RemoteStoreResult<()> {
    stream.write_all(IPC_RESPONSE_MAGIC).map_err(io_unavailable)?;
    write_string(stream, authority, IPC_MAX_AUTHORITY_BYTES)?;
    stream.write_all(&[status]).map_err(io_unavailable)?;
    stream
        .write_all(&metrics.request_attempts.to_le_bytes())
        .map_err(io_unavailable)?;
    stream
        .write_all(&metrics.payload_bytes_read.to_le_bytes())
        .map_err(io_unavailable)?;
    stream
        .write_all(&metrics.payload_bytes_written.to_le_bytes())
        .map_err(io_unavailable)?;
    stream
        .write_all(&metrics.service_elapsed_ns.to_le_bytes())
        .map_err(io_unavailable)
}

fn read_response_prelude(
    stream: &mut TcpStream,
    state: &CoordinatorState,
) -> RemoteStoreResult<(u8, object::TransferMetrics)> {
    let mut magic = [0_u8; 8];
    stream.read_exact(&mut magic).map_err(io_unavailable)?;
    if &magic != IPC_RESPONSE_MAGIC {
        return Err(RemoteStoreError::integrity(
            "remote coordinator response has invalid magic",
        ));
    }
    if read_string(stream, IPC_MAX_AUTHORITY_BYTES)? != state.authority {
        return Err(RemoteStoreError::integrity(
            "remote coordinator returned the wrong authority",
        ));
    }
    let mut status = [0_u8; 1];
    stream.read_exact(&mut status).map_err(io_unavailable)?;
    Ok((
        status[0],
        object::TransferMetrics {
            request_attempts: read_u64(stream)?,
            payload_bytes_read: read_u64(stream)?,
            payload_bytes_written: read_u64(stream)?,
            service_elapsed_ns: read_u64(stream)?,
        },
    ))
}

fn write_response_trailer(stream: &mut TcpStream, service_elapsed_ns: u64) -> RemoteStoreResult<()> {
    stream.write_all(IPC_RESPONSE_TRAILER_MAGIC).map_err(io_unavailable)?;
    stream
        .write_all(&service_elapsed_ns.to_le_bytes())
        .map_err(io_unavailable)
}

fn read_response_trailer(stream: &mut TcpStream) -> RemoteStoreResult<u64> {
    let mut magic = [0_u8; 8];
    stream.read_exact(&mut magic).map_err(io_unavailable)?;
    if &magic != IPC_RESPONSE_TRAILER_MAGIC {
        return Err(RemoteStoreError::integrity(
            "remote coordinator response has an invalid trailer",
        ));
    }
    read_u64(stream)
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn write_environment_names(stream: &mut TcpStream, names: &[String]) -> RemoteStoreResult<()> {
    super::validate_environment_names(names)?;
    let count = u16::try_from(names.len())
        .map_err(|_| RemoteStoreError::integrity("remote environment count is out of range"))?;
    stream.write_all(&count.to_le_bytes()).map_err(io_unavailable)?;
    for name in names {
        write_string(stream, name, super::MAX_APPROVED_ENVIRONMENT_NAME_BYTES)?;
    }
    Ok(())
}

fn read_environment_names(stream: &mut TcpStream) -> RemoteStoreResult<Vec<String>> {
    let mut count = [0_u8; 2];
    stream.read_exact(&mut count).map_err(io_unavailable)?;
    let count = usize::from(u16::from_le_bytes(count));
    if count > super::MAX_APPROVED_ENVIRONMENT_NAMES {
        return Err(RemoteStoreError::integrity(
            "remote environment count exceeds its bound",
        ));
    }
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(read_string(stream, super::MAX_APPROVED_ENVIRONMENT_NAME_BYTES)?);
    }
    super::validate_environment_names(&names)?;
    Ok(names)
}

fn write_dynamic_input_selector(
    stream: &mut TcpStream,
    selector: &crate::compiler::native_cache::NativeDynamicInputSelector,
) -> RemoteStoreResult<()> {
    selector
        .validate()
        .map_err(|_| RemoteStoreError::integrity("remote dynamic-input selector is invalid"))?;
    write_environment_names(stream, &selector.environment_names)?;
    let count = u16::try_from(selector.repository_paths.len())
        .map_err(|_| RemoteStoreError::integrity("remote repository path count is out of range"))?;
    stream.write_all(&count.to_le_bytes()).map_err(io_unavailable)?;
    for path in &selector.repository_paths {
        write_string(
            stream,
            path,
            crate::compiler::native_cache::MAX_DYNAMIC_REPOSITORY_PATH_BYTES,
        )?;
    }
    Ok(())
}

fn read_dynamic_input_selector(
    stream: &mut TcpStream,
) -> RemoteStoreResult<crate::compiler::native_cache::NativeDynamicInputSelector> {
    let environment_names = read_environment_names(stream)?;
    let mut count = [0_u8; 2];
    stream.read_exact(&mut count).map_err(io_unavailable)?;
    let count = usize::from(u16::from_le_bytes(count));
    if count > crate::compiler::native_cache::MAX_DYNAMIC_REPOSITORY_INPUTS {
        return Err(RemoteStoreError::integrity(
            "remote repository path count exceeds its bound",
        ));
    }
    let mut repository_paths = Vec::with_capacity(count);
    let mut total_bytes = 0usize;
    for _ in 0..count {
        let path = read_string(stream, crate::compiler::native_cache::MAX_DYNAMIC_REPOSITORY_PATH_BYTES)?;
        total_bytes = total_bytes
            .checked_add(path.len())
            .filter(|bytes| *bytes <= crate::compiler::native_cache::MAX_DYNAMIC_REPOSITORY_TOTAL_PATH_BYTES)
            .ok_or_else(|| RemoteStoreError::integrity("remote repository path bytes exceed their bound"))?;
        repository_paths.push(path);
    }
    crate::compiler::native_cache::NativeDynamicInputSelector::new(environment_names, repository_paths)
        .map_err(|_| RemoteStoreError::integrity("remote dynamic-input selector is invalid"))
}

fn write_string(stream: &mut TcpStream, value: &str, maximum: usize) -> RemoteStoreResult<()> {
    if value.is_empty() || value.len() > maximum {
        return Err(RemoteStoreError::integrity(
            "remote coordinator string is empty or over its bound",
        ));
    }
    let length = u16::try_from(value.len())
        .map_err(|_| RemoteStoreError::integrity("remote coordinator string length is out of range"))?;
    stream.write_all(&length.to_le_bytes()).map_err(io_unavailable)?;
    stream.write_all(value.as_bytes()).map_err(io_unavailable)
}

fn read_string(stream: &mut TcpStream, maximum: usize) -> RemoteStoreResult<String> {
    let mut length = [0_u8; 2];
    stream.read_exact(&mut length).map_err(io_unavailable)?;
    let length = usize::from(u16::from_le_bytes(length));
    if length == 0 || length > maximum {
        return Err(RemoteStoreError::integrity(
            "remote coordinator string is empty or over its bound",
        ));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes).map_err(io_unavailable)?;
    String::from_utf8(bytes).map_err(|_| RemoteStoreError::integrity("remote coordinator string is not UTF-8"))
}

fn read_u64(stream: &mut TcpStream) -> RemoteStoreResult<u64> {
    let mut bytes = [0_u8; 8];
    stream.read_exact(&mut bytes).map_err(io_unavailable)?;
    Ok(u64::from_le_bytes(bytes))
}

fn response_error(code: u8) -> RemoteStoreError {
    let _ = code;
    RemoteStoreError::integrity("remote coordinator returned an unknown response")
}

fn io_unavailable(error: std::io::Error) -> RemoteStoreError {
    RemoteStoreError::unavailable(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_tokens_are_independent_fixed_hex_capabilities() {
        let read = random_token().expect("read token");
        let publish = random_token().expect("publish token");
        assert!(valid_token(&read));
        assert!(valid_token(&publish));
        assert_ne!(read, publish);
    }

    #[test]
    fn dynamic_input_selector_round_trips_over_the_coordinator_protocol() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        let selector = crate::compiler::native_cache::NativeDynamicInputSelector::new(
            vec!["CARGO_PKG_NAME".to_string()],
            vec![".config/target-matrix.json".to_string()],
        )
        .expect("selector");
        let sent = selector.clone();
        let address = listener.local_addr().expect("listener address");
        let writer = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).expect("connect coordinator test client");
            write_dynamic_input_selector(&mut stream, &sent).expect("write selector");
        });
        let (mut stream, _) = listener.accept().expect("accept coordinator test client");

        assert_eq!(
            read_dynamic_input_selector(&mut stream).expect("read selector"),
            selector
        );
        writer.join().expect("join coordinator test client");
    }

    #[test]
    fn capability_comparison_rejects_length_and_content_changes() {
        assert!(capability_matches("abc", "abc"));
        assert!(!capability_matches("abc", "abd"));
        assert!(!capability_matches("abc", "abc0"));
    }

    #[test]
    fn configured_stream_is_blocking() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        let mut client = TcpStream::connect(listener.local_addr().expect("listener address")).expect("connect client");
        let (mut server, _) = listener.accept().expect("accept client");
        server.set_nonblocking(true).expect("make server nonblocking");
        configure_stream(&server).expect("configure server stream");

        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            client.write_all(b"x").expect("write client byte");
        });
        let mut byte = [0_u8; 1];
        server.read_exact(&mut byte).expect("blocking read");
        writer.join().expect("join client writer");
        assert_eq!(byte, *b"x");
    }

    #[test]
    fn pack_reader_records_service_time_only_after_the_complete_body() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        let client = TcpStream::connect(listener.local_addr().expect("listener address")).expect("connect client");
        let (mut server, _) = listener.accept().expect("accept client");
        let writer = std::thread::spawn(move || {
            server.write_all(b"pack").expect("write pack");
            write_response_trailer(&mut server, 123_456).expect("write trailer");
        });
        let metrics = Arc::new(ClientMetrics::default());
        let mut reader = PackReader {
            stream: client,
            remaining: 4,
            metrics: Arc::clone(&metrics),
            finished: false,
        };

        let mut body = Vec::new();
        reader.read_to_end(&mut body).expect("read framed pack");
        writer.join().expect("join server writer");

        assert_eq!(body, b"pack");
        assert_eq!(metrics.service_elapsed_ns.load(Ordering::Relaxed), 123_456);
    }

    #[test]
    fn pack_reader_finish_consumes_the_trailer_without_an_eof_probe() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        let client = TcpStream::connect(listener.local_addr().expect("listener address")).expect("connect client");
        let (mut server, _) = listener.accept().expect("accept client");
        let writer = std::thread::spawn(move || {
            server.write_all(b"pack").expect("write pack");
            write_response_trailer(&mut server, 654_321).expect("write trailer");
        });
        let metrics = Arc::new(ClientMetrics::default());
        let mut reader = PackReader {
            stream: client,
            remaining: 4,
            metrics: Arc::clone(&metrics),
            finished: false,
        };

        let mut body = [0_u8; 4];
        reader.read_exact(&mut body).expect("read exact pack");
        assert_eq!(metrics.service_elapsed_ns.load(Ordering::Relaxed), 0);
        reader.finish().expect("finish framed pack");
        writer.join().expect("join server writer");

        assert_eq!(&body, b"pack");
        assert_eq!(metrics.service_elapsed_ns.load(Ordering::Relaxed), 654_321);
    }

    #[test]
    fn pack_reader_rejects_a_missing_service_trailer() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind listener");
        let client = TcpStream::connect(listener.local_addr().expect("listener address")).expect("connect client");
        let (mut server, _) = listener.accept().expect("accept client");
        let writer = std::thread::spawn(move || {
            server
                .write_all(b"packbad-trailer-data")
                .expect("write malformed frame");
        });
        let metrics = Arc::new(ClientMetrics::default());
        let mut reader = PackReader {
            stream: client,
            remaining: 4,
            metrics: Arc::clone(&metrics),
            finished: false,
        };

        let mut body = Vec::new();
        let error = reader.read_to_end(&mut body).expect_err("reject missing trailer");
        writer.join().expect("join server writer");

        assert_eq!(body, b"pack");
        assert!(error.to_string().contains("invalid trailer"));
        assert_eq!(metrics.service_elapsed_ns.load(Ordering::Relaxed), 0);
    }
}
