//! Cargo credential protocol boundary for checksum-bound package uploads.
//!
//! Cargo computes the upload checksum before requesting publication credentials.
//! Arguments supplied by the executor bind that request to an exact registry and
//! package set. Credentials cannot be cached across requests or operations.

use std::collections::BTreeSet;
use std::io::{BufRead as _, Read as _, Write as _};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::registry::{RegistryObservation, RegistryObserver, valid_checksum, valid_name};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGES: usize = 4096;
const AUTHORIZATION_ARGUMENT: &str = "cargo-rail-sealed-publish-v1";
const TOKEN_ENV: &str = "CARGO_RAIL_RELEASE_TOKEN";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    v: u32,
    registry: Registry,
    kind: String,
    operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cksum: Option<String>,
    args: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Registry {
    index_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    headers: Vec<String>,
}

/// Dispatch Cargo's credential-provider invocation before ordinary CLI parsing.
///
/// The executor supplies `cargo-rail-sealed-publish-v1`, the exact index URL,
/// and one or more `(name, version, sha256)` triples as provider arguments.
/// `CARGO_RAIL_RELEASE_TOKEN` supplies process-local publication authority.
/// This boundary checks package bytes; it does not grant release authorization
/// or replace transaction, source, destination, and evidence validation.
#[doc(hidden)]
pub fn dispatch_credential_provider() -> Option<i32> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--cargo-plugin")) {
        return None;
    }
    // Cargo sends configured provider arguments in the JSON request.
    if arguments.next().is_some() {
        return Some(1);
    }
    let result = serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
    Some(if result.is_ok() { 0 } else { 1 })
}

fn serve(input: &mut impl std::io::BufRead, output: &mut impl std::io::Write) -> std::io::Result<()> {
    output.write_all(b"{\"v\":[1]}\n")?;
    output.flush()?;
    let mut bytes = Zeroizing::new(Vec::new());
    input.take(MAX_REQUEST_BYTES + 1).read_until(b'\n', &mut bytes)?;
    let result = if bytes.len() as u64 > MAX_REQUEST_BYTES || bytes.last() != Some(&b'\n') {
        Err("invalid or oversized Cargo credential request")
    } else {
        serde_json::from_slice::<Request>(&bytes)
            .map_err(|_| "invalid Cargo credential request")
            .and_then(|request| {
                authorize(&request)?;
                Ok(request)
            })
    };
    let result = result.and_then(|request| {
        let token = credential(&request)?;
        if token.is_empty() || token.chars().any(char::is_control) {
            return Err("release credential is invalid");
        }
        if request.operation == "publish" {
            persist_attempt(&request)?;
        }
        Ok(token)
    });
    match result {
        Ok(token) => {
            output.write_all(b"{\"Ok\":{\"kind\":\"get\",\"token\":")?;
            serde_json::to_writer(&mut *output, token.as_str())?;
            output.write_all(b",\"cache\":\"never\",\"operation_independent\":false}}\n")?;
        }
        Err(message) => {
            // `other` terminates Cargo authentication; it must not fall through
            // to an ambient provider after a package or registry mismatch.
            serde_json::to_writer(
                &mut *output,
                &serde_json::json!({
                    "Err": {"kind": "other", "message": message},
                }),
            )?;
            output.write_all(b"\n")?;
        }
    }
    output.flush()
}

fn credential(request: &Request) -> Result<Zeroizing<String>, &'static str> {
    if let Some(command) = std::env::var_os("CARGO_RAIL_RELEASE_CREDENTIAL_PROVIDER") {
        if std::env::var_os(TOKEN_ENV).is_some() {
            return Err("configure one release credential source");
        }
        return external_credential(request, &command.to_string_lossy());
    }
    if let Ok(token) = std::env::var(TOKEN_ENV) {
        return Ok(Zeroizing::new(token));
    }
    if request.registry.index_url != "https://github.com/rust-lang/crates.io-index" {
        return Err("stored crates.io credentials cannot authorize another registry");
    }
    let home = std::env::var_os("CARGO_RAIL_RELEASE_CARGO_HOME").ok_or("release credential is unavailable")?;
    let home = std::path::PathBuf::from(home);
    let extensionless = home.join("credentials");
    let path = if extensionless
        .try_exists()
        .map_err(|_| "Cargo credential storage is unavailable")?
    {
        extensionless
    } else {
        home.join("credentials.toml")
    };
    let mut bytes = Zeroizing::new(String::new());
    std::fs::File::open(path)
        .map_err(|_| "release credential is unavailable")?
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_string(&mut bytes)
        .map_err(|_| "Cargo credential storage is unreadable")?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("Cargo credential storage is oversized");
    }
    let document: toml_edit::DocumentMut = bytes.parse().map_err(|_| "Cargo credential storage is invalid")?;
    let token = document
        .get("registry")
        .and_then(|registry| registry.get("token"))
        .and_then(toml_edit::Item::as_str)
        .ok_or("release credential is unavailable")?;
    // This live read intentionally permits credential replacement between package requests.
    Ok(Zeroizing::new(token.to_owned()))
}

