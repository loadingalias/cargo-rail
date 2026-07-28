//! Integration tests for `cargo rail run` command
//!
//! Tests the smart test runner with change detection

use crate::helpers::{TestWorkspace, git, run_cargo_rail, run_cargo_rail_with_env};
use anyhow::Result;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

fn generate_lockfile(workspace: &Path) -> Result<()> {
  generate_lockfile_with_env(workspace, &[])
}

fn generate_lockfile_with_env(workspace: &Path, environment: &[(&str, &str)]) -> Result<()> {
  let mut command = std::process::Command::new("cargo");
  command.current_dir(workspace).arg("generate-lockfile");
  for (name, value) in environment {
    command.env(name, value);
  }
  let output = command.output()?;
  anyhow::ensure!(
    output.status.success(),
    "lockfile generation failed:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

#[cfg(target_os = "macos")]
fn assert_materialized_output_matches_manifest(root: &Path, manifest: &serde_json::Value) -> Result<()> {
  fn collect(root: &Path, directory: &Path, paths: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
      let entry = entry?;
      let path = entry.path();
      let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
      paths.push(relative);
      if entry.file_type()?.is_dir() {
        collect(root, &path, paths)?;
      }
    }
    Ok(())
  }

  let entries = manifest["entries"].as_array().expect("output manifest entries");
  let mut expected_paths = entries
    .iter()
    .map(|entry| entry["path"].as_str().expect("manifest path").to_string())
    .collect::<Vec<_>>();
  let mut actual_paths = Vec::new();
  collect(root, root, &mut actual_paths)?;
  expected_paths.sort();
  actual_paths.sort();
  assert_eq!(
    actual_paths, expected_paths,
    "materialized tree must contain exactly the declared paths"
  );

  for entry in entries {
    let relative = entry["path"].as_str().expect("manifest path");
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    match entry["kind"].as_str().expect("manifest entry kind") {
      "directory" => assert!(metadata.is_dir() && !metadata.file_type().is_symlink()),
      "file" => {
        assert!(metadata.is_file() && !metadata.file_type().is_symlink());
        let bytes = fs::read(&path)?;
        let digest = Sha256::digest(&bytes)
          .iter()
          .map(|byte| format!("{byte:02x}"))
          .collect::<String>();
        assert_eq!(bytes.len() as u64, entry["bytes"].as_u64().expect("file byte count"));
        assert_eq!(
          format!("sha256:{digest}"),
          entry["digest"].as_str().expect("file digest"),
          "restored file bytes differ at {relative}"
        );
      }
      "symlink" => {
        assert!(metadata.file_type().is_symlink());
        assert_eq!(
          fs::read_link(&path)?.to_string_lossy(),
          entry["target"].as_str().expect("symlink target")
        );
      }
      kind => panic!("unsupported output manifest kind {kind}"),
    }
    use std::os::unix::fs::PermissionsExt as _;
    if entry["kind"] != "symlink" {
      assert_eq!(
        metadata.permissions().mode() & 0o7777,
        entry["mode"].as_u64().expect("output mode") as u32,
        "restored mode differs at {relative}"
      );
    }
  }
  Ok(())
}

#[derive(Clone, Copy, Default)]
struct RegistryObservations {
  requests: usize,
  before_fetch_boundary: bool,
  during_build: bool,
}

#[derive(Default)]
struct RegistryState {
  active_workspace: Option<std::path::PathBuf>,
  observations: RegistryObservations,
  failure: Option<String>,
}

struct SparseRegistry {
  index: String,
  wake_address: std::net::SocketAddr,
  state: Arc<Mutex<RegistryState>>,
  stop: Arc<AtomicBool>,
  threads: Vec<JoinHandle<()>>,
  connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl SparseRegistry {
  fn start(crate_archive: Vec<u8>, checksum: &str) -> Result<Self> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let index = format!("sparse+http://{address}/");
    let download = format!("http://{address}/api/v1/crates/{{crate}}/{{version}}/download");
    let config: Arc<[u8]> = serde_json::to_vec(&serde_json::json!({ "dl": download }))?.into();
    let package = serde_json::to_vec(&serde_json::json!({
      "name": "external-dep",
      "vers": "0.1.0",
      "deps": [],
      "cksum": checksum,
      "features": {},
      "yanked": false,
      "links": null
    }))?;
    let mut index_entry = package;
    index_entry.push(b'\n');
    let index_entry: Arc<[u8]> = index_entry.into();
    let crate_archive: Arc<[u8]> = crate_archive.into();
    let state = Arc::new(Mutex::new(RegistryState::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let connections = Arc::new(Mutex::new(Vec::new()));
    let config = Arc::clone(&config);
    let index_entry = Arc::clone(&index_entry);
    let crate_archive = Arc::clone(&crate_archive);
    let thread_state = Arc::clone(&state);
    let thread_stop = Arc::clone(&stop);
    let thread_connections = Arc::clone(&connections);
    let threads = vec![std::thread::spawn(move || {
      loop {
        match listener.accept() {
          Ok((stream, _)) => {
            if thread_stop.load(Ordering::Acquire) {
              break;
            }
            let config = Arc::clone(&config);
            let index_entry = Arc::clone(&index_entry);
            let crate_archive = Arc::clone(&crate_archive);
            let connection_state = Arc::clone(&thread_state);
            let connection = std::thread::spawn(move || {
              if let Err(error) =
                serve_sparse_registry_request(stream, &config, &index_entry, &crate_archive, &connection_state)
                && let Ok(mut state) = connection_state.lock()
              {
                state.failure = Some(error.to_string());
              }
            });
            if let Ok(mut connections) = thread_connections.lock() {
              connections.push(connection);
            } else {
              break;
            }
          }
          Err(error) => {
            if let Ok(mut state) = thread_state.lock() {
              state.failure = Some(error.to_string());
            }
            break;
          }
        }
      }
    })];
    Ok(Self {
      index,
      wake_address: address,
      state,
      stop,
      threads,
      connections,
    })
  }

  fn begin(&self, workspace: &Path) -> Result<()> {
    let mut state = self
      .state
      .lock()
      .map_err(|_| anyhow::anyhow!("sparse registry state lock poisoned"))?;
    state.active_workspace = Some(workspace.to_path_buf());
    state.observations = RegistryObservations::default();
    state.failure = None;
    Ok(())
  }

  fn observations(&self) -> Result<RegistryObservations> {
    let state = self
      .state
      .lock()
      .map_err(|_| anyhow::anyhow!("sparse registry state lock poisoned"))?;
    if let Some(error) = &state.failure {
      anyhow::bail!("sparse registry server failed: {error}");
    }
    Ok(state.observations)
  }
}

impl Drop for SparseRegistry {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Release);
    let _ = TcpStream::connect(self.wake_address);
    for thread in self.threads.drain(..) {
      let _ = thread.join();
    }
    if let Ok(mut connections) = self.connections.lock() {
      for connection in connections.drain(..) {
        let _ = connection.join();
      }
    }
  }
}

fn serve_sparse_registry_request(
  mut stream: TcpStream,
  config: &[u8],
  index_entry: &[u8],
  crate_archive: &[u8],
  state: &Mutex<RegistryState>,
) -> std::io::Result<()> {
  // Cargo may connect before its request bytes are runnable on a loaded test
  // host. Keep each connection blocking and bounded independently.
  stream.set_nonblocking(false)?;
  // Cargo may open a registry connection before a heavily parallel test host
  // schedules its request headers. Keep the timeout bounded, but do not turn
  // ordinary scheduler pressure into an empty HTTP reply.
  stream.set_read_timeout(Some(Duration::from_secs(10)))?;
  let mut request = Vec::with_capacity(1024);
  while request.len() < 16 * 1024 {
    let mut chunk = [0u8; 1024];
    let bytes = stream.read(&mut chunk)?;
    if bytes == 0 {
      if request.is_empty() {
        return Ok(());
      }
      break;
    }
    request.extend_from_slice(&chunk[..bytes]);
    if request.windows(4).any(|window| window == b"\r\n\r\n") {
      break;
    }
  }
  let request = String::from_utf8_lossy(&request);
  let path = request
    .lines()
    .next()
    .and_then(|line| line.split_whitespace().nth(1))
    .and_then(|path| path.split('?').next())
    .unwrap_or("/");
  if let Ok(mut state) = state.lock() {
    state.observations.requests += 1;
    if let Some(workspace) = state.active_workspace.clone() {
      let inventories = workspace.join("target/cargo-rail/hermetic/inventories");
      if !inventories.is_dir() {
        state.observations.before_fetch_boundary = true;
      }
      let runs = workspace.join("target/cargo-rail/hermetic/runs");
      if fs::read_dir(runs).is_ok_and(|mut entries| entries.next().is_some()) {
        state.observations.during_build = true;
      }
    }
  }
  let (status, content_type, body) = match path {
    "/config.json" => ("200 OK", "application/json", config),
    "/ex/te/external-dep" => ("200 OK", "application/json", index_entry),
    "/api/v1/crates/external-dep/0.1.0/download" => ("200 OK", "application/octet-stream", crate_archive),
    _ => ("404 Not Found", "text/plain", &b"not found\n"[..]),
  };
  write!(
    stream,
    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
    body.len()
  )?;
  stream.write_all(body)
}

#[test]
fn test_runner_basic_change_detection() -> Result<()> {
  // Setup workspace with two crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add lib-a and lib-b")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a source
  ws.modify_file("lib-a", "src/lib.rs", "pub fn modified() -> u32 { 42 }")?;
  ws.commit("Modify lib-a")?;

  // Run test with change detection
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(output.status.success(), "Run command should succeed");
  assert!(
    stderr.contains("testing") && stderr.contains("crates"),
    "Should invoke runner"
  );
  assert!(
    stderr.contains("lib-a") && stderr.contains("lib-b"),
    "Should include dependent crates"
  );

  Ok(())
}

#[test]
fn test_runner_no_changes() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create baseline
  git(&ws.path, &["branch", "baseline"])?;

  // Run test with no changes
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should skip all tests
  assert!(
    stdout.contains("no test targets"),
    "Should skip tests when no changes. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_docs_only_change() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only README
  ws.modify_file("lib-a", "README.md", "# Updated Documentation\n")?;
  ws.commit("Update README")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Documentation-only changes might still trigger tests depending on implementation
  // The key is that it should be detected and handled appropriately
  assert!(
    output.status.success(),
    "Run command should succeed. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_ci_only_change_skips_tests() -> Result<()> {
  let ws = TestWorkspace::new_named("test-ci-only")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  std::fs::create_dir_all(ws.path.join(".github/workflows"))?;
  std::fs::write(ws.path.join(".github/workflows/ci.yml"), "name: CI\n")?;
  ws.commit("ci change only")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "Run command should succeed");
  assert!(
    stdout.contains("no test targets"),
    "CI-only changes should not trigger crate test execution. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_rejects_infra_surface() -> Result<()> {
  let ws = TestWorkspace::new_named("run-reject-infra-surface")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--surface", "infra"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let combined = format!("{stdout}\n{stderr}");

  assert!(!output.status.success(), "infra surface should be rejected");
  assert!(
    combined.contains("planner output") && combined.contains("run.action.<name>.when"),
    "expected planner-output rejection. Output:\n{}",
    combined
  );

  Ok(())
}

#[test]
fn test_runner_transitive_dependencies() -> Result<()> {
  // Setup: lib-a <- lib-b <- lib-c (chain)
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.add_crate("lib-c", "0.1.0", &[("lib-b", r#"{ path = "../lib-b" }"#)])?;
  ws.commit("Add dependency chain")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify lib-a (root of chain)
  ws.modify_file("lib-a", "src/lib.rs", "pub fn chain_changed() {}")?;
  ws.commit("Modify lib-a")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // All three should be tested (lib-a changed, lib-b and lib-c depend on it)
  assert!(
    stderr.contains("lib-a"),
    "Should test lib-a (directly changed). Output:\n{}",
    stderr
  );
  assert!(
    stderr.contains("lib-b"),
    "Should test lib-b (depends on lib-a). Output:\n{}",
    stderr
  );
  assert!(
    stderr.contains("lib-c"),
    "Should test lib-c (transitive dependent). Output:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_runner_isolated_change() -> Result<()> {
  // Setup: two independent crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add independent crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify only lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn isolated_change() {}")?;
  ws.commit("Modify lib-a only")?;

  // Run test
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should test only lib-a, not lib-b
  assert!(
    stderr.contains("lib-a"),
    "Should test lib-a (changed). Output:\n{}",
    stderr
  );
  assert!(
    !stderr.contains("lib-b"),
    "Should NOT list lib-b as affected. Output:\n{}",
    stderr
  );

  Ok(())
}

#[test]
fn test_runner_with_explain() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn explained() {}")?;
  ws.commit("Modify lib-a")?;

  // Run with --explain flag
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline", "--explain"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show detailed explanation
  assert!(
    stdout.contains("surfaces:") || stdout.contains("why:") || stdout.contains("explain:"),
    "Should show detailed explanation with --explain. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_auto_detect_base_ref() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create a base branch if it does not exist.
  let existing_base_branch = git(&ws.path, &["branch", "--list", "base-branch"])?;
  if String::from_utf8_lossy(&existing_base_branch.stdout).trim().is_empty() {
    git(&ws.path, &["branch", "base-branch"])?;
  }
  git(&ws.path, &["checkout", "-b", "feature-branch"])?;

  ws.modify_file(
    "lib-a",
    "src/lib.rs",
    r#"
    pub fn feature_work() {}
    #[cfg(test)]
    mod tests {
        #[test]
        fn test_feature_work() {
            super::feature_work();
        }
    }
    "#,
  )?;
  ws.commit("Feature work")?;

  // Run without --since (should auto-detect base ref or use HEAD)
  let output = run_cargo_rail(&ws.path, &["rail", "run"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should successfully run (whether it detects changes or not is okay)
  assert!(
    output.status.success(),
    "Should successfully handle auto-detect. Output:\n{}\nStderr:\n{}",
    stdout,
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_runner_ignores_manifest_formatting_only_changes() -> Result<()> {
  // Formatting-only Cargo.toml changes have no compilation impact.
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify Cargo.toml (add a comment or metadata)
  let cargo_toml = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/Cargo.toml"),
    format!("# Modified\n{}", cargo_toml),
  )?;
  ws.commit("Modify Cargo.toml")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "formatting-only plan should succeed");
  assert!(
    stdout.contains("no test targets"),
    "formatting-only Cargo.toml changes must not trigger testing. Output:\n{stdout}"
  );

  Ok(())
}

#[test]
fn test_runner_test_file_changes() -> Result<()> {
  // Test that test file changes are detected
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Add an integration test
  std::fs::create_dir_all(ws.path.join("crates/lib-a/tests"))?;
  std::fs::write(
    ws.path.join("crates/lib-a/tests/integration_test.rs"),
    "#[test]\nfn new_test() { assert!(true); }",
  )?;
  ws.commit("Add integration test")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Test file changes should trigger testing
  assert!(
    stderr.contains("lib-a"),
    "Test file changes should trigger testing. Output:\n{}",
    stderr
  );

  Ok(())
}

/// Test --all flag runs all tests regardless of changes
#[test]
fn test_runner_all_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("test-all")?;
  ws.add_crate("all-a", "0.1.0", &[])?;
  ws.add_crate("all-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Run with --all flag (skip change detection)
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(output.status.success(), "test --all should succeed");
  assert!(
    stderr.contains("testing") || stderr.contains("all-a") || stderr.contains("all-b"),
    "Should run runs for all crates. Output:\n{}",
    stderr
  );

  Ok(())
}

/// Test --skip-nextest flag forces use of cargo test
#[test]
fn test_runner_skip_nextest_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("test-skip-nextest")?;
  ws.add_crate("nextest-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  git(&ws.path, &["branch", "baseline"])?;

  // Modify crate
  ws.modify_file("nextest-crate", "src/lib.rs", "pub fn test_fn() { }")?;
  ws.commit("Modify crate")?;

  // Run with --skip-nextest flag
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--since", "baseline", "--skip-nextest"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  // Should succeed and use cargo test (not nextest)
  // The output should mention cargo test or not mention nextest in the runner selection
  assert!(
    output.status.success(),
    "test --skip-nextest should succeed. stderr: {}",
    stderr
  );

  // When nextest is disabled, it should use cargo test
  // The absence of "nextest" in output confirms this (or presence of "cargo test")
  let combined = format!("{}{}", stdout, stderr);
  assert!(
    !combined.contains("cargo nextest") || combined.contains("cargo test"),
    "Should use cargo test not nextest. Output:\n{}",
    combined
  );

  Ok(())
}

/// Test --all combined with --skip-nextest
#[test]
fn test_runner_all_skip_nextest() -> Result<()> {
  let ws = TestWorkspace::new_named("test-all-skip-nextest")?;
  ws.add_crate("combo-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  // Run with both flags
  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--skip-nextest"])?;

  assert!(
    output.status.success(),
    "test --all --skip-nextest should succeed. stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn test_runner_cargo_backend_renders_typed_arguments_in_exact_order() -> Result<()> {
  let ws = TestWorkspace::new_named("test-cargo-typed-arguments")?;
  ws.add_crate("typed-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "test",
      "--dry-run",
      "--cargo-test-arg=--all-features",
      "--test-filter",
      "selected_test",
      "--",
      "--nocapture",
      "--test-threads=1",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "typed Cargo invocation should render");
  assert!(
    stdout.contains("test: cargo test -p typed-crate --all-features selected_test -- --nocapture --test-threads=1"),
    "Cargo options, filter, separator, and harness args must keep their domains. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_nextest_backend_renders_typed_arguments_in_exact_order() -> Result<()> {
  let ws = TestWorkspace::new_named("test-nextest-typed-arguments")?;
  ws.add_crate("typed-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "test",
      "--dry-run",
      "--test-runner",
      "nextest",
      "--nextest-arg=-P",
      "--nextest-arg=default",
      "--test-filter",
      "selected_test",
      "--",
      "--nocapture",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "typed nextest invocation should render");
  assert!(
    stdout.contains("test: cargo nextest run -p typed-crate -P default selected_test -- --nocapture"),
    "nextest options, filter, separator, and harness args must keep their domains. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_builtin_actions_render_byte_exact_argv_in_request_order() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-builtin-action-argv")?;
  let action_crate = ws.add_crate("action-crate", "0.1.0", &[])?;
  let manifest = std::fs::read_to_string(action_crate.join("Cargo.toml"))?.replace(
    "authors.workspace = true",
    "authors.workspace = true\nrust-version = \"1.97.1\"",
  );
  std::fs::write(action_crate.join("Cargo.toml"), manifest)?;
  ws.commit("Add crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "build",
      "--surface",
      "test",
      "--surface",
      "bench",
      "--surface",
      "docs",
      "--action",
      "format",
      "--action",
      "lint",
      "--action",
      "msrv",
      "--action",
      "package",
      "--action",
      "audit",
      "--action",
      "distribution",
      "--test-runner",
      "cargo",
      "--dry-run",
    ],
  )?;

  assert!(
    output.status.success(),
    "built-in action preview should succeed. Stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    String::from_utf8(output.stdout)?,
    concat!(
      "build: cargo check --workspace\n",
      "test: cargo test -p action-crate\n",
      "bench: cargo bench --workspace\n",
      "docs: cargo doc --workspace --no-deps\n",
      "format: cargo fmt --all --check\n",
      "lint: cargo clippy --workspace --all-targets --all-features -- -D warnings\n",
      "msrv: cargo +1.97.1 check --workspace --all-targets --all-features --locked\n",
      "package: cargo package --workspace --locked\n",
      "audit: cargo deny check all\n",
      "distribution: cargo build --workspace --release --locked\n",
    ),
    "built-in action order and argv are a byte-exact CLI contract"
  );

  Ok(())
}

#[test]
fn test_runner_rejects_backend_argument_mismatch_before_spawn() -> Result<()> {
  let ws = TestWorkspace::new_named("test-backend-argument-mismatch")?;
  ws.add_crate("typed-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "docs",
      "--surface",
      "test",
      "--test-runner",
      "cargo",
      "--nextest-arg=-P",
    ],
  )?;
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert_eq!(output.status.code(), Some(2), "backend mismatch is a usage error");
  assert!(
    stderr.contains("nextest options cannot be used with cargo test"),
    "diagnostic should name both incompatible domains. Stderr:\n{}",
    stderr
  );
  assert!(
    !ws.path.join("target/doc").exists(),
    "every action must expand successfully before an earlier subprocess can start"
  );

  Ok(())
}

#[test]
fn test_runner_build_surface_uses_planner_selected_packages() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-selected-packages")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;

  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed_for_build() {}")?;
  ws.commit("Modify lib-a")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "baseline",
      "--surface",
      "build",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check -p lib-a"),
    "build should target selected crate(s). Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains(" -p lib-b"),
    "build should not include unaffected crates. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "partial selection should not use --workspace. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_refines_optional_impact_for_the_action_feature_view() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-optional-feature-view")?;
  ws.add_crate("optional-a", "0.1.0", &[])?;
  let optional_b = ws.add_crate("optional-b", "0.1.0", &[])?;
  std::fs::write(
    optional_b.join("Cargo.toml"),
    r#"[package]
name = "optional-b"
version = "0.1.0"
edition.workspace = true

[features]
default = []
with-a = ["dep:optional-a"]

[dependencies]
optional-a = { path = "../optional-a", optional = true }
"#,
  )?;
  ws.commit("add optional dependency")?;
  ws.modify_file("optional-a", "src/lib.rs", "pub fn changed() {}\n")?;

  let default = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "HEAD",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
    ],
  )?;
  assert!(default.status.success(), "default action plan failed");
  let default: serde_json::Value = serde_json::from_slice(&default.stdout)?;
  assert_eq!(
    default["actions"][0]["selected_packages"],
    serde_json::json!(["optional-a"])
  );

  let all_features = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "HEAD",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--all-features",
    ],
  )?;
  assert!(
    all_features.status.success(),
    "all-feature action plan failed: {}",
    String::from_utf8_lossy(&all_features.stderr)
  );
  let all_features: serde_json::Value = serde_json::from_slice(&all_features.stdout)?;
  assert_eq!(
    all_features["actions"][0]["selected_packages"],
    serde_json::json!(["optional-a", "optional-b"])
  );
  assert_eq!(
    all_features["actions"][0]["resolution_views"][0]["features"]["all_features"],
    true
  );
  Ok(())
}

#[test]
fn test_runner_refines_target_impact_for_the_action_target_view() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-target-view")?;
  ws.add_crate("target-a", "0.1.0", &[])?;
  let target_b = ws.add_crate("target-b", "0.1.0", &[])?;
  std::fs::write(
    target_b.join("Cargo.toml"),
    r#"[package]
name = "target-b"
version = "0.1.0"
edition.workspace = true

[target.'thumbv7em-none-eabihf'.dependencies]
target-a = { path = "../target-a" }
"#,
  )?;
  ws.commit("add target dependency")?;
  ws.modify_file("target-a", "src/lib.rs", "pub fn changed() {}\n")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "HEAD",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--target",
      "thumbv7em-none-eabihf",
    ],
  )?;
  assert!(
    output.status.success(),
    "target action plan failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(
    json["actions"][0]["selected_packages"],
    serde_json::json!(["target-a", "target-b"])
  );
  assert_eq!(
    json["actions"][0]["resolution_views"][0]["target"],
    "thumbv7em-none-eabihf"
  );
  Ok(())
}

#[test]
fn test_runner_activates_target_only_lock_impact_in_the_action_view() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-target-lock-view")?;
  let consumer = ws.add_crate("target-consumer", "0.1.0", &[])?;
  ws.add_crate("target-unrelated", "0.1.0", &[])?;
  std::fs::write(
    consumer.join("Cargo.toml"),
    r#"[package]
name = "target-consumer"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[target.'thumbv7em-none-eabihf'.dependencies]
anyhow.workspace = true
"#,
  )?;
  let lockfile = std::process::Command::new("cargo")
    .current_dir(&ws.path)
    .args(["generate-lockfile", "--offline"])
    .output()?;
  assert!(
    lockfile.status.success(),
    "offline lockfile generation failed: {}",
    String::from_utf8_lossy(&lockfile.stderr)
  );
  let lock_path = ws.path.join("Cargo.lock");
  let valid_lock = std::fs::read_to_string(&lock_path)?;
  let checksum_prefix = "checksum = \"";
  let checksum_start = valid_lock
    .find(checksum_prefix)
    .map(|index| index + checksum_prefix.len())
    .ok_or_else(|| anyhow::anyhow!("fixture lockfile has no registry checksum"))?;
  let checksum_end = valid_lock[checksum_start..]
    .find('"')
    .map(|index| checksum_start + index)
    .ok_or_else(|| anyhow::anyhow!("fixture lockfile has an unterminated checksum"))?;
  let mut baseline_lock = valid_lock.clone();
  baseline_lock.replace_range(checksum_start..checksum_end, &"0".repeat(checksum_end - checksum_start));
  std::fs::write(&lock_path, baseline_lock)?;
  ws.commit("record previous target dependency checksum")?;
  std::fs::write(&lock_path, valid_lock)?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "HEAD",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--target",
      "thumbv7em-none-eabihf",
    ],
  )?;
  assert!(
    output.status.success(),
    "target lock action plan failed:\nstdout: {}\nstderr: {}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(
    json["actions"][0]["selected_packages"],
    serde_json::json!(["target-consumer"])
  );
  assert_eq!(
    json["actions"][0]["resolution_views"][0]["target"],
    "thumbv7em-none-eabihf"
  );
  Ok(())
}

