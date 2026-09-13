//! Real Cargo uploads against a disposable sparse registry.

use crate::helpers::{cargo_binary, cargo_command, finish_test, git_command};
use anyhow::{Context as _, Result, ensure};
use rscrypto::Sha256;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Default)]
struct RegistryState {
    uploads: BTreeMap<String, Vec<u8>>,
    index: BTreeMap<String, Value>,
    attempts: Vec<String>,
    credentials: BTreeMap<String, String>,
    required_credentials: BTreeMap<String, String>,
    reject: Option<String>,
    hide_index: bool,
    index_status: Option<u16>,
    stop: bool,
}

struct Registry {
    address: SocketAddr,
    state: Arc<Mutex<RegistryState>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Registry {
    fn new() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(RegistryState::default()));
        let shared = Arc::clone(&state);
        let worker = thread::spawn(move || {
            for stream in listener.incoming() {
                let stream = stream?;
                if shared.lock().expect("fixture registry lock").stop {
                    break;
                }
                serve(stream, &shared, address)?;
            }
            Ok(())
        });
        Ok(Self {
            address,
            state,
            worker: Some(worker),
        })
    }

    fn index_url(&self) -> String {
        format!("sparse+http://{}/index/", self.address)
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop = true;
        drop(TcpStream::connect(self.address));
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                assert_eq!(
                    result
                        .expect("fixture registry worker")
                        .map_err(|error| error.to_string()),
                    Ok(())
                );
            }
        }
    }
}