fn external_credential(request: &Request, configured: &str) -> Result<Zeroizing<String>, &'static str> {
    use std::io::Seek as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    if configured.len() > 8192 {
        return Err("release credential provider command is oversized");
    }
    let arguments: Vec<String> =
        serde_json::from_str(configured).map_err(|_| "release credential provider must be a JSON command array")?;
    let (program, arguments) = arguments
        .split_first()
        .ok_or("release credential provider command is empty")?;
    if program.is_empty() || arguments.iter().any(|argument| argument.contains('\0')) {
        return Err("release credential provider command is invalid");
    }
    let mut value = serde_json::to_value(request).map_err(|_| "release credential request cannot be encoded")?;
    value["args"] = serde_json::to_value(arguments).map_err(|_| "release credential arguments cannot be encoded")?;
    let mut input = tempfile::tempfile().map_err(|_| "release credential request storage is unavailable")?;
    serde_json::to_writer(&mut input, &value).map_err(|_| "release credential request cannot be written")?;
    input
        .write_all(b"\n")
        .and_then(|()| input.rewind())
        .map_err(|_| "release credential request cannot be written")?;
    let mut command = Command::new(program);
    command
        .arg("--cargo-plugin")
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("CARGO_RAIL_RELEASE_CREDENTIAL_PROVIDER")
        .env_remove(TOKEN_ENV);
    if let Some(home) = std::env::var_os("CARGO_RAIL_RELEASE_CARGO_HOME") {
        command.env("CARGO_HOME", home);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "release credential provider could not start")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("release credential provider output is unavailable")?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        let result = stdout
            .take(MAX_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        drop(sender.send(result));
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut status = None;
    let mut response = None;
    while status.is_none() || response.is_none() {
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            return Err("release credential provider timed out");
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|_| "release credential provider status is unavailable")?;
        }
        if response.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() as u64 <= MAX_REQUEST_BYTES => response = Some(bytes),
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    drop(child.kill());
                    drop(child.wait());
                    return Err("release credential provider response is invalid or oversized");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() || response.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if !status.is_some_and(|status| status.success()) {
        return Err("release credential provider failed");
    }
    let bytes = response.ok_or("release credential provider returned no response")?;
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let hello: serde_json::Value = super::contract::decode(lines.next().ok_or("credential provider hello is missing")?)
        .map_err(|_| "credential provider hello is invalid")?;
    if hello != serde_json::json!({"v":[1]}) {
        return Err("release credential provider protocol is unsupported");
    }
    let mut response: serde_json::Value =
        super::contract::decode(lines.next().ok_or("credential provider response is missing")?)
            .map_err(|_| "credential provider response is invalid")?;
    if lines.any(|line| !line.is_empty()) || response.get("Err").is_some() {
        return Err("release credential provider refused this request");
    }
    let result = response
        .get("Ok")
        .filter(|value| value.get("kind").and_then(serde_json::Value::as_str) == Some("get"))
        .ok_or("release credential provider returned the wrong operation")?;
    if !result.get("token").is_some_and(serde_json::Value::is_string) {
        return Err("release credential provider returned no token");
    }
    // The outer guard owns caching regardless of the inner provider's preference.
    match response.pointer_mut("/Ok/token").map(serde_json::Value::take) {
        Some(serde_json::Value::String(token)) => Ok(Zeroizing::new(token)),
        _ => Err("release credential provider returned no token"),
    }
}