#[test]
fn test_runner_build_surface_ignore_bin_crates_filters_spawned_command() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-ignore-bin")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  std::fs::create_dir_all(ws.path.join("crates/bin-only/src"))?;
  std::fs::write(
    ws.path.join("crates/bin-only/Cargo.toml"),
    r#"[package]
name = "bin-only"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[[bin]]
name = "bin-only"
path = "src/main.rs"

[dependencies]
"#,
  )?;
  std::fs::write(ws.path.join("crates/bin-only/src/main.rs"), "fn main() {}\n")?;
  ws.commit("Add lib and bin-only crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--surface",
      "build",
      "--ignore-bin-crates",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check -p lib-a"),
    "ignore-bin-crates should keep non-bin crates in build command. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("bin-only"),
    "ignore-bin-crates should remove bin-only crates from build command. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "ignore-bin-crates should force package-scoped build. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_build_surface_global_change_uses_workspace_scope() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-workspace-scope")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "baseline"])?;
  std::fs::write(
    ws.path.join("rust-toolchain.toml"),
    "[toolchain]\nchannel = \"stable\"\n",
  )?;
  ws.commit("Add toolchain file")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--since",
      "baseline",
      "--surface",
      "build",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed. Output:\n{}", stdout);
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "global build scope should use workspace execution. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_uses_config_default_profile() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-default-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "ci"
"#,
  )?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--dry-run", "--print-cmd"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "config default ci profile should include build. Output:\n{}",
    stdout
  );
  assert!(stdout.contains("test:"), "ci profile should include test");

  Ok(())
}