fn serve(stream: TcpStream, shared: &Mutex<RegistryState>, address: SocketAddr) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut input = BufReader::new(stream);
    let mut request = String::new();
    input.read_line(&mut request)?;
    let words: Vec<_> = request.split_whitespace().collect();
    ensure!(words.len() == 3, "invalid fixture request: {request:?}");
    let mut length = 0;
    let mut authorization = String::new();
    loop {
        let mut line = String::new();
        ensure!(input.read_line(&mut line)? > 0, "truncated HTTP headers");
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("authorization") {
                authorization = value.trim().to_owned();
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>()?;
            }
            if key.eq_ignore_ascii_case("expect") && value.trim() == "100-continue" {
                input.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                input.get_mut().flush()?;
            }
        }
    }
    ensure!(length <= 1024 * 1024, "oversized fixture upload");
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let mut state = shared.lock().expect("fixture registry lock");
    let (status, response) = match (words[0], words[1]) {
        ("GET", "/index/config.json") => (
            200,
            serde_json::to_vec(&json!({
                "api": format!("http://{address}"),
                "dl": format!("http://{address}/download/{{crate}}/{{version}}"),
            }))?,
        ),
        ("GET", path) if path.starts_with("/index/") => {
            let name = path.rsplit('/').next().context("index name")?;
            if let Some(status) = state.index_status {
                (status, Vec::new())
            } else if let Some(record) = state.index.get(name).filter(|_| !state.hide_index) {
                let mut data = serde_json::to_vec(record)?;
                data.push(b'\n');
                (200, data)
            } else {
                (404, Vec::new())
            }
        }
        ("GET", path) if path.starts_with("/download/") => {
            let name = path.split('/').nth(2).context("download name")?;
            (200, state.uploads.get(name).context("missing uploaded crate")?.clone())
        }
        ("PUT", "/api/v1/crates/new") => {
            let meta_size = usize::try_from(u32::from_le_bytes(body.get(..4).context("metadata size")?.try_into()?))?;
            let meta: Value = serde_json::from_slice(body.get(4..4 + meta_size).context("metadata")?)?;
            let offset = 4 + meta_size;
            let size = usize::try_from(u32::from_le_bytes(
                body.get(offset..offset + 4).context("crate size")?.try_into()?,
            ))?;
            let archive = body.get(offset + 4..).context("archive")?;
            ensure!(archive.len() == size, "invalid Cargo upload framing");
            let name = meta["name"].as_str().context("package name")?.to_owned();
            state.attempts.push(name.clone());
            state.credentials.insert(name.clone(), authorization.clone());
            if state
                .required_credentials
                .get(&name)
                .is_some_and(|expected| expected != &authorization)
            {
                (403, br#"{"errors":[{"detail":"expired package credential"}]}"#.to_vec())
            } else if state.reject.as_ref() == Some(&name) {
                (503, br#"{"errors":[{"detail":"fixture upload unavailable"}]}"#.to_vec())
            } else if state.uploads.contains_key(&name) {
                (409, br#"{"errors":[{"detail":"duplicate upload"}]}"#.to_vec())
            } else {
                let deps: Vec<_> = meta["deps"]
                    .as_array()
                    .context("dependencies")?
                    .iter()
                    .map(|dep| {
                        json!({
                            "name": dep.get("explicit_name_in_toml").unwrap_or_else(|| &dep["name"]),
                            "package": dep["name"], "req": dep["version_req"], "features": dep["features"],
                            "optional": dep["optional"], "default_features": dep["default_features"],
                            "target": dep["target"], "kind": dep["kind"], "registry": dep["registry"],
                        })
                    })
                    .collect();
                state.index.insert(
                    name.clone(),
                    json!({
                        "name": name, "vers": meta["vers"], "cksum": checksum(archive),
                        "deps": deps, "features": meta["features"], "yanked": false,
                    }),
                );
                state.uploads.insert(name, archive.to_vec());
                (200, br#"{"ok":true}"#.to_vec())
            }
        }
        _ => anyhow::bail!("unexpected fixture request: {request}"),
    };
    drop(state);
    let stream = input.get_mut();
    write!(
        stream,
        "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    )?;
    stream.write_all(&response)?;
    stream.flush()?;
    Ok(())
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    cargo_home: PathBuf,
    registry: Registry,
}

impl Fixture {
    fn new(dependent: bool) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("source");
        let cargo_home = temp.path().join("cargo-home");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(&cargo_home)?;
        fs::create_dir_all(temp.path().join("attempts"))?;
        let registry = Registry::new()?;
        fs::write(
            cargo_home.join("config.toml"),
            format!(
                "[registries.fixture]\nindex = {:?}\n[net]\nretry = 0\n[http]\nproxy = ''\n",
                registry.index_url(),
            ),
        )?;
        let members = if dependent { "['base', 'app']" } else { "['base']" };
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nmembers = {members}\nresolver = '3'\n"),
        )?;
        fs::write(root.join(".gitignore"), "/target\n")?;
        for name in if dependent { vec!["base", "app"] } else { vec!["base"] } {
            fs::create_dir_all(root.join(name).join("src"))?;
            let dependency = if name == "app" {
                "[dependencies]\nrail-fixture-base = {path = '../base', version = '0.1.0', registry = 'fixture'}\n"
            } else {
                ""
            };
            fs::write(
                root.join(name).join("Cargo.toml"),
                format!(
                    "[package]\nname = 'rail-fixture-{name}'\nversion = '0.1.0'\nedition = '2024'\ndescription = 'Disposable publication fixture'\nlicense = 'MIT'\n{dependency}",
                ),
            )?;
            fs::write(
                root.join(name).join("src/lib.rs"),
                if name == "app" {
                    "pub fn value() -> u32 { rail_fixture_base::value() }\n"
                } else {
                    "pub fn value() -> u32 { 1 }\n"
                },
            )?;
        }
        let fixture = Self {
            _root: temp,
            root,
            cargo_home,
            registry,
        };
        successful(fixture.cargo().args(["generate-lockfile", "--offline"]).output()?)?;
        successful(git_command(&fixture.root).args(["init", "-b", "main"]).output()?)?;
        successful(git_command(&fixture.root).args(["add", "."]).output()?)?;
        successful(git_command(&fixture.root).args(["commit", "-m", "fixture"]).output()?)?;
        Ok(fixture)
    }

    fn cargo(&self) -> Command {
        let mut command = cargo_command(&self.root);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if matches!(name.as_ref(), "RUSTUP_HOME" | "RUSTUP_TOOLCHAIN") {
                continue;
            }
            if name.starts_with("CARGO_") || name.starts_with("RUST") || name.starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .env("CARGO_HOME", &self.cargo_home)
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env("CARGO_REGISTRIES_FIXTURE_TOKEN", "fixture-token")
            .env("CARGO_RAIL_RELEASE_TOKEN", "fixture-token")
            .env(
                "CARGO_RAIL_RELEASE_ATTEMPT_DIRECTORY",
                self._root.path().join("attempts"),
            )
            .env("CARGO_NET_RETRY", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self._root.path().join("empty-git-config"));
        command
    }

    fn seal(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        successful(
            self.cargo()
                .args(["package", "--workspace", "--registry", "fixture", "--locked"])
                .output()?,
        )?;
        let mut packages = BTreeMap::new();
        for entry in fs::read_dir(self.root.join("target/package"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "crate") {
                let name = path
                    .file_stem()
                    .context("archive name")?
                    .to_str()
                    .context("UTF-8 name")?
                    .strip_suffix("-0.1.0")
                    .context("archive version")?
                    .to_owned();
                packages.insert(name, fs::read(path)?);
            }
        }
        Ok(packages)
    }

    fn publish(&self, sealed: &BTreeMap<String, Vec<u8>>, names: &[&str]) -> Command {
        let mut provider = vec![
            cargo_binary("cargo-rail")
                .to_str()
                .expect("UTF-8 binary path")
                .to_owned(),
            "cargo-rail-sealed-publish-v1".to_owned(),
            self.registry.index_url(),
        ];
        for (name, bytes) in sealed {
            provider.extend([name.clone(), "0.1.0".to_owned(), checksum(bytes)]);
        }
        let config = format!(
            "registries.fixture.credential-provider={}",
            serde_json::to_string(&provider).expect("provider arguments JSON")
        );
        let mut command = self.cargo();
        command.args([
            "publish",
            "--registry",
            "fixture",
            "--locked",
            "--no-verify",
            "--config",
            &config,
        ]);
        for name in names {
            command.args(["-p", name]);
        }
        command
    }
}

fn successful(output: Output) -> Result<Output> {
    ensure!(
        output.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

#[test]
fn cargo_first_publication_uploads_the_sealed_archive() {
    finish_test((|| {
        let fixture = Fixture::new(false)?;
        let sealed = fixture.seal()?;
        assert_eq!(
            sealed.keys().map(String::as_str).collect::<Vec<_>>(),
            ["rail-fixture-base"]
        );
        successful(fixture.publish(&sealed, &["rail-fixture-base"]).output()?)?;
        let state = fixture.registry.state.lock().expect("fixture registry lock");
        assert_eq!(state.uploads, sealed);
        assert_eq!(state.attempts, ["rail-fixture-base"]);
        assert_eq!(
            state.index["rail-fixture-base"]["cksum"],
            checksum(&sealed["rail-fixture-base"])
        );
        drop(state);
        let retry = fixture.publish(&sealed, &["rail-fixture-base"]).output()?;
        assert!(
            String::from_utf8_lossy(&retry.stderr).contains("already exists on `fixture` index"),
            "{retry:?}"
        );
        assert_eq!(
            fixture.registry.state.lock().expect("fixture registry lock").attempts,
            ["rail-fixture-base"]
        );
        Ok(())
    })());
}

#[test]
fn cargo_fresh_checkout_uploads_the_original_sealed_bytes() {
    finish_test((|| {
        let mut fixture = Fixture::new(true)?;
        let sealed = fixture.seal()?;
        let fresh = fixture._root.path().join("fresh-runner");
        successful(
            git_command(fixture._root.path())
                .arg("clone")
                .arg(&fixture.root)
                .arg(&fresh)
                .output()?,
        )?;
        fs::remove_dir_all(&fixture.root)?;
        fixture.root = fresh;
        successful(
            fixture
                .publish(&sealed, &["rail-fixture-base", "rail-fixture-app"])
                .output()?,
        )?;
        assert_eq!(
            fixture.registry.state.lock().expect("fixture registry lock").uploads,
            sealed
        );
        Ok(())
    })());
}

#[test]
fn cargo_index_timeout_preserves_the_accepted_upload() {
    finish_test((|| {
        let fixture = Fixture::new(false)?;
        let sealed = fixture.seal()?;
        fixture.registry.state.lock().expect("fixture registry lock").hide_index = true;
        let output = fixture.publish(&sealed, &["rail-fixture-base"]).output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("timed out"), "{output:?}");
        let mut state = fixture.registry.state.lock().expect("fixture registry lock");
        assert_eq!(state.uploads, sealed);
        assert_eq!(state.attempts, ["rail-fixture-base"]);
        state.hide_index = false;
        // The independent registry record survives Cargo's timeout. A transaction
        // must reconcile this checksum before deciding whether another upload is safe.
        assert_eq!(
            state.index["rail-fixture-base"]["cksum"],
            checksum(&sealed["rail-fixture-base"])
        );
        Ok(())
    })());
}

#[test]
fn cargo_workspace_validates_unpublished_dependencies_and_preserves_uncertain_partial_publication() {
    finish_test((|| {
        let fixture = Fixture::new(true)?;
        let sealed = fixture.seal()?;
        assert_eq!(sealed.len(), 2);
        assert!(
            fixture
                .registry
                .state
                .lock()
                .expect("fixture registry lock")
                .uploads
                .is_empty()
        );
        fixture.registry.state.lock().expect("fixture registry lock").reject = Some("rail-fixture-app".to_owned());
        let output = fixture
            .publish(&sealed, &["rail-fixture-base", "rail-fixture-app"])
            .output()?;
        assert!(!output.status.success(), "{output:?}");
        {
            let mut state = fixture.registry.state.lock().expect("fixture registry lock");
            assert_eq!(state.attempts, ["rail-fixture-base", "rail-fixture-app"]);
            assert_eq!(
                state.uploads.keys().map(String::as_str).collect::<Vec<_>>(),
                ["rail-fixture-base"]
            );
            assert_eq!(state.uploads["rail-fixture-base"], sealed["rail-fixture-base"]);
            state.reject = None;
        }
        let remaining = BTreeMap::from([("rail-fixture-app".to_owned(), sealed["rail-fixture-app"].clone())]);
        let retry = fixture.publish(&remaining, &["rail-fixture-app"]).output()?;
        assert!(!retry.status.success(), "{retry:?}");
        assert!(String::from_utf8_lossy(&retry.stderr).contains("already attempted"));
        let state = fixture.registry.state.lock().expect("fixture registry lock");
        assert_eq!(
            state.uploads.keys().map(String::as_str).collect::<Vec<_>>(),
            ["rail-fixture-base"]
        );
        assert_eq!(state.attempts, ["rail-fixture-base", "rail-fixture-app"]);
        Ok(())
    })());
}

#[test]
fn cargo_repackaging_changed_source_is_rejected_before_upload_even_with_ambient_credentials() {
    finish_test((|| {
        let fixture = Fixture::new(false)?;
        let sealed = fixture.seal()?;
        fs::write(fixture.root.join("base/src/lib.rs"), "pub fn value() -> u32 { 2 }\n")?;
        successful(git_command(&fixture.root).args(["add", "."]).output()?)?;
        successful(
            git_command(&fixture.root)
                .args(["commit", "-m", "changed after sealing"])
                .output()?,
        )?;
        let output = fixture.publish(&sealed, &["rail-fixture-base"]).output()?;
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Cargo upload does not match a sealed package"),
            "{output:?}"
        );
        let state = fixture.registry.state.lock().expect("fixture registry lock");
        assert!(state.uploads.is_empty());
        assert!(state.attempts.is_empty());
        Ok(())
    })());
}

#[test]
fn credential_protocol_rejects_malformed_or_unbound_requests_without_returning_the_token() {
    finish_test((|| {
        let permit = json!([
            "cargo-rail-sealed-publish-v1",
            "sparse+https://example.invalid/index/",
            "example",
            "1.0.0",
            "a".repeat(64)
        ]);
        let valid = json!({"v": 1, "kind": "get", "operation": "publish", "registry": {
            "index-url": "sparse+https://example.invalid/index/"
        }, "name": "example", "vers": "1.0.0", "cksum": "a".repeat(64), "args": permit});
        let mut cases = Vec::new();
        for (key, value) in [
            ("v", json!(2)),
            ("extra", json!(true)),
            ("operation", json!("yank")),
            ("name", json!("other")),
            ("vers", json!("2.0.0")),
            ("cksum", json!("b".repeat(64))),
            ("registry", json!({"index-url": "sparse+https://other.invalid/index/"})),
        ] {
            let mut request = valid.clone();
            request[key] = value;
            cases.push(request.to_string());
        }
        let mut duplicate = valid.clone();
        let args = duplicate["args"].as_array_mut().unwrap();
        args.extend(args[2..].to_vec());
        cases.push(duplicate.to_string());
        cases.push(valid.to_string().replacen('{', "{\"v\":1,", 1));
        cases.push(valid.to_string().replacen(
            "\"index-url\":",
            "\"index-url\":\"sparse+https://other.invalid/index/\",\"index-url\":",
            1,
        ));
        for request in cases {
            let mut child = Command::new(cargo_binary("cargo-rail"))
                .arg("--cargo-plugin")
                .env("CARGO_RAIL_RELEASE_TOKEN", "never-return-this-token")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            writeln!(child.stdin.take().context("provider stdin")?, "{request}")?;
            let output = child.wait_with_output()?;
            let lines: Vec<_> = output
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .collect();
            assert_eq!(lines.len(), 2, "{output:?}");
            assert_eq!(serde_json::from_slice::<Value>(lines[0])?, json!({"v": [1]}));
            let response: Value = serde_json::from_slice(lines[1])?;
            assert_eq!(response["Err"]["kind"], "other", "{request}: {response}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains("never-return-this-token"));
            assert!(output.stderr.is_empty(), "{output:?}");
        }
        Ok(())
    })());
}

#[test]
fn credential_provider_withholds_upload_authority_for_conflicting_yanked_or_unavailable_versions() {
    finish_test((|| {
        let fixture = Fixture::new(false)?;
        let sealed = fixture.seal()?;
        let digest = checksum(&sealed["rail-fixture-base"]);
        for (observed, yanked, status, expected) in [
            ("b".repeat(64), false, None, "registry version conflicts"),
            (digest.clone(), true, None, "registry version conflicts"),
            (digest.clone(), false, None, "sealed package is already published"),
            (
                digest.clone(),
                false,
                Some(429),
                "release registry observation is unavailable",
            ),
            (
                digest.clone(),
                false,
                Some(503),
                "release registry observation is unavailable",
            ),
        ] {
            {
                let mut state = fixture.registry.state.lock().expect("fixture registry lock");
                state.index.insert(
                    "rail-fixture-base".into(),
                    json!({
                        "name": "rail-fixture-base", "vers": "0.1.0", "cksum": observed,
                        "deps": [], "features": {}, "yanked": yanked,
                    }),
                );
                state.index_status = status;
            }
            let request = json!({
                "v": 1, "kind": "get", "operation": "publish",
                "registry": {"index-url": fixture.registry.index_url()},
                "name": "rail-fixture-base", "vers": "0.1.0", "cksum": digest,
                "args": ["cargo-rail-sealed-publish-v1", fixture.registry.index_url(),
                    "rail-fixture-base", "0.1.0", digest],
            });
            let mut child = Command::new(cargo_binary("cargo-rail"))
                .arg("--cargo-plugin")
                .env("CARGO_RAIL_RELEASE_TOKEN", "withheld-token")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            writeln!(child.stdin.take().context("provider stdin")?, "{request}")?;
            let output = child.wait_with_output()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains(expected), "{stdout}");
            assert!(!stdout.contains("withheld-token"), "{stdout}");
            assert!(output.stderr.is_empty(), "{output:?}");
        }
        let state = fixture.registry.state.lock().expect("fixture registry lock");
        assert!(state.attempts.is_empty());
        assert!(state.uploads.is_empty());
        Ok(())
    })());
}

#[cfg(unix)]
#[test]
fn cargo_publication_renews_external_credentials_for_each_sealed_package() {
    use std::os::unix::fs::PermissionsExt as _;
    finish_test((|| {
        let fixture = Fixture::new(true)?;
        let sealed = fixture.seal()?;
        let provider = fixture._root.path().join("renewing-provider");
        fs::write(
            &provider,
            r#"#!/usr/bin/env python3
import json, sys
assert sys.argv[1:] == ["--cargo-plugin"]
print(json.dumps({"v": [1]}), flush=True)
request = json.loads(sys.stdin.readline())
assert request["args"] == ["renew-per-package"]
print(json.dumps({"Ok": {"kind": "get", "token": "fresh-" + (request.get("name") or "read"),
                         "cache": "session", "operation_independent": True}}), flush=True)
"#,
        )?;
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o755))?;
        {
            let mut state = fixture.registry.state.lock().expect("fixture registry lock");
            for name in sealed.keys() {
                state.required_credentials.insert(name.clone(), format!("fresh-{name}"));
            }
        }
        let output = fixture
            .publish(&sealed, &["rail-fixture-base", "rail-fixture-app"])
            .env_remove("CARGO_RAIL_RELEASE_TOKEN")
            .env(
                "CARGO_RAIL_RELEASE_CREDENTIAL_PROVIDER",
                serde_json::to_string(&[provider.to_str().unwrap(), "renew-per-package"])?,
            )
            .output()?;
        successful(output)?;
        let state = fixture.registry.state.lock().expect("fixture registry lock");
        assert_eq!(state.uploads, sealed);
        assert_eq!(state.credentials, state.required_credentials);
        assert_eq!(state.attempts, ["rail-fixture-base", "rail-fixture-app"]);
        Ok(())
    })());
}