fn persist_attempt(request: &Request) -> Result<(), &'static str> {
    let directory = std::env::var_os("CARGO_RAIL_RELEASE_ATTEMPT_DIRECTORY")
        .map(std::path::PathBuf::from)
        .ok_or("release attempt storage is unavailable")?;
    let directory =
        crate::utils::canonicalize_existing(&directory).map_err(|_| "release attempt storage is unavailable")?;
    let name = request.name.as_deref().ok_or("missing package name")?;
    let version = request.vers.as_deref().ok_or("missing package version")?;
    let path = directory.join(format!("{name}@{version}.json"));
    // A partial marker also blocks retry. Credentials are not returned until
    // both the file and its directory entry have crossed the durable boundary.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(
            |_| "package upload was already attempted or its evidence cannot be persisted; reconcile before retrying",
        )?;
    let mut entropy = [0u8; 32];
    getrandom::fill(&mut entropy).map_err(|_| "upload attempt identity is unavailable")?;
    let record = super::packages::PublicationAttempt {
        identity: crate::source::ContentDigest::sha256(&entropy).to_string(),
        registry: request.registry.index_url.clone(),
        name: name.to_owned(),
        version: version.to_owned(),
        sha256: request.cksum.clone().ok_or("missing upload checksum")?,
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| "release attempt cannot be encoded")?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "release attempt cannot be persisted")?;
    crate::utils::sync_parent_directory(&directory).map_err(|_| "release attempt cannot be persisted")?;
    if let Some(path) = std::env::var_os("CARGO_RAIL_RELEASE_STATE_PATH") {
        let path = std::path::PathBuf::from(path);
        let mut state = super::state::ReleaseState::load_for_recovery(&path)
            .map_err(|_| "original release record is unavailable")?;
        let seal = state
            .package_seal
            .as_ref()
            .ok_or("original package seal is unavailable")?;
        if state.intent.skip_publish
            || seal.registry_index != request.registry.index_url
            || !seal.packages.iter().any(|archive| {
                archive.name == name
                    && archive.version == version
                    && request.cksum.as_deref() == Some(archive.sha256.as_str())
            })
        {
            return Err("Cargo request does not match the original release record");
        }
        let index = state
            .crate_index(name)
            .map_err(|_| "original package progress is unavailable")?;
        if state.crates[index].publication.status != super::state::StepStatus::Pending {
            return Err("package publication was already attempted; reconcile its original record");
        }
        state.crates[index].publication.status = super::state::StepStatus::InProgress;
        state.crates[index].publication.object = request.cksum.clone();
        state.crates[index].publication_attempt = Some(record.identity);
        state
            .save(&path)
            .map_err(|_| "release upload attempt could not be retained before credential issuance")?;
    }
    Ok(())
}

fn authorize(request: &Request) -> Result<(), &'static str> {
    let args = &request.args;
    if request.v != 1 || request.kind != "get" {
        return Err("unsupported Cargo credential operation");
    }
    if args.first().map(String::as_str) != Some(AUTHORIZATION_ARGUMENT)
        || args.len() < 5
        || !(args.len() - 2).is_multiple_of(3)
        || (args.len() - 2) / 3 > MAX_PACKAGES
    {
        return Err("invalid sealed package authorization");
    }
    let index = &args[1];
    if index != &request.registry.index_url
        || !(index.starts_with("sparse+https://")
            || index.starts_with("sparse+http://")
            || index.starts_with("https://"))
        || index.chars().any(char::is_control)
        || index.contains(['@', '?', '#'])
    {
        return Err("release registry does not match sealed package authorization");
    }
    // These optional Cargo fields describe authentication, not destination authority.
    let _ = (&request.registry.name, &request.registry.headers);
    let mut identities = BTreeSet::new();
    let mut matched = false;
    for [name, version, checksum] in args[2..].as_chunks::<3>().0 {
        if !valid_name(name)
            || semver::Version::parse(version).is_err()
            || !valid_checksum(checksum)
            || !identities.insert((name, version))
        {
            return Err("invalid or duplicate sealed package identity");
        }
        matched |= request.name.as_ref() == Some(name)
            && request.vers.as_ref() == Some(version)
            && request.cksum.as_ref() == Some(checksum);
    }
    match request.operation.as_str() {
        "read" if request.name.is_none() && request.vers.is_none() && request.cksum.is_none() => Ok(()),
        "publish" if matched => {
            let index = if index == "https://github.com/rust-lang/crates.io-index" {
                "https://index.crates.io/"
            } else if index.starts_with("sparse+") {
                index
            } else {
                return Err("release registry has no qualified observation adapter");
            };
            let observer = RegistryObserver::new(index).map_err(|_| "release registry observation is unavailable")?;
            match observer.observe(
                request.name.as_deref().ok_or("missing package name")?,
                request.vers.as_deref().ok_or("missing package version")?,
                request.cksum.as_deref().ok_or("missing package checksum")?,
            ) {
                RegistryObservation::Absent => Ok(()),
                RegistryObservation::Matching => {
                    Err("sealed package is already published; reconcile the release before retrying")
                }
                RegistryObservation::Conflicting { .. } => {
                    Err("registry version conflicts with the sealed package or is yanked")
                }
                RegistryObservation::Unavailable { .. } => {
                    Err("release registry observation is unavailable; no upload credential was issued")
                }
            }
        }
        "publish" => Err("Cargo upload does not match a sealed package name, version, and checksum"),
        _ => Err("unsupported Cargo credential operation"),
    }
}