#[test]
fn test_runner_profile_flag_overrides_config_default() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-profile-overrides-default")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "local"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "nightly",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "nightly profile should include build. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps"),
    "nightly profile should include docs. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_surface_flag_overrides_profile_selection() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-surface-overrides-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run]
default_profile = "ci"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "run", "--all", "--surface", "docs", "--dry-run", "--print-cmd"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps"),
    "explicit surface should execute docs. Output:\n{}",
    stdout
  );
  assert!(
    !stdout.contains("build: cargo check --workspace"),
    "explicit surface should bypass default ci profile. Output:\n{}",
    stdout
  );
  assert!(!stdout.contains("test:"), "explicit surface should not include test");

  Ok(())
}

#[test]
fn test_runner_workflow_maps_to_profile() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-workflow-profile")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.workflow]
commit = "ci"
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--workflow",
      "commit",
      "--dry-run",
      "--print-cmd",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "workflow->profile mapping should include build. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("test:"),
    "workflow->profile mapping should include test. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_profile_run_args_token_substitution() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-profile-token-substitution")?;
  ws.add_crate("profile-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.profile.docs_custom]
surfaces = ["docs"]
run_args = ["--manifest-path", "{workspace_root}/Cargo.toml", "{cargo_args}", "--quiet"]
"#,
  )?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "docs_custom",
      "--dry-run",
      "--print-cmd",
      "--",
      "--color",
      "never",
    ],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let manifest_path = ws.path.join("Cargo.toml");
  let stdout_normalized = stdout.replace('\\', "/");
  let manifest_path_normalized = manifest_path.display().to_string().replace('\\', "/");

  assert!(output.status.success(), "run should succeed");
  assert!(
    stdout.contains("docs: cargo doc --workspace --no-deps --manifest-path"),
    "docs command should include profile args. Output:\n{}",
    stdout
  );
  assert!(
    stdout_normalized.contains(&manifest_path_normalized),
    "workspace_root token should expand to absolute path. Output:\n{}",
    stdout
  );
  assert!(
    stdout.contains("--color never --quiet"),
    "cargo_args token should splice CLI args before trailing profile args. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_runner_workspace_root_applies_to_spawned_subprocesses() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-workspace-root-cwd")?;
  ws.add_crate("cwd-crate", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let outside_cwd = ws.path.parent().expect("temp workspace should have parent directory");
  let workspace_root = ws.path.display().to_string();
  let args = [
    "rail",
    "--workspace-root",
    workspace_root.as_str(),
    "run",
    "--all",
    "--surface",
    "build",
    "--print-cmd",
  ];
  let output = run_cargo_rail(outside_cwd, &args)?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(
    output.status.success(),
    "run should succeed from outside workspace when --workspace-root is set. stdout:\n{}\nstderr:\n{}",
    stdout,
    stderr
  );
  assert!(
    stdout.contains("build: cargo check --workspace"),
    "build command should execute via run surface. Output:\n{}",
    stdout
  );

  Ok(())
}

#[test]
fn test_repository_generated_actions_check_regenerate_order_and_redaction() -> Result<()> {
  let ws = TestWorkspace::new_named("test-repository-generated-actions")?;
  let helper = ws.add_crate("action-helper", "0.1.0", &[])?;
  std::fs::write(
    helper.join("src/main.rs"),
    r#"use std::path::Path;

fn main() {
  let mut args = std::env::args().skip(1);
  let mode = args.next().expect("mode");
  let action = args.next().expect("action");
  let root = std::env::var("WORKSPACE_ROOT").expect("WORKSPACE_ROOT");
  let policy = std::env::var("ACTION_POLICY").expect("ACTION_POLICY");
  assert!(std::env::var_os("TEST_ACTION_SECRET").is_some());
  let output = Path::new(&root).join("generated").join(format!("{action}.txt"));
  let cwd = std::env::current_dir().expect("current directory");
  let expected = format!("action={action}\npolicy={policy}\ncwd={}\n", cwd.display());
  if mode == "check" {
    let current = std::fs::read_to_string(&output).unwrap_or_default();
    if current != expected {
      eprintln!("generated output is stale: {}", output.display());
      std::process::exit(1);
    }
  } else {
    std::fs::create_dir_all(output.parent().expect("output parent")).expect("create output directory");
    std::fs::write(output, expected).expect("write generated output");
  }
}
"#,
  )?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.action.prepare]
kind = "generated"
argv = ["cargo", "run", "--quiet", "-p", "action-helper", "--", "regenerate", "prepare"]
check_argv = ["cargo", "run", "--quiet", "-p", "action-helper", "--", "check", "prepare"]
when = ["build"]
working_directory = "crates/action-helper"
inputs = ["Cargo.toml", "crates/action-helper/src"]
outputs = ["generated/prepare.txt"]

[run.action.prepare.environment]
inherit = true
entries = [
  { kind = "fixed", name = "ACTION_POLICY", value = "typed" },
  { kind = "cargo", name = "WORKSPACE_ROOT", value = "workspace-root" },
  { kind = "secret", name = "TEST_ACTION_SECRET" },
]

[run.action.finish]
kind = "generated"
argv = ["cargo", "run", "--quiet", "-p", "action-helper", "--", "regenerate", "finish"]
check_argv = ["cargo", "run", "--quiet", "-p", "action-helper", "--", "check", "finish"]
dependencies = ["prepare"]
when = ["build"]
working_directory = "crates/action-helper"
inputs = ["Cargo.toml", "crates/action-helper/src"]
outputs = ["generated/finish.txt"]

[run.action.finish.environment]
inherit = true
entries = [
  { kind = "fixed", name = "ACTION_POLICY", value = "typed" },
  { kind = "cargo", name = "WORKSPACE_ROOT", value = "workspace-root" },
  { kind = "secret", name = "TEST_ACTION_SECRET" },
]

[run.profile.pipeline]
actions = ["finish"]
"#,
  )?;
  ws.commit("Add generated action pipeline")?;

  const SECRET: &str = "must-not-enter-action-plans-or-receipts";
  let regenerate = run_cargo_rail_with_env(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "pipeline",
      "--generated",
      "regenerate",
      "--print-cmd",
    ],
    &[("TEST_ACTION_SECRET", SECRET)],
  )?;
  let stdout = String::from_utf8_lossy(&regenerate.stdout);
  assert!(regenerate.status.success(), "generator pipeline failed:\n{stdout}");
  let prepare_index = stdout.find("prepare: cargo run").expect("prepare preview");
  let finish_index = stdout.find("finish: cargo run").expect("finish preview");
  assert!(prepare_index < finish_index, "dependency must run before its owner");
  for action in ["prepare", "finish"] {
    let output = std::fs::read_to_string(ws.path.join(format!("generated/{action}.txt")))?;
    assert!(output.contains(&format!("action={action}\npolicy=typed\n")));
    assert!(output.replace('\\', "/").contains("/crates/action-helper\n"));
  }

  let explained = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "pipeline",
      "--dry-run",
      "--explain",
    ],
  )?;
  let explained_stdout = String::from_utf8_lossy(&explained.stdout);
  assert!(explained.status.success());
  assert!(explained_stdout.contains("action `prepare` owns: generated/prepare.txt"));
  assert!(explained_stdout.contains("action `finish` owns: generated/finish.txt"));

  let ci_plan = run_cargo_rail_with_env(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--profile",
      "pipeline",
      "--generated",
      "check",
      "--dry-run",
      "--format",
      "json",
    ],
    &[("TEST_ACTION_SECRET", SECRET)],
  )?;
  assert!(ci_plan.status.success());
  assert!(!String::from_utf8_lossy(&ci_plan.stdout).contains(SECRET));
  let ci_plan: serde_json::Value = serde_json::from_slice(&ci_plan.stdout)?;
  let ci_ids = ci_plan["actions"]
    .as_array()
    .unwrap()
    .iter()
    .map(|action| action["id"].as_str().unwrap())
    .collect::<Vec<_>>();
  assert_eq!(ci_ids, ["prepare", "finish"]);
  assert!(
    ci_plan["actions"][0]["argv"]
      .as_array()
      .unwrap()
      .iter()
      .any(|arg| arg == "check")
  );

  let check = run_cargo_rail_with_env(
    &ws.path,
    &["rail", "run", "--all", "--profile", "pipeline", "--generated", "check"],
    &[("TEST_ACTION_SECRET", SECRET)],
  )?;
  assert!(check.status.success(), "fresh generated outputs should pass check");

  std::fs::write(ws.path.join("generated/prepare.txt"), "stale\n")?;
  let stale = run_cargo_rail_with_env(
    &ws.path,
    &["rail", "run", "--all", "--profile", "pipeline", "--generated", "check"],
    &[("TEST_ACTION_SECRET", SECRET)],
  )?;
  assert_eq!(stale.status.code(), Some(1), "stale generated output must exit one");

  let receipts = std::fs::read_dir(ws.path.join("target/cargo-rail/receipts"))?
    .filter_map(|entry| entry.ok())
    .filter(|entry| entry.file_name().to_string_lossy().starts_with("run-decision-"))
    .map(|entry| std::fs::read_to_string(entry.path()))
    .collect::<std::io::Result<Vec<_>>>()?;
  assert!(receipts.iter().any(|receipt| receipt.contains("TEST_ACTION_SECRET")));
  assert!(receipts.iter().all(|receipt| !receipt.contains(SECRET)));

  Ok(())
}

#[test]
fn test_run_ci_plan_matches_local_graph_order_and_is_byte_deterministic() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-ci-action-plan")?;
  ws.add_crate("plan-crate", "0.1.0", &[])?;
  ws.commit("Add plan crate")?;
  let base = [
    "rail",
    "run",
    "--all",
    "--action",
    "lint",
    "--action",
    "build",
    "--dry-run",
  ];

  let local = run_cargo_rail(&ws.path, &base)?;
  let local_stdout = String::from_utf8_lossy(&local.stdout);
  assert!(local.status.success());
  assert!(local_stdout.find("lint: cargo clippy").unwrap() < local_stdout.find("build: cargo check").unwrap());

  let mut json_args = base.to_vec();
  json_args.extend(["--format", "json"]);
  let first_json = run_cargo_rail(&ws.path, &json_args)?;
  let second_json = run_cargo_rail(&ws.path, &json_args)?;
  assert!(first_json.status.success());
  assert_eq!(
    first_json.stdout, second_json.stdout,
    "identical action plans must be byte stable"
  );
  let json: serde_json::Value = serde_json::from_slice(&first_json.stdout)?;
  assert_eq!(json["version"], 4);
  assert_eq!(json["execution_profile"], "normal");
  assert!(json["fetch_action"].is_null());
  let ids = json["actions"]
    .as_array()
    .unwrap()
    .iter()
    .map(|action| action["id"].as_str().unwrap())
    .collect::<Vec<_>>();
  assert_eq!(ids, ["lint", "build"]);
  assert_eq!(json["actions"][0]["selected_features"]["all_features"], true);
  assert_eq!(json["actions"][1]["selected_features"]["default_features"], true);
  for action in json["actions"].as_array().expect("actions should be an array") {
    let binding = action["resolution_views"]
      .as_array()
      .and_then(|bindings| bindings.first())
      .expect("Cargo actions must bind an exact resolution view");
    assert!(binding["root_package_ids"].as_array().is_some_and(|packages| {
      packages
        .iter()
        .any(|package| package.as_str().is_some_and(|id| id.contains("plan-crate")))
    }));
    assert!(binding["target"].as_str().is_some());
    assert!(binding["resolved_node_count"].as_u64().is_some_and(|count| count > 0));
  }

  let mut github_args = base.to_vec();
  github_args.extend(["--format", "github"]);
  let github = run_cargo_rail(&ws.path, &github_args)?;
  assert!(
    github.status.success(),
    "GitHub action plan failed: {}",
    String::from_utf8_lossy(&github.stderr)
  );
  let github_stdout = String::from_utf8_lossy(&github.stdout);
  let github_ids = github_stdout
    .lines()
    .find_map(|line| line.strip_prefix("action_ids_json="))
    .expect("GitHub action IDs");
  assert_eq!(serde_json::from_str::<Vec<String>>(github_ids)?, ids);

  let executing_json = run_cargo_rail(
    &ws.path,
    &["rail", "run", "--all", "--action", "build", "--format", "json"],
  )?;
  assert!(!executing_json.status.success());
  assert!(
    String::from_utf8_lossy(&executing_json.stderr).contains("dry-run")
      || String::from_utf8_lossy(&executing_json.stdout).contains("dry-run")
  );

  Ok(())
}

#[test]
fn test_action_key_analysis_selects_exact_dependency_closure_inputs() -> Result<()> {
  fn populate_fixture(workspace: &TestWorkspace) -> Result<()> {
    workspace.add_crate("lib-dep", "0.1.0", &[])?;
    workspace.add_crate("lib-a", "0.1.0", &[("lib-dep", "{ path = \"../lib-dep\" }")])?;
    let ignored_bin = workspace.add_crate("ignored-bin", "0.1.0", &[])?;
    std::fs::remove_file(ignored_bin.join("src/lib.rs"))?;
    std::fs::write(ignored_bin.join("src/main.rs"), "fn main() {}\n")?;
    std::fs::write(workspace.path.join("README.md"), "workspace documentation\n")?;
    workspace.commit("Add action-key fixture")?;
    workspace.modify_file(
      "lib-a",
      "src/lib.rs",
      "pub fn selected_action_input() { lib_dep::hello(); }\n",
    )?;
    Ok(())
  }

  let ws = TestWorkspace::new_named("action-key-selected-inputs")?;
  populate_fixture(&ws)?;

  let command = [
    "rail",
    "run",
    "--since",
    "HEAD",
    "--action",
    "build",
    "--ignore-bin-crates",
    "--dry-run",
    "--format",
    "json",
  ];
  let baseline = run_cargo_rail(&ws.path, &command)?;
  assert!(
    baseline.status.success(),
    "baseline action-key analysis failed (status {}): stdout={} stderr={}",
    baseline.status,
    String::from_utf8_lossy(&baseline.stdout),
    String::from_utf8_lossy(&baseline.stderr),
  );
  let baseline: serde_json::Value = serde_json::from_slice(&baseline.stdout)?;
  let action = &baseline["actions"][0];
  assert_eq!(action["selected_packages"], serde_json::json!(["lib-a"]));
  let baseline_input_root = action["action_key"]["declared_inputs"]["root_digest"]
    .as_str()
    .expect("declared input root");
  let baseline_resolution = action["resolution_views"][0]["resolution_digest"]
    .as_str()
    .expect("resolution digest");
  assert_eq!(action["action_key"]["status"], "uncacheable");
  assert!(
    action["action_key"].get("key").is_none(),
    "incomplete evidence must not issue a key"
  );
  let reason_codes = action["action_key"]["reasons"]
    .as_array()
    .expect("action-key reasons")
    .iter()
    .filter_map(|reason| reason["code"].as_str())
    .collect::<Vec<_>>();
  assert!(reason_codes.contains(&"ambient_environment"));
  assert!(reason_codes.contains(&"cargo_units_unmodeled"));

  std::fs::write(ws.path.join("README.md"), "unrelated documentation changed\n")?;
  let unrelated = run_cargo_rail_with_env(&ws.path, &command, &[("CARGO_RAIL_UNRELATED", "changed")])?;
  assert!(unrelated.status.success(), "unrelated-input analysis failed");
  let unrelated: serde_json::Value = serde_json::from_slice(&unrelated.stdout)?;
  assert_eq!(
    unrelated["actions"][0]["action_key"]["declared_inputs"]["root_digest"], baseline_input_root,
    "a root-level file outside selected package roots must not invalidate the action input tree"
  );
  assert_eq!(
    unrelated["actions"][0]["resolution_views"][0]["resolution_digest"], baseline_resolution,
    "an unrelated environment variable and source file must not change resolution identity"
  );

  ws.modify_file("ignored-bin", "src/main.rs", "fn main() { println!(\"unrelated\"); }\n")?;
  let unrelated_package = run_cargo_rail(&ws.path, &command)?;
  assert!(unrelated_package.status.success(), "unrelated-package analysis failed");
  let unrelated_package: serde_json::Value = serde_json::from_slice(&unrelated_package.stdout)?;
  assert_eq!(
    unrelated_package["actions"][0]["action_key"]["declared_inputs"]["root_digest"], baseline_input_root,
    "a package excluded from action selection must not invalidate the declared input tree"
  );
  assert_eq!(
    unrelated_package["actions"][0]["resolution_views"][0]["resolution_digest"], baseline_resolution,
    "a package excluded from action selection must not invalidate resolution identity"
  );

  ws.modify_file("lib-dep", "src/lib.rs", "pub fn changed_dependency_input() {}\n")?;
  let changed = run_cargo_rail(&ws.path, &command)?;
  assert!(changed.status.success(), "changed-input analysis failed");
  let changed: serde_json::Value = serde_json::from_slice(&changed.stdout)?;
  assert_ne!(
    changed["actions"][0]["action_key"]["declared_inputs"]["root_digest"], baseline_input_root,
    "exact selected source bytes must invalidate the declared input root"
  );

  let mirror = TestWorkspace::new_named("action-key-selected-inputs-mirror")?;
  populate_fixture(&mirror)?;
  let mirrored = run_cargo_rail(&mirror.path, &command)?;
  assert!(mirrored.status.success(), "mirrored action-key analysis failed");
  let mirrored: serde_json::Value = serde_json::from_slice(&mirrored.stdout)?;
  assert_eq!(
    mirrored["actions"][0]["action_key"]["declared_inputs"]["root_digest"], baseline_input_root,
    "declared source identity must not depend on the physical workspace root"
  );
  assert_eq!(
    mirrored["actions"][0]["resolution_views"][0]["resolution_digest"], baseline_resolution,
    "resolved graph identity must not depend on the physical workspace root"
  );

  Ok(())
}

#[test]
fn test_action_key_cargo_cli_config_bypass_respects_argument_domains() -> Result<()> {
  let ws = TestWorkspace::new_named("action-key-cargo-cli-config")?;
  ws.add_crate("config-probe", "0.1.0", &[])?;
  ws.commit("Add Cargo CLI config fixture")?;

  let build = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--config",
      "build.rustflags=[]",
    ],
  )?;
  assert!(
    build.status.success(),
    "build plan failed: {}",
    String::from_utf8_lossy(&build.stderr)
  );
  let build: serde_json::Value = serde_json::from_slice(&build.stdout)?;
  let build_reasons = build["actions"][0]["action_key"]["reasons"]
    .as_array()
    .expect("build action-key reasons");
  assert!(
    build_reasons
      .iter()
      .any(|reason| reason["code"] == "cargo_cli_configuration_unmodeled"),
    "Cargo --config must remain an explicit bypass: {build}"
  );

  let test = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "test",
      "--skip-nextest",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--config=ordinary-harness-value",
    ],
  )?;
  assert!(
    test.status.success(),
    "test plan failed: {}",
    String::from_utf8_lossy(&test.stderr)
  );
  let test: serde_json::Value = serde_json::from_slice(&test.stdout)?;
  assert!(
    test["actions"][0]["argv"].as_array().is_some_and(|argv| argv
      .iter()
      .any(|argument| argument == "--config=ordinary-harness-value")),
    "fixture must pass --config after Cargo's harness separator: {test}"
  );
  assert!(
    test["actions"][0]["action_key"]["reasons"]
      .as_array()
      .is_some_and(|reasons| reasons
        .iter()
        .all(|reason| reason["code"] != "cargo_cli_configuration_unmodeled")),
    "a test-harness argument must not be interpreted as Cargo configuration: {test}"
  );

  Ok(())
}

#[test]
fn test_native_cache_honors_explicit_opt_out_and_cargo_cli_configuration() -> Result<()> {
  let ws = TestWorkspace::new_named("native-cache-explicit-bypasses")?;
  ws.add_crate("cache-bypass", "0.1.0", &[])?;
  ws.commit("Add native cache bypass fixture")?;

  let disabled = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--no-cache",
      "--explain",
      "--",
      "--quiet",
    ],
  )?;
  assert!(
    disabled.status.success(),
    "explicitly disabled native-cache run failed: {}",
    String::from_utf8_lossy(&disabled.stderr)
  );
  assert!(
    String::from_utf8_lossy(&disabled.stdout)
      .contains("native compiler cache: bypassed (native_cache_disabled_by_request)"),
    "the explicit opt-out must win: {}",
    String::from_utf8_lossy(&disabled.stdout)
  );

  fs::remove_dir_all(ws.path.join("target"))?;
  let configured = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--quiet",
      "--config=build.jobs=1",
    ],
  )?;
  assert!(
    configured.status.success(),
    "Cargo CLI configuration run failed: {}",
    String::from_utf8_lossy(&configured.stderr)
  );
  assert!(
    String::from_utf8_lossy(&configured.stdout)
      .contains("native compiler cache: bypassed (cargo_cli_configuration_not_graduated)"),
    "Cargo CLI configuration must remain authoritative: {}",
    String::from_utf8_lossy(&configured.stdout)
  );
  Ok(())
}

#[test]
fn test_native_cache_leaves_default_incremental_development_cold() -> Result<()> {
  let ws = TestWorkspace::new_named("native-cache-incremental-bypass")?;
  ws.add_crate("incremental-bypass", "0.1.0", &[])?;
  ws.commit("Add incremental native cache bypass fixture")?;

  let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--quiet",
    ])
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    output.status.success(),
    "default incremental build failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    String::from_utf8_lossy(&output.stdout)
      .contains("native compiler cache: bypassed (native_cache_incremental_policy_not_graduated)"),
    "default Cargo development must bypass native-cache setup: {}",
    String::from_utf8_lossy(&output.stdout)
  );

  fs::remove_dir_all(ws.path.join("target"))?;
  let forced = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--quiet",
    ])
    .env("CARGO_INCREMENTAL", "0")
    .env("RUSTC_FORCE_INCREMENTAL", "1")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    forced.status.success(),
    "forced incremental build failed: {}",
    String::from_utf8_lossy(&forced.stderr)
  );
  assert!(
    String::from_utf8_lossy(&forced.stdout)
      .contains("native compiler cache: bypassed (forced_incremental_compilation_not_graduated)"),
    "rustc's forced incremental mode must bypass native-cache setup: {}",
    String::from_utf8_lossy(&forced.stdout)
  );
  Ok(())
}

#[test]
fn test_native_cache_bypasses_binary_only_workload_before_toolchain_hashing() -> Result<()> {
  if !matches!(
    (std::env::consts::OS, std::env::consts::ARCH),
    ("macos", "aarch64") | ("linux", "aarch64")
  ) {
    return Ok(());
  }
  let rustc = std::process::Command::new("rustc").arg("-vV").output()?;
  let cargo = std::process::Command::new("cargo").arg("-Vv").output()?;
  if !String::from_utf8_lossy(&rustc.stdout).starts_with("rustc 1.97.1 ")
    || !String::from_utf8_lossy(&cargo.stdout).starts_with("cargo 1.97.1 ")
  {
    return Ok(());
  }

  let ws = TestWorkspace::new_single_crate("native-cache-binary-only", "0.1.0")?;
  fs::remove_file(ws.path.join("src/lib.rs"))?;
  fs::write(ws.path.join("src/main.rs"), "fn main() {}\n")?;
  git(&ws.path, &["add", "src"])?;
  git(&ws.path, &["commit", "-m", "Use a binary-only target"])?;

  let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--quiet",
    ])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env("CARGO_INCREMENTAL", "0")
    .output()?;
  assert!(
    output.status.success(),
    "binary-only build failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    String::from_utf8_lossy(&output.stdout)
      .contains("native compiler cache: bypassed (native_cache_no_eligible_library_units)"),
    "an unsupported workload must bypass before native-cache setup: {}",
    String::from_utf8_lossy(&output.stdout)
  );

  fs::create_dir(ws.path.join(".cargo"))?;
  fs::write(
    ws.path.join(".cargo/config.toml"),
    "[build]\nunmodeled-native-cache-test = true\n",
  )?;
  git(&ws.path, &["add", ".cargo/config.toml"])?;
  git(&ws.path, &["commit", "-m", "Add unmodeled Cargo build configuration"])?;
  fs::remove_dir_all(ws.path.join("target"))?;
  let unmodeled = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&ws.path)
    .args([
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--quiet",
    ])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env("CARGO_INCREMENTAL", "0")
    .output()?;
  assert!(
    unmodeled.status.success(),
    "unmodeled Cargo configuration build failed: {}",
    String::from_utf8_lossy(&unmodeled.stderr)
  );
  assert!(
    String::from_utf8_lossy(&unmodeled.stdout)
      .contains("native compiler cache: bypassed (cargo_configuration_unmodeled)"),
    "unmodeled Cargo build settings must bypass native-cache setup: {}",
    String::from_utf8_lossy(&unmodeled.stdout)
  );
  Ok(())
}

#[test]
fn test_doctor_hermeticity_reports_fail_closed_action_key_reasons_without_receipt() -> Result<()> {
  let ws = TestWorkspace::new_named("doctor-hermeticity")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add hermeticity fixture")?;

  let output = run_cargo_rail(
    &ws.path,
    &["rail", "doctor", "hermeticity", "--action", "build", "--format", "json"],
  )?;
  assert!(
    output.status.success(),
    "hermeticity doctor failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(report["artifact"], "hermeticity_report");
  assert_eq!(report["version"], 1);
  assert_eq!(report["actions"][0]["action_key"]["version"], 2);
  assert_eq!(report["actions"][0]["action_key"]["status"], "uncacheable");
  assert!(report["actions"][0]["action_key"].get("key").is_none());
  assert!(
    report["actions"][0]["action_key"]["reasons"]
      .as_array()
      .is_some_and(|reasons| reasons.iter().any(|reason| reason["code"] == "ambient_environment"))
  );
  assert!(
    report["actions"][0]["action_key"]["reasons"]
      .as_array()
      .is_some_and(|reasons| reasons
        .iter()
        .any(|reason| reason["code"] == "executable_runtime_inputs_unavailable")),
    "content-addressed executables must still report unobserved dynamic runtime inputs"
  );
  assert!(
    !ws.path.join("target/cargo-rail/receipts").exists(),
    "read-only hermeticity diagnosis must not write a run receipt"
  );
  Ok(())
}

#[test]
fn test_doctor_native_cache_reports_the_exact_capability_as_one_json_value() -> Result<()> {
  let ws = TestWorkspace::new_named("doctor-native-cache")?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add native-cache doctor fixture")?;

  let output = run_cargo_rail(&ws.path, &["rail", "doctor", "native-cache", "--format", "json"])?;
  assert!(
    output.status.success(),
    "native-cache doctor failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(output.stderr.is_empty(), "JSON diagnostics must not contaminate stderr");
  let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(report["command"], "doctor");
  assert_eq!(report["mode"], "native_cache");
  assert_eq!(report["result"], "success");
  assert_eq!(report["exit_code"], 0);

  let capability = &report["capability"];
  assert_eq!(capability["schema_version"], 1);
  assert_eq!(capability["cache_class"], "library_metadata_rlib");
  assert_eq!(capability["execution_contract"], "direct-global-wrapper-v2");
  assert!(capability["platform"].as_str().is_some_and(|value| !value.is_empty()));
  assert!(
    capability["host_target"]
      .as_str()
      .is_some_and(|value| !value.is_empty())
  );
  assert!(
    capability["identity"]
      .as_str()
      .is_some_and(|value| value.len() == 71 && value.starts_with("sha256:"))
  );
  assert!(capability["certified"].is_boolean());
  assert!(
    !ws.path.join("target/cargo-rail/receipts").exists(),
    "read-only native-cache diagnosis must not write a run receipt"
  );
  Ok(())
}

#[test]
fn test_hermetic_build_proves_identical_check_result_in_two_roots() -> Result<()> {
  let local_cache = tempfile::tempdir()?;
  let local_cache = local_cache.path().to_string_lossy().into_owned();
  let first = TestWorkspace::new_named("hermetic-check-first")?;
  first.add_crate("lib-a", "0.1.0", &[])?;
  fs::create_dir_all(first.path.join(".cargo"))?;
  fs::write(
    first.path.join(".cargo/config.toml"),
    "[env]\nCARGO_RAIL_HERMETIC_VALUE = { value = \"stable\", force = true }\n",
  )?;
  fs::write(
    first.path.join("crates/lib-a/src/lib.rs"),
    "pub const VALUE: &str = env!(\"CARGO_RAIL_HERMETIC_VALUE\");\npub fn hello() -> &'static str { \"Hello from lib-a\" }\n",
  )?;
  generate_lockfile(&first.path)?;
  first.commit("Add hermetic check fixture")?;
  let second = TestWorkspace::new_named("hermetic-check-second")?;
  second.add_crate("lib-a", "0.1.0", &[])?;
  fs::write(second.path.join(".gitignore"), "target/\nCargo.lock\n")?;
  fs::create_dir_all(second.path.join(".cargo"))?;
  fs::write(
    second.path.join(".cargo/config.toml"),
    "[env]\nCARGO_RAIL_HERMETIC_VALUE = { value = \"stable\", force = true }\n",
  )?;
  fs::write(
    second.path.join("crates/lib-a/src/lib.rs"),
    "pub const VALUE: &str = env!(\"CARGO_RAIL_HERMETIC_VALUE\");\npub fn hello() -> &'static str { \"Hello from lib-a\" }\n",
  )?;
  generate_lockfile(&second.path)?;
  second.commit("Add hermetic check fixture")?;
  let changed = TestWorkspace::new_named("hermetic-check-changed")?;
  changed.add_crate("lib-a", "0.1.0", &[])?;
  fs::create_dir_all(changed.path.join(".cargo"))?;
  fs::write(
    changed.path.join(".cargo/config.toml"),
    "[env]\nCARGO_RAIL_HERMETIC_VALUE = { value = \"stable\", force = true }\n",
  )?;
  let changed_source = changed.path.join("crates/lib-a/src/lib.rs");
  fs::write(
    changed_source,
    "pub const VALUE: &str = env!(\"CARGO_RAIL_HERMETIC_VALUE\");\npub fn hello() -> &'static str { \"Hallo from lib-a\" }\n",
  )?;
  generate_lockfile(&changed.path)?;
  changed.commit("Change hermetic check input")?;

  let plan = run_cargo_rail(
    &first.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--hermetic",
      "--dry-run",
      "--format",
      "json",
    ],
  )?;
  assert!(plan.status.success(), "hermetic action plan failed");
  let plan: serde_json::Value = serde_json::from_slice(&plan.stdout)?;
  assert_eq!(plan["execution_profile"], "hermetic");
  assert_eq!(plan["fetch_action"]["id"], "fetch");
  assert_eq!(plan["fetch_action"]["network"], "allowed");
  assert_eq!(plan["fetch_action"]["consumer_network"], "denied");

  let command = ["rail", "run", "--all", "--action", "build", "--hermetic"];
  let first_output = run_cargo_rail_with_env(&first.path, &command, &[("CARGO_RAIL_CACHE_DIR", &local_cache)])?;
  assert!(
    first_output.status.success(),
    "first hermetic check failed ({})\nstdout:\n{}\nstderr:\n{}",
    first_output.status,
    String::from_utf8_lossy(&first_output.stdout),
    String::from_utf8_lossy(&first_output.stderr)
  );
  fs::write(second.path.join("UNRELATED.md"), "not selected by the build action\n")?;
  second.commit("Add unrelated documentation")?;
  let diagnostics = tempfile::tempdir()?;
  let hit_diagnostics = diagnostics.path().join("hit.json");
  let hit_diagnostics_text = hit_diagnostics.to_string_lossy().into_owned();
  let second_output = run_cargo_rail_with_env(
    &second.path,
    &[
      "rail",
      "--diagnostics-file",
      hit_diagnostics_text.as_str(),
      "run",
      "--all",
      "--action",
      "build",
      "--hermetic",
    ],
    &[
      ("CARGO_RAIL_CACHE_DIR", &local_cache),
      ("CARGO_RAIL_UNRELATED", "changed"),
    ],
  )?;
  assert!(
    second_output.status.success(),
    "second hermetic check failed ({})\nstdout:\n{}\nstderr:\n{}",
    second_output.status,
    String::from_utf8_lossy(&second_output.stdout),
    String::from_utf8_lossy(&second_output.stderr)
  );
  let changed_output = run_cargo_rail_with_env(&changed.path, &command, &[("CARGO_RAIL_CACHE_DIR", &local_cache)])?;
  assert!(
    changed_output.status.success(),
    "changed hermetic check failed ({})\nstdout:\n{}\nstderr:\n{}",
    changed_output.status,
    String::from_utf8_lossy(&changed_output.stdout),
    String::from_utf8_lossy(&changed_output.stderr)
  );

  let report = |workspace: &Path| -> Result<serde_json::Value> {
    let directory = workspace.join("target/cargo-rail/hermetic/reports");
    let paths = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(paths.len(), 1, "one hermetic report should be published");
    Ok(serde_json::from_slice(&fs::read(paths[0].path())?)?)
  };
  let first_report = report(&first.path)?;
  let second_report = report(&second.path)?;
  let changed_report = report(&changed.path)?;
  let inventories =
    fs::read_dir(first.path.join("target/cargo-rail/hermetic/inventories"))?.collect::<std::io::Result<Vec<_>>>()?;
  assert_eq!(inventories.len(), 1, "one fetch inventory should be published");
  let mut inventory_entries = fs::read_dir(inventories[0].path())?
    .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
    .collect::<std::io::Result<Vec<_>>>()?;
  inventory_entries.sort();
  assert_eq!(
    inventory_entries,
    ["cargo-home", "manifest.json"],
    "fetch staging homes and Rust toolchains must never enter the immutable dependency inventory"
  );
  assert_eq!(first_report["version"], 3);
  assert_eq!(first_report["profile_version"], 1);
  assert_eq!(first_report["action_class"], "cargo_check");
  assert_eq!(first_report["fetch"]["version"], 1);
  assert_eq!(first_report["fetch"]["reused"], false);
  assert_eq!(first_report["fetch"]["packages"], 0);
  assert!(
    first_report["output_manifest"]["files"]
      .as_u64()
      .is_some_and(|files| files > 0),
    "rustc outputs must be declared: {first_report}"
  );
  assert_eq!(
    first_report["output_manifest"]["digest"], second_report["output_manifest"]["digest"],
    "equivalent source in different roots must have one output manifest:\nfirst={first_report:#}\nsecond={second_report:#}"
  );
  assert_ne!(
    first_report["output_manifest"]["digest"], changed_report["output_manifest"]["digest"],
    "a semantic source mutation must change the compiler output manifest"
  );
  let encoded = serde_json::to_string(&first_report)?;
  assert!(!encoded.contains(&first.path.to_string_lossy().into_owned()));
  assert!(!first.path.join("target/debug").exists());
  assert!(!second.path.join("target/debug").exists());

  #[cfg(target_os = "macos")]
  {
    assert_eq!(
      second_report["fetch"]["reused"],
      true,
      "equivalent checkout did not hit:\nreport={second_report:#}\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&second_output.stdout),
      String::from_utf8_lossy(&second_output.stderr)
    );
    assert!(
      first_report["action_key"].is_string(),
      "an eligible action must have an action key"
    );
    assert!(
      first_report["result_digest"].is_string(),
      "an eligible action must have a result digest"
    );
    assert_eq!(
      first_report["action_key"], second_report["action_key"],
      "equivalent source in different roots must have one action key:\nfirst={first_report:#}\nsecond={second_report:#}"
    );
    assert_eq!(
      first_report["result_digest"], second_report["result_digest"],
      "equivalent source in different roots must have one result digest"
    );
    assert_ne!(
      first_report["action_key"], changed_report["action_key"],
      "a same-size source mutation must change the hermetic action key"
    );
    assert_eq!(first_report["support"], "eligible");
    assert_eq!(first_report["enforcement"], "filesystem_and_network");
    assert!(first_report["reasons"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(first_report["cache"]["status"], "miss");
    assert_eq!(first_report["cache"]["reason"], "validated_candidate_not_found");
    assert_eq!(first_report["cache"]["stored"], true);
    assert_eq!(first_report["cache"]["cargo_check_executed"], true);
    assert_eq!(first_report["cache"]["compiler_units_executed"], true);
    assert_eq!(second_report["cache"]["status"], "hit");
    assert_eq!(second_report["cache"]["stored"], false);
    assert_eq!(second_report["cache"]["cargo_check_executed"], false);
    assert_eq!(second_report["cache"]["compiler_units_executed"], false);
    assert_eq!(
      second_report["cache"]["bytes_restored"], second_report["output_manifest"]["bytes"],
      "a hit must restore every declared regular-file byte"
    );
    assert!(
      !String::from_utf8_lossy(&second_output.stderr).contains("Checking lib-a"),
      "a cache hit must not execute the Cargo check action:\n{}",
      String::from_utf8_lossy(&second_output.stderr)
    );
    let materialized_root = second_report["materialized_root"]
      .as_str()
      .expect("a cache hit must publish its declared output root");
    assert!(!Path::new(materialized_root).is_absolute());
    let materialized_root = second.path.join(materialized_root);
    assert!(
      materialized_root.is_dir(),
      "restored outputs must persist after cargo-rail exits"
    );
    assert_materialized_output_matches_manifest(&materialized_root, &second_report["output_manifest"])?;
    let receipts =
      fs::read_dir(second.path.join("target/cargo-rail/receipts"))?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(receipts.len(), 1, "one cache-hit decision receipt");
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(receipts[0].path())?)?;
    assert_eq!(receipt["execution"]["execution_mode"], "verified_local_cache_restore");
    assert_eq!(receipt["execution"]["cargo_check_executed"], false);
    assert_eq!(receipt["execution"]["compiler_units_executed"], false);
    assert!(
      receipt["execution"]["fetch_action"].is_null(),
      "a process-free hit receipt must not claim that Cargo fetch ran"
    );

    let counters: serde_json::Value = serde_json::from_slice(&fs::read(&hit_diagnostics)?)?;
    assert_eq!(counters["schema_version"], 6);
    assert_eq!(
      counters["cargo_metadata_loads"], 0,
      "a cache hit must not execute Cargo metadata"
    );
    assert_eq!(
      counters["hermetic_fetch_executions"], 0,
      "a cache hit must not execute Cargo fetch"
    );
    assert_eq!(
      counters["hermetic_cargo_probes"], 0,
      "a cache hit must not execute Cargo probes"
    );
    assert_eq!(
      counters["hermetic_rustc_probes"], 0,
      "a cache hit must not execute rustc probes"
    );
    assert_eq!(
      counters["hermetic_rustdoc_probes"], 0,
      "a cache hit must not execute rustdoc probes"
    );
    assert_eq!(
      counters["hermetic_cargo_executions"], 0,
      "a cache hit must not execute Cargo check"
    );
    assert_eq!(
      counters["hermetic_compiler_units"], 0,
      "a cache hit must not execute compiler units"
    );
    assert_eq!(changed_report["cache"]["status"], "miss");
    assert_eq!(changed_report["cache"]["cargo_check_executed"], true);

    let override_directory = tempfile::tempdir()?;
    let override_path = override_directory.path().join("rail.toml");
    fs::write(&override_path, "[run]\n")?;
    let override_path = override_path.to_string_lossy().into_owned();
    let config_override = run_cargo_rail_with_env(
      &second.path,
      &[
        "rail",
        "--config",
        override_path.as_str(),
        "run",
        "--all",
        "--action",
        "build",
        "--hermetic",
        "--explain",
      ],
      &[("CARGO_RAIL_CACHE_DIR", &local_cache)],
    )?;
    assert!(
      config_override.status.success()
        && String::from_utf8_lossy(&config_override.stdout)
          .contains("local cache: uncacheable (pre_context_request_not_graduated)")
        && String::from_utf8_lossy(&config_override.stdout).contains("cargo_check_executed=true")
        && String::from_utf8_lossy(&config_override.stderr).contains("Checking lib-a"),
      "an explicit configuration override must not enter the process-free path:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&config_override.stdout),
      String::from_utf8_lossy(&config_override.stderr)
    );

    let historical_candidates = run_cargo_rail_with_env(
      &second.path,
      &["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"],
      &[("CARGO_RAIL_CACHE_DIR", &local_cache)],
    )?;
    assert!(
      historical_candidates.status.success()
        && String::from_utf8_lossy(&historical_candidates.stdout).contains("local cache: hit")
        && !String::from_utf8_lossy(&historical_candidates.stderr).contains("Checking lib-a"),
      "the exact candidate must hit after a same-seed source mutation is cached:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&historical_candidates.stdout),
      String::from_utf8_lossy(&historical_candidates.stderr)
    );

    let relevant_environment = run_cargo_rail_with_env(
      &second.path,
      &["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"],
      &[("CARGO_RAIL_CACHE_DIR", &local_cache), ("CARGO_PROFILE_DEV_DEBUG", "0")],
    )?;
    assert!(
      relevant_environment.status.success()
        && String::from_utf8_lossy(&relevant_environment.stdout)
          .contains("local cache precheck: miss (cargo_configuration_changed)")
        && String::from_utf8_lossy(&relevant_environment.stdout).contains("cargo_check_executed=true")
        && String::from_utf8_lossy(&relevant_environment.stderr).contains("Checking lib-a"),
      "a relevant Cargo environment mutation must execute cold:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&relevant_environment.stdout),
      String::from_utf8_lossy(&relevant_environment.stderr)
    );

    let config_path = second.path.join(".cargo/config.toml");
    let original_config = fs::read_to_string(&config_path)?;
    fs::write(
      &config_path,
      "[env]\nCARGO_RAIL_HERMETIC_VALUE = { value = \"mutant\", force = true }\n",
    )?;
    let changed_config = run_cargo_rail_with_env(
      &second.path,
      &["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"],
      &[("CARGO_RAIL_CACHE_DIR", &local_cache)],
    )?;
    assert!(
      changed_config.status.success()
        && !String::from_utf8_lossy(&changed_config.stdout).contains("local cache: hit")
        && String::from_utf8_lossy(&changed_config.stdout).contains("cargo_check_executed=true")
        && String::from_utf8_lossy(&changed_config.stderr).contains("Checking lib-a"),
      "a same-size Cargo configuration mutation must execute cold:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&changed_config.stdout),
      String::from_utf8_lossy(&changed_config.stderr)
    );
    fs::write(&config_path, original_config)?;

    let lock_path = second.path.join("Cargo.lock");
    let original_lock = fs::read_to_string(&lock_path)?;
    fs::write(&lock_path, format!("{original_lock}\n"))?;
    let changed_lock = run_cargo_rail_with_env(
      &second.path,
      &["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"],
      &[("CARGO_RAIL_CACHE_DIR", &local_cache)],
    )?;
    assert!(
      changed_lock.status.success()
        && !String::from_utf8_lossy(&changed_lock.stdout).contains("local cache: hit")
        && String::from_utf8_lossy(&changed_lock.stdout).contains("cargo_check_executed=true")
        && String::from_utf8_lossy(&changed_lock.stderr).contains("Checking lib-a"),
      "an exact lockfile mutation must execute cold:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&changed_lock.stdout),
      String::from_utf8_lossy(&changed_lock.stderr)
    );
    fs::write(&lock_path, original_lock)?;
  }
  #[cfg(not(target_os = "macos"))]
  {
    for report in [&first_report, &second_report, &changed_report] {
      assert!(
        report.get("action_key").is_none(),
        "a platform-limited action must not publish an action key: {report}"
      );
      assert!(
        report.get("result_digest").is_none(),
        "a platform-limited action must not publish a reusable result identity: {report}"
      );
      assert_eq!(report["support"], "platform_limited");
      assert_eq!(report["enforcement"], "cargo_offline_only");
      assert_eq!(
        report["reasons"],
        serde_json::json!(["os_network_and_filesystem_enforcement_unavailable"])
      );
      assert_eq!(report["cache"]["status"], "uncacheable");
      assert_eq!(
        report["cache"]["reason"],
        "os_network_and_filesystem_enforcement_unavailable"
      );
      assert_eq!(report["cache"]["stored"], false);
      assert_eq!(report["cache"]["cargo_check_executed"], true);
      assert_eq!(report["cache"]["compiler_units_executed"], true);
      assert_eq!(report["fetch"]["reused"], false);
    }
  }
  Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn test_local_action_cache_corruption_disable_and_cleanup_fail_closed() -> Result<()> {
  let ws = TestWorkspace::new_named("local-action-cache-fail-closed")?;
  ws.add_crate("cached-lib", "0.1.0", &[])?;
  generate_lockfile(&ws.path)?;
  ws.commit("Add local cache fixture")?;
  let cache = tempfile::tempdir()?;
  let cache_path = cache.path().to_string_lossy().into_owned();
  fs::write(cache.path().join("keep"), "outside owned CAS root\n")?;
  let environment = [("CARGO_RAIL_CACHE_DIR", cache_path.as_str())];
  let command = ["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"];

  let cold = run_cargo_rail_with_env(&ws.path, &command, &environment)?;
  assert!(
    cold.status.success(),
    "cold cache population failed:\n{}",
    String::from_utf8_lossy(&cold.stderr)
  );
  let cas_root = cache.path().join("cargo-rail/local-cas-v1");
  let preview = run_cargo_rail_with_env(&ws.path, &["rail", "clean", "--cache", "--check"], &environment)?;
  assert_eq!(preview.status.code(), Some(1), "cache preview must report work");
  assert!(
    String::from_utf8_lossy(&preview.stdout).contains(&cas_root.display().to_string()),
    "clean --cache --check must explain the owned shared root"
  );
  assert!(cas_root.is_dir(), "check mode must not remove the local CAS");
  let pin_path = fs::read_dir(cas_root.join("pins"))?
    .next()
    .expect("cold execution should publish one pin")?
    .path();
  let pin: serde_json::Value = serde_json::from_slice(&fs::read(pin_path)?)?;
  let action_result = pin["action_result"].as_str().expect("action-result identity");
  let result_hex = action_result
    .strip_prefix("action-result-v1-sha256-")
    .expect("versioned action-result identity");
  let blob_path = fs::read_dir(cas_root.join("results").join(result_hex).join("blobs"))?
    .next()
    .expect("compiler result should contain a blob")?
    .path();
  fs::write(&blob_path, b"x")?;

  let corrupt = run_cargo_rail_with_env(&ws.path, &command, &environment)?;
  assert_eq!(corrupt.status.code(), Some(2), "corrupt cache must fail closed");
  let corrupt_stderr = String::from_utf8_lossy(&corrupt.stderr);
  assert!(
    corrupt_stderr.contains("local action cache corrupt") && corrupt_stderr.contains("blob_"),
    "corruption explanation must identify the rejected object:\n{corrupt_stderr}"
  );
  assert!(
    !corrupt_stderr.contains("Checking cached-lib"),
    "a corrupt object must not authorize reuse or trigger an implicit cold execution"
  );

  let disabled = run_cargo_rail_with_env(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--hermetic",
      "--no-cache",
      "--explain",
    ],
    &environment,
  )?;
  assert!(disabled.status.success(), "explicitly disabled cache execution failed");
  let disabled_stdout = String::from_utf8_lossy(&disabled.stdout);
  assert!(
    disabled_stdout.contains("local cache: disabled (disabled_by_request)")
      && disabled_stdout.contains("cargo_check_executed=true compiler_units_executed=true"),
    "disabled explanation must identify the cold execution:\n{disabled_stdout}"
  );

  let cleaned = run_cargo_rail_with_env(&ws.path, &["rail", "clean", "--cache"], &environment)?;
  assert!(
    cleaned.status.success(),
    "cache cleanup failed:\n{}",
    String::from_utf8_lossy(&cleaned.stderr)
  );
  assert!(
    !cas_root.exists(),
    "clean --cache must remove the validated owned CAS root"
  );
  assert_eq!(
    fs::read_to_string(cache.path().join("keep"))?,
    "outside owned CAS root\n",
    "cleanup must not remove the configured cache base or unrelated files"
  );
  Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn test_local_action_cache_concurrent_cold_writers_converge() -> Result<()> {
  fn fixture(name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named(name)?;
    ws.add_crate("concurrent-lib", "0.1.0", &[])?;
    generate_lockfile(&ws.path)?;
    ws.commit("Add concurrent cache fixture")?;
    Ok(ws)
  }

  let first = fixture("local-cache-concurrent-first")?;
  let second = fixture("local-cache-concurrent-second")?;
  let cache = tempfile::tempdir()?;
  let cache_path = cache.path().to_string_lossy().into_owned();
  let command = ["rail", "run", "--all", "--action", "build", "--hermetic"];
  let barrier = Arc::new(std::sync::Barrier::new(2));
  let first_path = &first.path;
  let second_path = &second.path;
  let command_ref = &command;
  let outputs = std::thread::scope(|scope| {
    let first_barrier = Arc::clone(&barrier);
    let first_cache = cache_path.clone();
    let first_handle = scope.spawn(move || {
      first_barrier.wait();
      run_cargo_rail_with_env(
        first_path,
        command_ref,
        &[("CARGO_RAIL_CACHE_DIR", first_cache.as_str())],
      )
    });
    let second_barrier = Arc::clone(&barrier);
    let second_cache = cache_path.clone();
    let second_handle = scope.spawn(move || {
      second_barrier.wait();
      run_cargo_rail_with_env(
        second_path,
        command_ref,
        &[("CARGO_RAIL_CACHE_DIR", second_cache.as_str())],
      )
    });
    [first_handle.join(), second_handle.join()]
  });
  for output in outputs {
    let output = output.expect("concurrent cargo-rail process should not panic")?;
    assert!(
      output.status.success(),
      "concurrent writer failed:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
  }
  let cas_root = cache.path().join("cargo-rail/local-cas-v1");
  assert_eq!(fs::read_dir(cas_root.join("pins"))?.count(), 1, "one action-key pin");
  assert_eq!(
    fs::read_dir(cas_root.join("results"))?.count(),
    1,
    "concurrent identical writers must publish one complete result bundle"
  );
  assert_eq!(
    fs::read_dir(cas_root.join("staging"))?.count(),
    0,
    "no partial staging roots"
  );
  assert_eq!(
    fs::read_dir(cas_root.join("leases"))?.count(),
    0,
    "no live leases after exit"
  );

  let hit = run_cargo_rail_with_env(
    &second.path,
    &["rail", "run", "--all", "--action", "build", "--hermetic", "--explain"],
    &[("CARGO_RAIL_CACHE_DIR", cache_path.as_str())],
  )?;
  assert!(hit.status.success());
  assert!(
    String::from_utf8_lossy(&hit.stdout).contains("local cache: hit")
      && !String::from_utf8_lossy(&hit.stderr).contains("Checking concurrent-lib"),
    "published concurrent result must be reusable without compiler execution"
  );
  Ok(())
}

#[test]
#[cfg_attr(
  windows,
  ignore = "isolated dependency-backed hermetic execution is not graduated on Windows"
)]
fn test_hermetic_fetch_inventory_converges_for_locked_git_dependency() -> Result<()> {
  let dependency = tempfile::tempdir()?;
  git(dependency.path(), &["init", "--initial-branch=main"])?;
  git(dependency.path(), &["config", "user.name", "Test User"])?;
  git(dependency.path(), &["config", "user.email", "test@example.com"])?;
  fs::create_dir_all(dependency.path().join("src"))?;
  fs::write(
    dependency.path().join("Cargo.toml"),
    "[package]\nname = \"external-dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
  )?;
  fs::write(
    dependency.path().join("src/lib.rs"),
    "pub const fn value() -> u8 { 7 }\n",
  )?;
  git(dependency.path(), &["add", "."])?;
  git(dependency.path(), &["commit", "-m", "Add external dependency"])?;
  let revision = String::from_utf8(git(dependency.path(), &["rev-parse", "HEAD"])?.stdout)?;
  let revision = revision.trim();
  #[cfg(windows)]
  let url = format!(
    "file:///{}",
    cargo_rail::utils::canonicalize_existing(dependency.path())?
      .display()
      .to_string()
      .replace('\\', "/")
  );
  #[cfg(not(windows))]
  let url = format!("file://{}", dependency.path().display());
  let dependency_spec = format!("{{ git = {url:?}, rev = {revision:?} }}");

  let first = TestWorkspace::new_named("hermetic-git-first")?;
  first.add_crate("consumer", "0.1.0", &[("external-dep", dependency_spec.as_str())])?;
  fs::write(
    first.path.join("crates/consumer/src/lib.rs"),
    "pub fn value() -> u8 { external_dep::value() }\n",
  )?;
  generate_lockfile(&first.path)?;
  first.commit("Add locked Git consumer")?;
  let second = TestWorkspace::new_named("hermetic-git-second")?;
  second.add_crate("consumer", "0.1.0", &[("external-dep", dependency_spec.as_str())])?;
  fs::write(
    second.path.join("crates/consumer/src/lib.rs"),
    "pub fn value() -> u8 { external_dep::value() }\n",
  )?;
  generate_lockfile(&second.path)?;
  second.commit("Add locked Git consumer")?;

  let command = [
    "rail",
    "run",
    "--since",
    "HEAD~1",
    "--action",
    "build",
    "--hermetic",
    "--no-cache",
  ];
  let ambient_home = tempfile::tempdir()?;
  let ambient_home_value = ambient_home.path().to_string_lossy().into_owned();
  let environment = [
    ("HOME", ambient_home_value.as_str()),
    ("USERPROFILE", ambient_home_value.as_str()),
  ];
  for workspace in [&first, &second] {
    let output = run_cargo_rail_with_env(&workspace.path, &command, &environment)?;
    assert!(
      output.status.success(),
      "hermetic Git build failed\nstdout:\n{}\nstderr:\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
    assert!(
      !ambient_home.path().join(".cargo/git").exists() && !ambient_home.path().join(".cargo/registry").exists(),
      "full metadata must not acquire the dependency in ambient Cargo state before the explicit fetch action"
    );
  }
  let report = |workspace: &Path| -> Result<serde_json::Value> {
    let directory = workspace.join("target/cargo-rail/hermetic/reports");
    let path = fs::read_dir(directory)?
      .next()
      .transpose()?
      .expect("one hermetic report")
      .path();
    Ok(serde_json::from_slice(&fs::read(path)?)?)
  };
  let first_report = report(&first.path)?;
  let second_report = report(&second.path)?;
  assert_eq!(first_report["fetch"]["reused"], false);
  assert_eq!(second_report["fetch"]["reused"], false);
  assert_eq!(
    first_report["fetch"]["packages"], 1,
    "locked Git package must be captured in the immutable inventory: {first_report:#}"
  );
  assert!(
    first_report["fetch"]["source_entries"]
      .as_u64()
      .is_some_and(|entries| entries > 0)
  );
  assert_eq!(
    first_report["fetch"]["result_digest"],
    second_report["fetch"]["result_digest"]
  );
  assert_eq!(first_report["action_key"], second_report["action_key"]);
  assert_eq!(
    first_report["output_manifest"]["digest"],
    second_report["output_manifest"]["digest"]
  );

  let warm = run_cargo_rail_with_env(&first.path, &command, &environment)?;
  assert!(
    warm.status.success(),
    "warm hermetic Git build failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&warm.stdout),
    String::from_utf8_lossy(&warm.stderr)
  );
  let reports = fs::read_dir(first.path.join("target/cargo-rail/hermetic/reports"))?
    .map(|entry| -> Result<serde_json::Value> { Ok(serde_json::from_slice(&fs::read(entry?.path())?)?) })
    .collect::<Result<Vec<_>>>()?;
  assert_eq!(reports.len(), 2);
  assert!(reports.iter().any(|report| report["fetch"]["reused"] == true));
  assert!(
    reports
      .iter()
      .all(|report| report["result_digest"] == first_report["result_digest"])
  );
  Ok(())
}

#[test]
#[cfg_attr(
  windows,
  ignore = "isolated dependency-backed hermetic execution is not graduated on Windows"
)]
fn test_hermetic_sparse_registry_fetch_is_the_only_network_boundary_and_reuses_warm_inventory() -> Result<()> {
  let dependency = tempfile::tempdir()?;
  fs::create_dir_all(dependency.path().join("src"))?;
  fs::write(
    dependency.path().join("Cargo.toml"),
    "[package]\nname = \"external-dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\ndescription = \"hermetic registry fixture\"\n",
  )?;
  fs::write(
    dependency.path().join("src/lib.rs"),
    "pub const fn value() -> u8 { 11 }\n",
  )?;
  let packaged = std::process::Command::new("cargo")
    .current_dir(dependency.path())
    .args(["package", "--allow-dirty", "--no-verify"])
    .output()?;
  anyhow::ensure!(
    packaged.status.success(),
    "registry fixture packaging failed:\n{}",
    String::from_utf8_lossy(&packaged.stderr)
  );
  let archive = fs::read(dependency.path().join("target/package/external-dep-0.1.0.crate"))?;
  let checksum = Sha256::digest(&archive)
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<Vec<_>>()
    .join("");
  let registry = SparseRegistry::start(archive, &checksum)?;

  let create_workspace = |name: &str| -> Result<TestWorkspace> {
    let workspace = TestWorkspace::new_named(name)?;
    workspace.add_crate("consumer", "0.1.0", &[("external-dep", "\"=0.1.0\"")])?;
    fs::create_dir_all(workspace.path.join(".cargo"))?;
    fs::write(
      workspace.path.join(".cargo/config.toml"),
      format!(
        "[source.crates-io]\nreplace-with = \"fixture\"\n\n[source.fixture]\nregistry = {:?}\n",
        registry.index
      ),
    )?;
    fs::write(
      workspace.path.join("crates/consumer/src/lib.rs"),
      "pub fn value() -> u8 { external_dep::value() }\n",
    )?;
    Ok(workspace)
  };
  let first = create_workspace("hermetic-registry-first")?;
  let second = create_workspace("hermetic-registry-second")?;
  let lock_home = tempfile::tempdir()?;
  let lock_home_value = lock_home.path().to_string_lossy().into_owned();
  let lock_environment = [("CARGO_HOME", lock_home_value.as_str())];
  for workspace in [&first, &second] {
    generate_lockfile_with_env(&workspace.path, &lock_environment)?;
    workspace.commit("Add locked sparse-registry consumer")?;
  }

  let ambient_home = tempfile::tempdir()?;
  let ambient_home_value = ambient_home.path().to_string_lossy().into_owned();
  let environment = [
    ("HOME", ambient_home_value.as_str()),
    ("USERPROFILE", ambient_home_value.as_str()),
  ];
  let command = ["rail", "run", "--all", "--action", "build", "--hermetic", "--no-cache"];
  let mut cold_reports = Vec::new();
  for workspace in [&first, &second] {
    registry.begin(&workspace.path)?;
    let output = run_cargo_rail_with_env(&workspace.path, &command, &environment)?;
    assert!(
      output.status.success(),
      "hermetic sparse-registry build failed\nstdout:\n{}\nstderr:\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
    let observations = registry.observations()?;
    assert!(
      observations.requests > 0,
      "a cold inventory must fetch the registry package"
    );
    assert!(
      !observations.before_fetch_boundary,
      "registry access occurred before cargo-rail established the explicit fetch boundary"
    );
    assert!(
      !observations.during_build,
      "the offline build attempted registry access after fetch"
    );
    assert!(
      !ambient_home.path().join(".cargo/registry").exists(),
      "dependency acquisition escaped into ambient Cargo state"
    );
    let report_path = fs::read_dir(workspace.path.join("target/cargo-rail/hermetic/reports"))?
      .next()
      .transpose()?
      .expect("one cold hermetic report")
      .path();
    cold_reports.push(serde_json::from_slice::<serde_json::Value>(&fs::read(report_path)?)?);
  }
  assert_eq!(cold_reports[0]["fetch"]["packages"], 1);
  assert!(
    cold_reports
      .iter()
      .all(|report| report["compiler_units"].as_u64().is_some_and(|units| units >= 2)),
    "the external dependency and workspace consumer must both be observed"
  );
  assert!(cold_reports.iter().all(|report| {
    report["output_manifest"]["entries"].as_array().is_some_and(|entries| {
      entries
        .iter()
        .filter(|entry| entry["path"].as_str().is_some_and(|path| path.ends_with(".d")))
        .count()
        >= 2
    })
  }));
  assert!(cold_reports.iter().all(|report| report["fetch"]["reused"] == false));
  assert_eq!(
    cold_reports[0]["fetch"]["result_digest"],
    cold_reports[1]["fetch"]["result_digest"]
  );
  assert_eq!(cold_reports[0]["action_key"], cold_reports[1]["action_key"]);
  assert_eq!(
    cold_reports[0]["output_manifest"]["digest"],
    cold_reports[1]["output_manifest"]["digest"]
  );

  registry.begin(&first.path)?;
  let warm = run_cargo_rail_with_env(&first.path, &command, &environment)?;
  assert!(
    warm.status.success(),
    "warm sparse-registry build failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&warm.stdout),
    String::from_utf8_lossy(&warm.stderr)
  );
  assert_eq!(
    registry.observations()?.requests,
    0,
    "a validated warm inventory must not touch the registry"
  );
  let reports = fs::read_dir(first.path.join("target/cargo-rail/hermetic/reports"))?
    .map(|entry| -> Result<serde_json::Value> { Ok(serde_json::from_slice(&fs::read(entry?.path())?)?) })
    .collect::<Result<Vec<_>>>()?;
  assert_eq!(reports.len(), 2);
  assert!(reports.iter().any(|report| report["fetch"]["reused"] == true));
  assert!(
    reports
      .iter()
      .all(|report| report["result_digest"] == cold_reports[0]["result_digest"])
  );
  Ok(())
}

#[test]
fn test_hermetic_all_target_check_proves_pure_rust_unit_classes_in_two_roots() -> Result<()> {
  let create = |name: &str| -> Result<TestWorkspace> {
    let workspace = TestWorkspace::new_named(name)?;
    let crate_root = workspace.add_crate("all-targets", "0.1.0", &[])?;
    fs::write(
      crate_root.join("src/main.rs"),
      "fn main() { let _ = all_targets::hello(); }\n",
    )?;
    for (directory, file, body) in [
      (
        "tests",
        "smoke.rs",
        "#[test]\nfn smoke() { assert!(!all_targets::hello().is_empty()); }\n",
      ),
      ("examples", "demo.rs", "fn main() { let _ = all_targets::hello(); }\n"),
      (
        "benches",
        "throughput.rs",
        "#[test]\nfn throughput() { let _ = all_targets::hello(); }\n",
      ),
    ] {
      fs::create_dir_all(crate_root.join(directory))?;
      fs::write(crate_root.join(directory).join(file), body)?;
    }
    generate_lockfile(&workspace.path)?;
    workspace.commit("Add pure Rust target matrix")?;
    Ok(workspace)
  };
  let first = create("hermetic-all-targets-first")?;
  let second = create("hermetic-all-targets-second")?;
  let command = [
    "rail",
    "run",
    "--all",
    "--action",
    "build",
    "--hermetic",
    "--no-cache",
    "--",
    "--all-targets",
  ];
  for workspace in [&first, &second] {
    let output = run_cargo_rail(&workspace.path, &command)?;
    assert!(
      output.status.success(),
      "all-target hermetic check failed\nstdout:\n{}\nstderr:\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
  }
  let report = |workspace: &Path| -> Result<serde_json::Value> {
    let path = fs::read_dir(workspace.join("target/cargo-rail/hermetic/reports"))?
      .next()
      .transpose()?
      .expect("one hermetic report")
      .path();
    Ok(serde_json::from_slice(&fs::read(path)?)?)
  };
  let first_report = report(&first.path)?;
  let second_report = report(&second.path)?;
  assert!(first_report["compiler_units"].as_u64().is_some_and(|units| units >= 5));
  assert_eq!(first_report["action_key"], second_report["action_key"]);
  assert_eq!(
    first_report["output_manifest"]["digest"],
    second_report["output_manifest"]["digest"]
  );
  #[cfg(target_os = "macos")]
  assert_eq!(
    first_report["support"], "eligible",
    "unexpected hermetic report: {first_report:#}"
  );
  #[cfg(not(target_os = "macos"))]
  assert_eq!(first_report["support"], "platform_limited");
  Ok(())
}

#[test]
fn test_hermetic_build_script_stays_executable_normally_but_fails_closed_for_reuse() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-build-script")?;
  let crate_root = workspace.add_crate("generated", "0.1.0", &[])?;
  fs::write(
    crate_root.join("build.rs"),
    "fn main() { println!(\"cargo::rustc-cfg=generated_by_build_script\"); }\n",
  )?;
  workspace.commit("Add build script")?;

  let normal = run_cargo_rail(&workspace.path, &["rail", "run", "--all", "--action", "build"])?;
  assert!(
    normal.status.success(),
    "ordinary execution must remain available\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&normal.stdout),
    String::from_utf8_lossy(&normal.stderr)
  );
  let hermetic = run_cargo_rail(
    &workspace.path,
    &["rail", "run", "--all", "--action", "build", "--hermetic"],
  )?;
  assert_eq!(hermetic.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&hermetic.stderr).contains("build_scripts_not_hermetic"),
    "the unsupported script boundary must be explicit: {}",
    String::from_utf8_lossy(&hermetic.stderr)
  );
  assert!(!workspace.path.join("target/cargo-rail/hermetic").exists());
  Ok(())
}

#[test]
fn test_hermetic_proc_macro_fails_before_fetch_state() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-proc-macro")?;
  let crate_root = workspace.add_crate("derive-fixture", "0.1.0", &[])?;
  let manifest_path = crate_root.join("Cargo.toml");
  let mut manifest = fs::read_to_string(&manifest_path)?;
  manifest.push_str("\n[lib]\nproc-macro = true\n");
  fs::write(manifest_path, manifest)?;
  fs::write(
    crate_root.join("src/lib.rs"),
    "use proc_macro::TokenStream;\n#[proc_macro]\npub fn passthrough(input: TokenStream) -> TokenStream { input }\n",
  )?;
  workspace.commit("Add proc macro")?;

  let output = run_cargo_rail(
    &workspace.path,
    &["rail", "run", "--all", "--action", "build", "--hermetic"],
  )?;
  assert_eq!(output.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("proc_macros_not_hermetic"),
    "the unsupported host executable must remain explicit: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!workspace.path.join("target/cargo-rail/hermetic").exists());
  Ok(())
}

#[test]
fn test_hermetic_rejects_unsupported_action_before_mutating_outputs() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-unsupported-docs")?;
  workspace.add_crate("documented", "0.1.0", &[])?;
  workspace.commit("Add documented crate")?;
  let output = run_cargo_rail(
    &workspace.path,
    &["rail", "run", "--all", "--action", "docs", "--hermetic"],
  )?;
  assert_eq!(output.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("does not yet support action 'docs' (docs)"),
    "unsupported class must be explicit: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!workspace.path.join("target/doc").exists());
  assert!(!workspace.path.join("target/cargo-rail/hermetic").exists());
  Ok(())
}

#[test]
fn test_hermetic_rejects_unmodeled_cargo_boundaries_before_fetch_state() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-cargo-boundary-overrides")?;
  workspace.add_crate("bounded", "0.1.0", &[])?;
  workspace.commit("Add bounded crate")?;

  for arguments in [
    vec!["--manifest-path", "other/Cargo.toml"],
    vec!["-C", "other-workspace"],
    vec!["--workspace"],
    vec!["--target-dir", "outside-target"],
    vec!["--", "--cfg", "unmodeled_compiler_input"],
  ] {
    let mut command = vec!["rail", "run", "--all", "--action", "build", "--hermetic", "--"];
    command.extend(arguments);
    let output = run_cargo_rail(&workspace.path, &command)?;
    assert_eq!(output.status.code(), Some(2));
    assert!(
      String::from_utf8_lossy(&output.stderr).contains("override the modeled action boundary"),
      "unmodeled Cargo boundary must be explicit: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    assert!(
      !workspace.path.join("target/cargo-rail/hermetic").exists(),
      "unmodeled Cargo arguments must fail before fetch state"
    );
  }
  Ok(())
}

#[test]
fn test_hermetic_rejects_tool_overrides_without_invoking_wrappers_or_creating_state() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-tool-overrides")?;
  workspace.add_crate("bounded-toolchain", "0.1.0", &[])?;
  fs::create_dir_all(workspace.path.join(".cargo"))?;
  fs::write(
    workspace.path.join(".cargo/config.toml"),
    "[build]\nrustc-wrapper = \"definitely-not-an-executable\"\n",
  )?;
  workspace.commit("Add rejected compiler wrapper")?;

  let command = ["rail", "run", "--all", "--action", "build", "--hermetic"];
  let configured = run_cargo_rail(&workspace.path, &command)?;
  assert_eq!(configured.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&configured.stderr).contains("cargo_tool_override_not_graduated"),
    "configured wrapper must fail before invocation: {}",
    String::from_utf8_lossy(&configured.stderr)
  );
  assert!(!workspace.path.join("target/cargo-rail/hermetic").exists());

  fs::remove_file(workspace.path.join(".cargo/config.toml"))?;
  let environment = [("RUSTC", "definitely-not-an-executable")];
  let ambient = run_cargo_rail_with_env(&workspace.path, &command, &environment)?;
  assert_eq!(ambient.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&ambient.stderr).contains("cargo_tool_override_not_graduated"),
    "ambient compiler override must fail before invocation: {}",
    String::from_utf8_lossy(&ambient.stderr)
  );
  assert!(!workspace.path.join("target/cargo-rail/hermetic").exists());
  Ok(())
}

#[test]
fn test_hermetic_build_requires_an_exact_lockfile_before_fetch() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-missing-lockfile")?;
  workspace.add_crate("unlocked", "0.1.0", &[])?;
  workspace.commit("Add unlocked crate")?;

  let output = run_cargo_rail(
    &workspace.path,
    &["rail", "run", "--all", "--action", "build", "--hermetic"],
  )?;
  assert_eq!(output.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("requires exact Cargo.lock"),
    "the dependency boundary must reject an implicit resolution: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    !workspace.path.join("target/cargo-rail/hermetic").exists(),
    "a missing lockfile must fail before creating fetch state"
  );
  Ok(())
}

#[test]
fn test_hermetic_workspace_root_runs_from_outside_the_repository() -> Result<()> {
  let workspace = TestWorkspace::new_named("hermetic-explicit-workspace-root")?;
  workspace.add_crate("rooted", "0.1.0", &[])?;
  generate_lockfile(&workspace.path)?;
  workspace.commit("Add locked crate")?;
  let outside = tempfile::tempdir()?;
  let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(outside.path())
    .arg("rail")
    .arg("--workspace-root")
    .arg(&workspace.path)
    .args(["run", "--all", "--action", "build", "--hermetic", "--no-cache"])
    .output()?;
  assert!(
    output.status.success(),
    "explicit hermetic workspace root failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(workspace.path.join("target/cargo-rail/hermetic/reports").is_dir());
  assert!(!outside.path().join("target").exists());
  Ok(())
}

#[test]
fn test_test_action_features_follow_backend_arguments_not_harness_arguments() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-test-feature-domain")?;
  let feature_crate = ws.add_crate("feature-crate", "0.1.0", &[])?;
  let manifest_path = feature_crate.join("Cargo.toml");
  let mut manifest = std::fs::read_to_string(&manifest_path)?;
  manifest.push_str("\n[features]\nbackend-feature = []\n");
  std::fs::write(manifest_path, manifest)?;
  ws.commit("Add feature crate")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "test",
      "--test-runner",
      "cargo",
      "--cargo-test-arg=--no-default-features",
      "--cargo-test-arg=--features",
      "--cargo-test-arg=backend-feature",
      "--cargo-test-arg=--target=x86_64-unknown-linux-gnu",
      "--dry-run",
      "--format",
      "json",
      "--",
      "--features",
      "harness-feature",
      "--target=wasm32-unknown-unknown",
    ],
  )?;
  assert!(
    output.status.success(),
    "action plan failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let features = &json["actions"][0]["selected_features"];
  assert_eq!(features["all_features"], false);
  assert_eq!(features["default_features"], false);
  assert_eq!(features["named"], serde_json::json!(["backend-feature"]));
  assert_eq!(
    json["actions"][0]["selected_targets"],
    serde_json::json!(["x86_64-unknown-linux-gnu"])
  );

  Ok(())
}

#[test]
fn test_build_action_uses_effective_cargo_build_target() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-build-target")?;
  ws.add_crate("targeted-crate", "0.1.0", &[])?;
  std::fs::create_dir_all(ws.path.join(".cargo"))?;
  std::fs::write(
    ws.path.join(".cargo/config.toml"),
    "[build]\ntarget = \"wasm32-unknown-unknown\"\n",
  )?;
  ws.commit("Select the Cargo build target")?;

  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--dry-run",
      "--format",
      "json",
    ],
  )?;
  assert!(
    output.status.success(),
    "action plan failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  assert_eq!(
    json["actions"][0]["selected_targets"],
    serde_json::json!(["wasm32-unknown-unknown"])
  );
  assert_eq!(
    json["actions"][0]["resolution_views"][0]["target"],
    "wasm32-unknown-unknown"
  );

  Ok(())
}

#[test]
fn test_repository_output_collision_fails_before_any_action_process() -> Result<()> {
  let ws = TestWorkspace::new_named("test-repository-output-collision")?;
  ws.add_crate("collision-crate", "0.1.0", &[])?;
  std::fs::write(
    ws.path.join(".config/rail.toml"),
    r#"[run.action.first]
kind = "generated"
argv = ["definitely-not-an-executable", "regenerate"]
check_argv = ["definitely-not-an-executable", "check"]
when = ["build"]
outputs = ["generated"]

[run.action.second]
kind = "generated"
argv = ["definitely-not-an-executable", "regenerate"]
check_argv = ["definitely-not-an-executable", "check"]
when = ["build"]
outputs = ["generated/nested"]

[run.profile.collision]
actions = ["first", "second"]
"#,
  )?;
  ws.commit("Add colliding generated actions")?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--profile", "collision"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success());
  assert!(
    stderr.contains("overlaps"),
    "graph validation must report the collision: {stderr}"
  );
  assert!(
    !stderr.contains("definitely-not-an-executable failed"),
    "no action may spawn before whole-graph validation"
  );

  Ok(())
}

#[test]
fn test_all_action_preview_does_not_require_git_without_base_ref_inputs() -> Result<()> {
  let ws = TestWorkspace::new_named("test-run-all-no-git")?;
  ws.add_crate("filesystem-crate", "0.1.0", &[])?;
  std::fs::remove_dir_all(ws.path.join(".git"))?;

  let output = run_cargo_rail(&ws.path, &["rail", "run", "--all", "--action", "build", "--dry-run"])?;
  assert!(
    output.status.success(),
    "all-mode actions without base-ref inputs must work in Cargo-only trees: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(String::from_utf8(output.stdout)?, "build: cargo check --workspace\n");

  Ok(())
}

#[cfg(unix)]
#[test]
fn test_builtin_action_executes_the_captured_cargo_program() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let ws = TestWorkspace::new_single_crate("selected-cargo-program", "0.1.0")?;
  let tools = tempfile::tempdir()?;
  let cargo = tools.path().join("cargo proxy");
  let log = tools.path().join("cargo.log");
  std::fs::write(
    &cargo,
    "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$CARGO_SELECTION_LOG\"\nexec cargo \"$@\"\n",
  )?;
  std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))?;

  let output = run_cargo_rail_with_env(
    &ws.path,
    &[
      "rail",
      "run",
      "--quiet",
      "--all",
      "--action",
      "build",
      "--no-cache",
      "--",
      "--quiet",
    ],
    &[
      ("CARGO", cargo.to_str().expect("UTF-8 Cargo proxy path")),
      (
        "CARGO_SELECTION_LOG",
        log.to_str().expect("UTF-8 Cargo selection log path"),
      ),
      ("RUSTC_WRAPPER", ""),
      ("RUSTC_WORKSPACE_WRAPPER", ""),
    ],
  )?;
  assert!(
    output.status.success(),
    "selected Cargo execution failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let invocations = std::fs::read_to_string(log)?;
  assert!(
    invocations.lines().any(|argument| argument == "check"),
    "snapshot capture used the selected Cargo program, but action execution did not:\n{invocations}"
  );
  Ok(())
}
