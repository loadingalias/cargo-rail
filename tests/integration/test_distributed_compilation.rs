use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use cargo_rail::source::ContentDigest;
use serde::{Deserialize, Serialize};

const REQUEST_MAGIC: &[u8; 8] = b"CRXREQ3\0";
const REQUEST_TRAILER: &[u8; 8] = b"CRXEND3\0";
const RESPONSE_MAGIC: &[u8; 8] = b"CRXRES3\0";
const RESPONSE_TRAILER: &[u8; 8] = b"CRXDONE3";
const CANCEL_MAGIC: &[u8; 8] = b"CRXCAN3\0";
const CANCEL_TRAILER: &[u8; 8] = b"CRXCEND3";
const CAPABILITY_MAGIC: &[u8; 8] = b"CRXCAP3\0";
const CAPABILITY_TRAILER: &[u8; 8] = b"CRXCPEN3";
const VIRTUAL_ROOT: &str = "/cargo-rail/exec/v3";
const VIRTUAL_WORKSPACE: &str = "/cargo-rail/exec/v3/workspace";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerCapability {
    architecture: String,
    capability_id: String,
    endianness: String,
    environment_contract: String,
    filesystem_contract: String,
    host_target: String,
    isolation: String,
    isolation_identity: String,
    operating_system: String,
    operation_classes: Vec<String>,
    platform_family: String,
    protocol_version: u32,
    resource_limits: ExecutionLimits,
    rustc_content_digest: String,
    rustc_verbose_version: String,
    sysroot_identity: String,
    working_directory_contract: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ExecutionLimits {
    cpu_period_micros: u64,
    cpu_quota_micros: u64,
    max_output_bytes: u64,
    max_processes: u32,
    max_stream_bytes: u64,
    memory_bytes: u64,
    scratch_bytes: u64,
    wall_time_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct RustLibraryOperation {
    cap_lints: Option<String>,
    cargo_json_diagnostics: bool,
    check_cfg: Vec<String>,
    codegen: RustLibraryCodegen,
    color: Option<String>,
    crate_name: String,
    crate_type: String,
    cfg: Vec<String>,
    dependencies: Vec<RustLibraryDependency>,
    diagnostic_width: Option<u32>,
    dep_info_name: String,
    edition: String,
    emission: String,
    extra_filename: String,
    metadata: String,
    metadata_name: String,
    lints: Vec<RustLibraryLint>,
    operation_class: String,
    output_relative_directory: String,
    output_dependency_search: bool,
    rlib_name: Option<String>,
    source_virtual_path: String,
    test_mode: bool,
    toolchain_proc_macro: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RustLibraryDependency {
    extern_name: String,
    virtual_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct RustLibraryLint {
    level: String,
    name: String,
}

#[derive(Debug, Clone, Serialize)]
struct RustLibraryCodegen {
    codegen_units: Option<u32>,
    debuginfo: Option<String>,
    debug_assertions: Option<bool>,
    embed_bitcode: Option<bool>,
    linker_plugin_lto: Option<bool>,
    lto: Option<String>,
    opt_level: Option<String>,
    overflow_checks: Option<bool>,
    panic: Option<String>,
    prefer_dynamic: Option<bool>,
    split_debuginfo: Option<String>,
    strip: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct InputFrame {
    bytes: u64,
    content_digest: String,
    kind: String,
    virtual_path: String,
}

#[derive(Debug, Serialize)]
struct ExecutionRequest {
    action_id: String,
    capability_id: String,
    inputs: Vec<InputFrame>,
    lease_id: String,
    limits: ExecutionLimits,
    operation: RustLibraryOperation,
    protocol_version: u32,
    workload_identity: String,
}

#[derive(Debug, Deserialize)]
struct ExecutionResponse {
    action_id: String,
    capability_id: String,
    frames: Vec<ResponseFrame>,
    lease_id: String,
    protocol_version: u32,
    reason: Option<String>,
    status: String,
    termination: Option<CompilerTermination>,
    workload_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CompilerTermination {
    Exit { code: i32 },
    Signal { signal: i32 },
    Unknown,
}

#[derive(Debug, Deserialize)]
struct ResponseFrame {
    bytes: u64,
    content_digest: String,
    mode: u32,
    slot: String,
}

struct DecodedResponse {
    header: ExecutionResponse,
    frames: BTreeMap<String, Vec<u8>>,
}

#[test]
fn one_shot_worker_matches_local_rustc_and_honors_cancellation() -> Result<()> {
    let worker = Path::new(env!("CARGO_BIN_EXE_cargo-rail-distributed-worker"));
    let rustc = which_rustc()?;
    let version = Command::new(worker).arg("protocol-version").output()?;
    anyhow::ensure!(version.status.success(), "worker protocol query failed: {version:?}");
    anyhow::ensure!(version.stdout == b"3\n", "unexpected worker protocol version");

    let qualification = Command::new(worker)
        .args(["qualify-local-client"])
        .arg(&rustc)
        .output()?;
    anyhow::ensure!(
        qualification.status.success(),
        "local client qualification failed: {qualification:?}"
    );
    anyhow::ensure!(
        qualification.stdout == b"3\n" && qualification.stderr.is_empty(),
        "local client qualification contaminated its machine output"
    );

    let capability_output = Command::new(worker).args(["capability"]).arg(&rustc).output()?;
    anyhow::ensure!(
        capability_output.status.success(),
        "worker capability query failed: {capability_output:?}"
    );
    let capability: WorkerCapability = serde_json::from_slice(&capability_output.stdout)?;
    anyhow::ensure!(capability.protocol_version == 3, "unexpected capability protocol");
    anyhow::ensure!(
        capability.resource_limits
            == ExecutionLimits {
                cpu_period_micros: 100_000,
                cpu_quota_micros: 100_000,
                max_output_bytes: 64 * 1024 * 1024,
                max_processes: 64,
                max_stream_bytes: 8 * 1024 * 1024,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                scratch_bytes: 512 * 1024 * 1024,
                wall_time_ms: 120_000,
            },
        "worker capability did not bind the fixed resource contract"
    );
    anyhow::ensure!(
        capability.isolation == "process_only_unqualified",
        "worker overstated its isolation qualification"
    );
    anyhow::ensure!(
        capability.operation_classes == ["rust_library"],
        "unexpected operation classes"
    );
    anyhow::ensure!(
        capability.capability_id == capability_identity(&capability)?,
        "capability identity did not bind the advertised execution environment"
    );

    let source = b"#![forbid(unsafe_code)]\npub fn answer() -> u64 { 42 }\n";
    let request = execution_request(&capability, source)?;
    let local = compile_locally(&rustc, &request.operation, source)?;
    let result = run_worker(worker, &rustc, &request, source, false)?;
    assert_success_authority(&result.header, &request)?;
    assert_frame_digests(&result)?;

    for slot in ["dep_info", "metadata", "rlib", "stderr", "stdout"] {
        let distributed = result.frames.get(slot).context("distributed result slot")?;
        let local = local.get(slot).context("local result slot")?;
        if distributed != local {
            let first_difference = distributed
                .iter()
                .zip(local)
                .position(|(distributed, local)| distributed != local);
            anyhow::bail!(
                "distributed {slot} differed from the same local rustc operation: distributed={} bytes {}, local={} bytes {}, first_difference={first_difference:?}",
                distributed.len(),
                digest(distributed),
                local.len(),
                digest(local),
            );
        }
    }

    let metadata_request = execution_request_with_emission(&capability, source, "metadata")?;
    let metadata_local = compile_locally(&rustc, &metadata_request.operation, source)?;
    let metadata_result = run_worker(worker, &rustc, &metadata_request, source, false)?;
    assert_success_authority(&metadata_result.header, &metadata_request)?;
    assert_frame_digests(&metadata_result)?;
    anyhow::ensure!(
        metadata_result.frames.keys().map(String::as_str).collect::<Vec<_>>()
            == ["dep_info", "metadata", "stderr", "stdout"],
        "metadata-only execution returned the wrong slot set"
    );
    anyhow::ensure!(
        metadata_result.frames == metadata_local,
        "metadata-only execution changed local rustc output"
    );

    let cancelled = run_worker(worker, &rustc, &request, source, true)?;
    anyhow::ensure!(cancelled.header.status == "rejected", "cancellation was not rejected");
    anyhow::ensure!(
        cancelled.header.reason.as_deref() == Some("execution_cancelled"),
        "unexpected cancellation result: {:?}",
        cancelled.header.reason
    );
    anyhow::ensure!(
        cancelled.frames.is_empty(),
        "cancelled execution returned artifact payloads"
    );
    anyhow::ensure!(
        cancelled.header.termination.is_none(),
        "cancelled execution reported a compiler termination"
    );

    let invalid_source = b"pub fn does_not_compile( {\n";
    let invalid_request = execution_request(&capability, invalid_source)?;
    let compiler_failure = run_worker(worker, &rustc, &invalid_request, invalid_source, false)?;
    anyhow::ensure!(
        compiler_failure.header.status == "compiler_failed",
        "invalid source was not reported as a compiler failure"
    );
    anyhow::ensure!(
        compiler_failure.header.termination.is_some()
            && compiler_failure.header.termination != Some(CompilerTermination::Exit { code: 0 }),
        "compiler failure reported successful termination"
    );
    anyhow::ensure!(
        compiler_failure.header.reason.is_none(),
        "compiler failure carried a worker rejection"
    );
    anyhow::ensure!(
        compiler_failure.frames.keys().map(String::as_str).collect::<Vec<_>>() == ["stderr", "stdout"],
        "compiler failure returned non-diagnostic artifacts"
    );
    anyhow::ensure!(
        !compiler_failure.frames["stderr"].is_empty(),
        "compiler failure dropped rustc diagnostics"
    );
    assert_frame_digests(&compiler_failure)?;
    Ok(())
}

#[test]
fn first_seen_compiler_environment_executes_locally_before_distribution() -> Result<()> {
    let workspace = crate::helpers::TestWorkspace::new_single_crate("observed_environment", "0.1.0")?;
    fs::write(
        workspace.path.join("src/lib.rs"),
        "pub const OBSERVED: &str = env!(\"TASK10_OBSERVED_ENV\");\n",
    )?;
    let cargo_home = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    let coverage = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
    }
    let coverage = fs::canonicalize(coverage.path())?;
    let setup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "setup", "--local-dir"])
        .arg(cache.path())
        .arg("--distributed-local")
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(setup.status.success(), "distributed local setup failed: {setup:?}");

    let built = Command::new("cargo")
        .current_dir(&workspace.path)
        .args(["build", "--release", "--lib", "--message-format=json"])
        .env("CARGO_HOME", cargo_home.path())
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
        .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
        .env("TASK10_OBSERVED_ENV", "authority-must-be-observed")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(
        built.status.success(),
        "first-seen compiler environment did not fall back locally:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        events.iter().any(|event| {
            event["status"] == "miss"
                && event["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("environment_selector_not_found"))
        }),
        "first-seen environment did not publish a locally observed miss: {events:?}"
    );
    anyhow::ensure!(
        events
            .iter()
            .all(|event| event["reason"] != "verified_distributed_execution"),
        "an unobserved compiler environment reached the worker: {events:?}"
    );
    Ok(())
}

#[test]
fn ordinary_cargo_distributes_module_trees_and_exact_rust_dependencies() -> Result<()> {
    let workspace = crate::helpers::TestWorkspace::new()?;
    let dependency = workspace.add_crate("task10-dep", "0.1.0", &[])?;
    fs::write(
        dependency.join("src/lib.rs"),
        "pub mod nested;\npub fn dependency_value() -> u64 { nested::value() }\n",
    )?;
    fs::write(dependency.join("src/nested.rs"), "pub fn value() -> u64 { 41 }\n")?;
    let consumer = workspace.add_crate("task10-app", "0.1.0", &[("task10-dep", "{ path = \"../task10-dep\" }")])?;
    fs::write(
        consumer.join("src/lib.rs"),
        "mod local;\npub fn answer() -> u64 { task10_dep::dependency_value() + local::one() }\n",
    )?;
    fs::write(consumer.join("src/local.rs"), "pub fn one() -> u64 { 1 }\n")?;

    let cargo_home = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    let coverage = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
    }
    let coverage = fs::canonicalize(coverage.path())?;
    let setup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "setup", "--local-dir"])
        .arg(cache.path())
        .arg("--distributed-local")
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(setup.status.success(), "distributed local setup failed: {setup:?}");

    let build = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["build", "--workspace", "--release", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let seeded = build()?;
    anyhow::ensure!(
        seeded.status.success(),
        "portable action seeding failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&seeded.stdout),
        String::from_utf8_lossy(&seeded.stderr)
    );

    fs::remove_dir_all(workspace.path.join("target"))?;
    let cargo_rail_cache = cache.path().join("cargo-rail");
    let roots = fs::read_dir(&cargo_rail_cache)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("local-cas-v2"))
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    let [root] = roots.as_slice() else {
        anyhow::bail!("test cache did not contain one local CAS root: {roots:?}");
    };
    let native_actions = root.path().join("native-actions-v2");
    for entry in fs::read_dir(&native_actions)? {
        let path = entry?.path();
        anyhow::ensure!(path.is_file(), "native action state contained a non-file entry");
        fs::remove_file(path)?;
    }
    for entry in fs::read_dir(&coverage)? {
        let path = entry?.path();
        anyhow::ensure!(path.is_file(), "coverage directory contained a non-file entry");
        fs::remove_file(path)?;
    }

    let distributed = build()?;
    anyhow::ensure!(
        distributed.status.success(),
        "dependency-bearing distributed build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&distributed.stdout),
        String::from_utf8_lossy(&distributed.stderr)
    );
    let events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    let hits = events
        .iter()
        .filter(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution")
        .count();
    anyhow::ensure!(
        hits == 2,
        "module/dependency build did not distribute both Rust actions: {events:?}"
    );

    fs::remove_dir_all(workspace.path.join("target"))?;
    for entry in fs::read_dir(&native_actions)? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let check = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--workspace", "--all-targets", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let seeded_check = check()?;
    anyhow::ensure!(
        seeded_check.status.success(),
        "metadata/test action seeding failed: {seeded_check:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
    for entry in fs::read_dir(&native_actions)? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let distributed_check = check()?;
    anyhow::ensure!(
        distributed_check.status.success(),
        "metadata/test distributed check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&distributed_check.stdout),
        String::from_utf8_lossy(&distributed_check.stderr)
    );
    let check_events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    let compiler_actions = check_events
        .iter()
        .filter(|event| event["action_key"].is_string())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        compiler_actions.len() >= 4
            && compiler_actions
                .iter()
                .all(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution"),
        "metadata/test compiler actions did not all use verified distributed execution: {check_events:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn mutual_tls_worker_executes_through_machine_owned_cargo_setup() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let worker = Path::new(env!("CARGO_BIN_EXE_cargo-rail-distributed-worker"));
    let rustc = which_rustc()?;
    let identity = generate_mutual_tls_identity()?;
    let bubblewrap = Path::new("/usr/bin/bwrap");
    let sandboxed = cfg!(target_os = "linux")
        && bubblewrap.is_file()
        && Command::new(worker)
            .arg("qualify-bubblewrap")
            .arg(&rustc)
            .arg(bubblewrap)
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout == b"3\n" && output.stderr.is_empty());
    let mut server_command = Command::new(worker);
    server_command.arg(if sandboxed {
        "serve-mtls-bubblewrap"
    } else {
        "serve-mtls"
    });
    server_command.arg(&rustc);
    if sandboxed {
        server_command.arg(bubblewrap);
    }
    let mut server = server_command
        .arg("127.0.0.1:0")
        .arg(&identity.server_certificate)
        .arg(&identity.server_private_key)
        .arg(&identity.authority_certificate)
        .arg("2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = server.stdout.take().context("worker server stdout")?;
    let mut startup = String::new();
    BufReader::new(stdout).read_line(&mut startup)?;
    anyhow::ensure!(!startup.is_empty(), "worker server emitted no startup authority");
    let startup: serde_json::Value = serde_json::from_str(startup.trim_end())?;
    anyhow::ensure!(
        startup["transport"] == "mutual_tls_1_3",
        "unexpected transport authority"
    );
    anyhow::ensure!(
        startup["isolation"]
            == if sandboxed {
                "bubblewrap_linux_v2"
            } else {
                "process_only_unqualified"
            },
        "unexpected worker isolation authority"
    );
    let endpoint = startup["address"].as_str().context("worker server address")?;
    let capability_id = startup["capability_id"]
        .as_str()
        .context("worker capability identity")?;
    let mut server = WorkerServer(server);

    let wrong_purpose = Command::new(worker)
        .args(["qualify-mtls-client"])
        .arg(&rustc)
        .arg(endpoint)
        .arg("localhost")
        .arg(capability_id)
        .arg(&identity.authority_certificate)
        .arg(&identity.server_certificate)
        .arg(&identity.server_private_key)
        .output()?;
    anyhow::ensure!(
        !wrong_purpose.status.success(),
        "server-only certificate was accepted as a client identity"
    );
    let wrong_name = Command::new(worker)
        .args(["qualify-mtls-client"])
        .arg(&rustc)
        .arg(endpoint)
        .arg("not-localhost.invalid")
        .arg(capability_id)
        .arg(&identity.authority_certificate)
        .arg(&identity.client_certificate)
        .arg(&identity.client_private_key)
        .output()?;
    anyhow::ensure!(!wrong_name.status.success(), "wrong TLS server name was accepted");
    let wrong_capability = Command::new(worker)
        .args(["qualify-mtls-client"])
        .arg(&rustc)
        .arg(endpoint)
        .arg("localhost")
        .arg(format!("worker-capability-v3:sha256:{}", "0".repeat(64)))
        .arg(&identity.authority_certificate)
        .arg(&identity.client_certificate)
        .arg(&identity.client_private_key)
        .output()?;
    anyhow::ensure!(
        !wrong_capability.status.success(),
        "unpinned worker capability was accepted"
    );
    let qualified = Command::new(worker)
        .args(["qualify-mtls-client"])
        .arg(&rustc)
        .arg(endpoint)
        .arg("localhost")
        .arg(capability_id)
        .arg(&identity.authority_certificate)
        .arg(&identity.client_certificate)
        .arg(&identity.client_private_key)
        .output()?;
    anyhow::ensure!(
        qualified.status.success(),
        "mTLS client qualification failed: {qualified:?}"
    );
    anyhow::ensure!(qualified.stdout == b"3\n", "mTLS qualification contaminated stdout");

    let workspace = crate::helpers::TestWorkspace::new_single_crate("mtls_front_door", "0.1.0")?;
    let cargo_home = tempfile::tempdir()?;
    let coverage = tempfile::tempdir()?;
    fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
    let coverage = fs::canonicalize(coverage.path())?;
    let setup_arguments = [
        "rail",
        "cache",
        "setup",
        "--distributed-endpoint",
        endpoint,
        "--distributed-server-name",
        "localhost",
        "--distributed-capability",
        capability_id,
        "--distributed-authority",
        identity.authority_certificate.to_str().context("authority path")?,
        "--distributed-client-certificate",
        identity
            .client_certificate
            .to_str()
            .context("client certificate path")?,
        "--distributed-client-private-key",
        identity.client_private_key.to_str().context("client key path")?,
        "--distributed-policy",
        "qualification",
        "-f",
        "json",
    ];
    let preview = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(setup_arguments)
        .arg("--check")
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(
        preview.status.code() == Some(1),
        "mTLS setup preview failed: {preview:?}"
    );
    anyhow::ensure!(
        !cargo_home.path().join("cargo-rail/compiler-cache-v1").exists(),
        "mTLS setup preview mutated private state"
    );
    let setup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(setup_arguments)
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(setup.status.success(), "mTLS setup failed: {setup:?}");
    let setup: serde_json::Value = serde_json::from_slice(&setup.stdout)?;
    anyhow::ensure!(
        setup["distributed"] == "mutual_tls_direct_v1",
        "wrong installed mTLS mode"
    );
    anyhow::ensure!(
        setup["distributed_policy"] == "qualification",
        "wrong installed mTLS placement policy"
    );
    let installed = cargo_home.path().join("cargo-rail/compiler-cache-v1");
    let installed_key = installed.join("distributed-client.key");
    anyhow::ensure!(
        fs::metadata(&installed_key)?.permissions().mode() & 0o777 == 0o600,
        "installed client key is not private"
    );
    let rejected_local_replacement = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "setup", "--distributed-local"])
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(
        rejected_local_replacement.status.code() == Some(2),
        "mTLS-to-local replacement was not rejected: {rejected_local_replacement:?}"
    );
    anyhow::ensure!(
        String::from_utf8_lossy(&rejected_local_replacement.stderr)
            .contains("cannot replace an mTLS distributed installation"),
        "mTLS-to-local rejection was not actionable: {rejected_local_replacement:?}"
    );
    anyhow::ensure!(
        installed_key.is_file(),
        "rejected replacement removed the installed client key"
    );
    let original_client_key = fs::read(&identity.client_private_key)?;
    fs::write(&identity.client_private_key, b"source identity no longer valid")?;

    let build = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["build", "--release", "--lib", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
            .env(
                "RUSTFLAGS",
                format!("--remap-path-prefix={}={VIRTUAL_WORKSPACE}", workspace.path.display()),
            )
            .env("RUSTC", &rustc)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let built = build()?;
    anyhow::ensure!(
        built.status.success(),
        "ordinary Cargo mTLS environment seeding failed: {built:?}"
    );
    let seeded_events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        seeded_events.iter().any(|event| {
            event["action_key"].is_string()
                && event["status"] == "miss"
                && event["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("environment_selector_not_found;stored_verified_result"))
        }),
        "first-seen compiler environment did not seed locally: {seeded_events:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
    let cache_roots = fs::read_dir(cargo_home.path().join("cargo-rail"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("local-cas-v2"))
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    let [cache_root] = cache_roots.as_slice() else {
        anyhow::bail!("mTLS test cache did not contain one local CAS root: {cache_roots:?}");
    };
    for entry in fs::read_dir(cache_root.path().join("native-actions-v2"))? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let built = build()?;
    anyhow::ensure!(
        built.status.success(),
        "ordinary Cargo mTLS execution failed: {built:?}"
    );
    let events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    let distributed_hit = events
        .iter()
        .find(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution")
        .with_context(|| format!("ordinary Cargo did not commit a mutually authenticated worker result: {events:?}"))?;
    assert_distributed_phase_timing(distributed_hit)?;

    let clean_target = Command::new("cargo")
        .current_dir(&workspace.path)
        .arg("clean")
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(
        clean_target.status.success(),
        "Cargo target cleanup failed: {clean_target:?}"
    );
    let check_coverage = tempfile::tempdir()?;
    fs::set_permissions(check_coverage.path(), fs::Permissions::from_mode(0o700))?;
    let check_coverage = fs::canonicalize(check_coverage.path())?;
    let check = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["check", "--release", "--lib", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &check_coverage)
            .env(
                "RUSTFLAGS",
                format!("--remap-path-prefix={}={VIRTUAL_WORKSPACE}", workspace.path.display()),
            )
            .env("RUSTC", &rustc)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let checked = check()?;
    anyhow::ensure!(
        checked.status.success(),
        "ordinary Cargo metadata-only environment seeding failed: {checked:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
    for entry in fs::read_dir(cache_root.path().join("native-actions-v2"))? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&check_coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let checked = check()?;
    anyhow::ensure!(
        checked.status.success(),
        "ordinary Cargo metadata-only mTLS execution failed: {checked:?}"
    );
    let check_events = fs::read_dir(&check_coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    let metadata_hit = check_events
        .iter()
        .find(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution")
        .with_context(|| format!("ordinary Cargo did not commit a metadata-only worker result: {check_events:?}"))?;
    assert_distributed_phase_timing(metadata_hit)?;
    let check_artifacts = fs::read_dir(workspace.path.join("target/release/deps"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    anyhow::ensure!(
        check_artifacts
            .iter()
            .any(|path| path.extension().is_some_and(|extension| extension == "rmeta"))
            && check_artifacts
                .iter()
                .all(|path| path.extension().is_none_or(|extension| extension != "rlib")),
        "metadata-only execution produced an rlib or omitted metadata: {check_artifacts:?}"
    );

    let automatic = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args([
            "rail",
            "cache",
            "setup",
            "--distributed-policy",
            "automatic",
            "-f",
            "json",
        ])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(
        automatic.status.success(),
        "automatic placement setup failed: {automatic:?}"
    );
    let automatic: serde_json::Value = serde_json::from_slice(&automatic.stdout)?;
    anyhow::ensure!(
        automatic["distributed_policy"] == "automatic",
        "automatic placement was not installed"
    );
    let clean = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "clean", "--scope", "local"])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(clean.status.success(), "test cache cleanup failed: {clean:?}");
    let reinitialize = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "setup", "--distributed-policy", "automatic"])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(
        reinitialize.status.success(),
        "automatic placement cache reinitialization failed: {reinitialize:?}"
    );
    if workspace.path.join("target").exists() {
        fs::remove_dir_all(workspace.path.join("target"))?;
    }
    let automatic_coverage = tempfile::tempdir()?;
    fs::set_permissions(automatic_coverage.path(), fs::Permissions::from_mode(0o700))?;
    let automatic_coverage = fs::canonicalize(automatic_coverage.path())?;
    let automatic_build = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["build", "--release", "--lib", "--message-format=json"])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &automatic_coverage)
            .env("RUSTC", &rustc)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let automatic_seed = automatic_build()?;
    anyhow::ensure!(
        automatic_seed.status.success(),
        "automatic environment seeding failed: {automatic_seed:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
    let automatic_cache_roots = fs::read_dir(cargo_home.path().join("cargo-rail"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("local-cas-v2"))
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    let [automatic_cache_root] = automatic_cache_roots.as_slice() else {
        anyhow::bail!("automatic test cache did not contain one local CAS root: {automatic_cache_roots:?}");
    };
    for entry in fs::read_dir(automatic_cache_root.path().join("native-actions-v2"))? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&automatic_coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let automatic_build = automatic_build()?;
    anyhow::ensure!(
        automatic_build.status.success(),
        "automatic local placement failed: {automatic_build:?}"
    );
    let automatic_events = fs::read_dir(&automatic_coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        automatic_events.iter().any(|event| {
            event["status"] == "miss"
                && event["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("distributed_cost_history_"))
        }),
        "automatic placement delegated without enough history: {automatic_events:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&automatic_build.stdout),
        String::from_utf8_lossy(&automatic_build.stderr)
    );
    anyhow::ensure!(
        automatic_events
            .iter()
            .all(|event| event["reason"] != "verified_distributed_execution"),
        "automatic placement ignored its conservative cost gate: {automatic_events:?}"
    );
    anyhow::ensure!(
        installed.join("distributed-placement-v1.json").is_file(),
        "automatic local execution did not retain placement history"
    );
    let placement_status = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "status", "--scope", "local", "-f", "json"])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(placement_status.status.success(), "placement status failed");
    let placement_status: serde_json::Value = serde_json::from_slice(&placement_status.stdout)?;
    let installation = &placement_status["status"]["installation"];
    anyhow::ensure!(installation["distributed_policy"] == "automatic");
    anyhow::ensure!(installation["distributed_placement_history"]["state"] == "ready");
    anyhow::ensure!(installation["distributed_placement_history"]["local_observations"] == 4);
    anyhow::ensure!(installation["distributed_placement_history"]["remote_observations"] == 2);

    let installed_key_bytes = fs::read(&installed_key)?;
    fs::write(&installed_key, b"drifted installed identity")?;
    let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "status", "--scope", "local", "-f", "json"])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    anyhow::ensure!(
        status["status"]["installation"]["state"] == "drifted",
        "key drift was not reported"
    );
    fs::write(&installed_key, installed_key_bytes)?;
    fs::write(&identity.client_private_key, original_client_key)?;
    let repair = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(setup_arguments)
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(repair.status.success(), "mTLS identity repair failed: {repair:?}");
    let remove = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args(["rail", "cache", "remove"])
        .env("CARGO_HOME", cargo_home.path())
        .output()?;
    anyhow::ensure!(remove.status.success(), "mTLS installation removal failed: {remove:?}");
    anyhow::ensure!(!installed.exists(), "removal retained mTLS installation state");
    server.stop();
    Ok(())
}

#[cfg(unix)]
#[test]
fn saturated_mutual_tls_worker_falls_excess_cargo_actions_back_locally() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let worker = Path::new(env!("CARGO_BIN_EXE_cargo-rail-distributed-worker"));
    let rustc = which_rustc()?;
    let identity = generate_mutual_tls_identity()?;
    let mut server = Command::new(worker)
        .arg("serve-mtls")
        .arg(&rustc)
        .arg("127.0.0.1:0")
        .arg(&identity.server_certificate)
        .arg(&identity.server_private_key)
        .arg(&identity.authority_certificate)
        .arg("1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = server.stdout.take().context("worker server stdout")?;
    let mut startup = String::new();
    BufReader::new(stdout).read_line(&mut startup)?;
    let startup: serde_json::Value = serde_json::from_str(startup.trim_end())?;
    anyhow::ensure!(startup["max_concurrency"] == 1, "worker did not bind its test capacity");
    let endpoint = startup["address"].as_str().context("worker server address")?;
    let capability_id = startup["capability_id"]
        .as_str()
        .context("worker capability identity")?;
    let mut server = WorkerServer(server);

    let workspace = crate::helpers::TestWorkspace::new()?;
    for index in 1..=4 {
        let name = format!("parallel-member-{index}");
        let member = workspace.add_crate(&name, "0.1.0", &[])?;
        fs::write(member.join("src/lib.rs"), parallel_library_source(index, 512))?;
    }
    workspace.commit("add independent parallel libraries")?;
    let cargo_home = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    let coverage = tempfile::tempdir()?;
    fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
    let coverage = fs::canonicalize(coverage.path())?;

    let setup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
        .current_dir(&workspace.path)
        .args([
            "rail",
            "cache",
            "setup",
            "--local-dir",
            cache.path().to_str().context("cache path")?,
            "--distributed-endpoint",
            endpoint,
            "--distributed-server-name",
            "localhost",
            "--distributed-capability",
            capability_id,
            "--distributed-authority",
            identity.authority_certificate.to_str().context("authority path")?,
            "--distributed-client-certificate",
            identity
                .client_certificate
                .to_str()
                .context("client certificate path")?,
            "--distributed-client-private-key",
            identity.client_private_key.to_str().context("client key path")?,
            "--distributed-policy",
            "qualification",
        ])
        .env("CARGO_HOME", cargo_home.path())
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    anyhow::ensure!(setup.status.success(), "parallel mTLS setup failed: {setup:?}");

    let build = || {
        Command::new("cargo")
            .current_dir(&workspace.path)
            .args([
                "build",
                "--workspace",
                "--release",
                "--jobs",
                "4",
                "--message-format=json",
            ])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage)
            .env(
                "RUSTFLAGS",
                format!("--remap-path-prefix={}={VIRTUAL_WORKSPACE}", workspace.path.display()),
            )
            .env("RUSTC", &rustc)
            .env_remove("OUT_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
    };
    let seeded = build()?;
    anyhow::ensure!(
        seeded.status.success(),
        "parallel environment seeding failed: {seeded:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
    let cache_roots = fs::read_dir(cache.path().join("cargo-rail"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("local-cas-v2"))
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    let [cache_root] = cache_roots.as_slice() else {
        anyhow::bail!("parallel test cache did not contain one local CAS root: {cache_roots:?}");
    };
    for entry in fs::read_dir(cache_root.path().join("native-actions-v2"))? {
        fs::remove_file(entry?.path())?;
    }
    for entry in fs::read_dir(&coverage)? {
        fs::remove_file(entry?.path())?;
    }
    let built = build()?;
    anyhow::ensure!(
        built.status.success(),
        "parallel Cargo execution failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let events = fs::read_dir(&coverage)?
        .map(|entry| Ok(serde_json::from_slice::<serde_json::Value>(&fs::read(entry?.path())?)?))
        .collect::<Result<Vec<_>>>()?;
    let distributed = events
        .iter()
        .filter(|event| event["status"] == "hit" && event["reason"] == "verified_distributed_execution")
        .count();
    let local_fallbacks = events
        .iter()
        .filter(|event| {
            event["status"] == "miss"
                && event["reason"].as_str().is_some_and(|reason| {
                    reason.starts_with("distributed_transport_unavailable;stored_verified_result")
                })
        })
        .count();
    anyhow::ensure!(
        distributed >= 1 && local_fallbacks >= 1 && distributed + local_fallbacks == 4,
        "bounded worker capacity did not split exact work between remote execution and local fallback: {events:?}"
    );

    server.stop();
    Ok(())
}

#[cfg(unix)]
#[test]
fn mutual_tls_worker_drains_accepted_connections_before_stopping() -> Result<()> {
    let worker = Path::new(env!("CARGO_BIN_EXE_cargo-rail-distributed-worker"));
    let rustc = which_rustc()?;
    let identity = generate_mutual_tls_identity()?;
    let mut server = Command::new(worker)
        .arg("serve-mtls")
        .arg(&rustc)
        .arg("127.0.0.1:0")
        .arg(&identity.server_certificate)
        .arg(&identity.server_private_key)
        .arg(&identity.authority_certificate)
        .arg("1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = server.stdout.take().context("worker server stdout")?;
    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    stdout.read_line(&mut line)?;
    let ready: serde_json::Value = serde_json::from_str(line.trim_end())?;
    anyhow::ensure!(ready["event"] == "worker_ready", "worker emitted no ready event");
    let endpoint = ready["address"].as_str().context("worker server address")?;

    let accepted = mutually_authenticated_idle_connection(endpoint, &identity)?;
    let signaled = Command::new("/bin/kill")
        .args(["-TERM", &server.id().to_string()])
        .output()?;
    anyhow::ensure!(signaled.status.success(), "failed to signal the worker: {signaled:?}");
    line.clear();
    stdout.read_line(&mut line)?;
    let draining: serde_json::Value = serde_json::from_str(line.trim_end())?;
    anyhow::ensure!(
        draining["event"] == "worker_draining" && draining["active_connections"] == 1,
        "worker did not retain its accepted connection while draining: {draining}"
    );
    anyhow::ensure!(
        std::net::TcpStream::connect(endpoint).is_err(),
        "draining worker accepted a new connection"
    );
    drop(accepted);
    line.clear();
    stdout.read_line(&mut line)?;
    let stopped: serde_json::Value = serde_json::from_str(line.trim_end())?;
    anyhow::ensure!(
        stopped["event"] == "worker_stopped" && stopped["active_connections"] == 0,
        "worker stopped without an idle connection set: {stopped}"
    );
    let status = server.wait()?;
    anyhow::ensure!(status.success(), "drained worker failed: {status}");
    Ok(())
}

#[cfg(unix)]
fn parallel_library_source(salt: usize, functions: usize) -> String {
    let mut source = String::from("#![forbid(unsafe_code)]\n");
    for index in 1..=functions {
        source.push_str(&format!(
      "pub fn value_{index}(mut value: u64) -> u64 {{\n  for step in 0..32 {{\n    value = value.wrapping_mul(0x9e3779b97f4a7c15).rotate_left(step) ^ {salt};\n  }}\n  value\n}}\n"
    ));
    }
    source
}

#[cfg(unix)]
struct WorkerServer(Child);

#[cfg(unix)]
impl WorkerServer {
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            drop(
                Command::new("/bin/kill")
                    .args(["-TERM", &self.0.id().to_string()])
                    .status(),
            );
            for _ in 0..300 {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            drop(self.0.kill());
        }
        drop(self.0.wait());
    }
}

#[cfg(unix)]
impl Drop for WorkerServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
struct MutualTlsIdentity {
    _directory: tempfile::TempDir,
    authority_certificate: PathBuf,
    server_certificate: PathBuf,
    server_private_key: PathBuf,
    client_certificate: PathBuf,
    client_private_key: PathBuf,
}

#[cfg(unix)]
fn mutually_authenticated_idle_connection(
    endpoint: &str,
    identity: &MutualTlsIdentity,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>> {
    use rustls::pki_types::pem::PemObject as _;

    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let authority = fs::read(&identity.authority_certificate)?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&authority) {
        roots.add(certificate?)?;
    }
    let client_certificate = fs::read(&identity.client_certificate)?;
    let client_chain = rustls::pki_types::CertificateDer::pem_slice_iter(&client_certificate)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let client_private_key = fs::read(&identity.client_private_key)?;
    let client_private_key = rustls::pki_types::PrivateKeyDer::from_pem_slice(&client_private_key)?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_client_auth_cert(client_chain, client_private_key)?;
    config.alpn_protocols = vec![b"cargo-rail-execution/3".to_vec()];
    let socket = std::net::TcpStream::connect(endpoint)?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let connection = rustls::ClientConnection::new(
        std::sync::Arc::new(config),
        rustls::pki_types::ServerName::try_from("localhost")?,
    )?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    let mut magic = [0_u8; 8];
    stream.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == CAPABILITY_MAGIC, "worker emitted the wrong capability frame");
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    anyhow::ensure!(
        (1..=64 * 1024).contains(&length),
        "worker capability frame was unbounded"
    );
    let mut capability = vec![0_u8; length];
    stream.read_exact(&mut capability)?;
    stream.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == CAPABILITY_TRAILER, "worker capability frame was incomplete");
    Ok(stream)
}

#[cfg(unix)]
/// Prove the committed distributed hit retained a complete, source-free phase
/// breakdown whose measured sub-phases decompose the client critical path.
fn assert_distributed_phase_timing(event: &serde_json::Value) -> Result<()> {
    const CRITICAL_PATH: [&str; 9] = [
        "capability_capture",
        "connect",
        "tls_setup",
        "handshake",
        "capability_exchange",
        "lease",
        "source_transfer",
        "remote_execution",
        "result_transfer",
    ];

    let timing = event["distributed_timing"]
        .as_object()
        .with_context(|| format!("distributed hit retained no phase timing: {event}"))?;
    let phase = |name: &str| -> Result<(u64, u64)> {
        let value = timing
            .get(name)
            .and_then(serde_json::Value::as_object)
            .with_context(|| format!("distributed phase timing lost {name}: {event}"))?;
        let count = value["count"].as_u64().context("phase count is not a number")?;
        let elapsed = value["elapsed_ns"].as_u64().context("phase elapsed is not a number")?;
        anyhow::ensure!(
            count == 1,
            "distributed phase {name} was not measured exactly once: {count}"
        );
        anyhow::ensure!(elapsed > 0, "distributed phase {name} measured no elapsed time");
        Ok((count, elapsed))
    };

    let (_, attempt) = phase("attempt")?;
    phase("admission")?;
    let mut measured = 0_u64;
    for name in CRITICAL_PATH {
        measured = measured.saturating_add(phase(name)?.1);
    }
    anyhow::ensure!(
        attempt >= measured,
        "distributed sub-phases {measured}ns exceeded the whole client attempt {attempt}ns"
    );
    let worker = timing["worker"]
        .as_object()
        .with_context(|| format!("distributed phase timing lost worker telemetry: {event}"))?;
    let worker_phase = |name: &str| -> Result<u64> {
        worker[name]
            .as_u64()
            .with_context(|| format!("distributed worker timing lost {name}: {event}"))
    };
    let worker_elapsed = worker_phase("elapsed_ns")?;
    let worker_measured = worker_phase("input_ns")?
        .saturating_add(worker_phase("compiler_ns")?)
        .saturating_add(worker_phase("result_encode_ns")?);
    anyhow::ensure!(
        worker_elapsed >= worker_measured && phase("remote_execution")?.1 >= worker_elapsed,
        "distributed worker phases do not decompose the remote execution interval: {event}"
    );
    anyhow::ensure!(
        timing["source_bytes"].as_u64().is_some_and(|bytes| bytes > 0)
            && timing["result_bytes"].as_u64().is_some_and(|bytes| bytes > 0),
        "distributed phase timing lost its transfer sizes: {event}"
    );
    anyhow::ensure!(
        worker_phase("source_bytes")?
            == timing["source_bytes"]
                .as_u64()
                .context("distributed source bytes are not numeric")?
            && worker_phase("result_bytes")?
                == timing["result_bytes"]
                    .as_u64()
                    .context("distributed result bytes are not numeric")?,
        "distributed worker and client transfer sizes disagree: {event}"
    );
    anyhow::ensure!(
        timing.values().all(|value| value.is_u64()
            || value
                .as_object()
                .is_some_and(|phase| phase.values().all(serde_json::Value::is_u64))),
        "distributed phase timing is not source-free: {event}"
    );
    Ok(())
}

fn generate_mutual_tls_identity() -> Result<MutualTlsIdentity> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let authority_certificate = directory.path().join("authority.pem");
    let authority_key = directory.path().join("authority.key");
    let server_certificate = directory.path().join("server.pem");
    let server_private_key = directory.path().join("server.key");
    let server_request = directory.path().join("server.csr");
    let server_extensions = directory.path().join("server.ext");
    let client_certificate = directory.path().join("client.pem");
    let client_private_key = directory.path().join("client.key");
    let client_request = directory.path().join("client.csr");
    let client_extensions = directory.path().join("client.ext");
    fs::write(
        &server_extensions,
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
    )?;
    fs::write(&client_extensions, "extendedKeyUsage=clientAuth\n")?;
    run_openssl([
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-sha256",
        "-nodes",
        "-days",
        "1",
        "-subj",
        "/CN=cargo-rail-test-authority",
        "-keyout",
        authority_key.to_str().context("authority key")?,
        "-out",
        authority_certificate.to_str().context("authority certificate")?,
    ])?;
    run_openssl([
        "req",
        "-newkey",
        "rsa:2048",
        "-sha256",
        "-nodes",
        "-subj",
        "/CN=localhost",
        "-keyout",
        server_private_key.to_str().context("server key")?,
        "-out",
        server_request.to_str().context("server request")?,
    ])?;
    run_openssl([
        "x509",
        "-req",
        "-sha256",
        "-days",
        "1",
        "-in",
        server_request.to_str().context("server request")?,
        "-CA",
        authority_certificate.to_str().context("authority certificate")?,
        "-CAkey",
        authority_key.to_str().context("authority key")?,
        "-set_serial",
        "1",
        "-extfile",
        server_extensions.to_str().context("server extensions")?,
        "-out",
        server_certificate.to_str().context("server certificate")?,
    ])?;
    run_openssl([
        "req",
        "-newkey",
        "rsa:2048",
        "-sha256",
        "-nodes",
        "-subj",
        "/CN=cargo-rail-test-client",
        "-keyout",
        client_private_key.to_str().context("client key")?,
        "-out",
        client_request.to_str().context("client request")?,
    ])?;
    run_openssl([
        "x509",
        "-req",
        "-sha256",
        "-days",
        "1",
        "-in",
        client_request.to_str().context("client request")?,
        "-CA",
        authority_certificate.to_str().context("authority certificate")?,
        "-CAkey",
        authority_key.to_str().context("authority key")?,
        "-set_serial",
        "2",
        "-extfile",
        client_extensions.to_str().context("client extensions")?,
        "-out",
        client_certificate.to_str().context("client certificate")?,
    ])?;
    for path in [&authority_key, &server_private_key, &client_private_key] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(MutualTlsIdentity {
        _directory: directory,
        authority_certificate,
        server_certificate,
        server_private_key,
        client_certificate,
        client_private_key,
    })
}

#[cfg(unix)]
fn run_openssl<const N: usize>(arguments: [&str; N]) -> Result<()> {
    let output = Command::new("openssl").args(arguments).output()?;
    anyhow::ensure!(
        output.status.success(),
        "openssl identity generation failed: {output:?}"
    );
    Ok(())
}

fn which_rustc() -> Result<PathBuf> {
    let output = Command::new("rustup").args(["which", "rustc"]).output()?;
    anyhow::ensure!(output.status.success(), "rustup which rustc failed: {output:?}");
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ContentDigest::sha256(bytes))
}

fn capability_identity(capability: &WorkerCapability) -> Result<String> {
    let encoded = serde_json::to_vec(&(
        &capability.architecture,
        &capability.endianness,
        &capability.environment_contract,
        &capability.filesystem_contract,
        &capability.host_target,
        &capability.isolation,
        &capability.isolation_identity,
        &capability.operating_system,
        &capability.operation_classes,
        &capability.platform_family,
        capability.protocol_version,
        capability.resource_limits,
        &capability.rustc_content_digest,
        &capability.rustc_verbose_version,
        &capability.sysroot_identity,
        &capability.working_directory_contract,
    ))?;
    Ok(format!(
        "worker-capability-v3:sha256:{}",
        ContentDigest::sha256(&encoded)
    ))
}

fn execution_request(capability: &WorkerCapability, source: &[u8]) -> Result<ExecutionRequest> {
    execution_request_with_emission(capability, source, "metadata_and_link")
}

fn execution_request_with_emission(
    capability: &WorkerCapability,
    source: &[u8],
    emission: &str,
) -> Result<ExecutionRequest> {
    let limits = capability.resource_limits;
    let operation = RustLibraryOperation {
        cap_lints: None,
        cargo_json_diagnostics: true,
        check_cfg: vec!["cfg(docsrs,test)".to_string(), "cfg(feature, values())".to_string()],
        codegen: RustLibraryCodegen {
            codegen_units: None,
            debuginfo: None,
            debug_assertions: None,
            embed_bitcode: Some(false),
            linker_plugin_lto: None,
            lto: None,
            opt_level: Some("3".to_string()),
            overflow_checks: None,
            panic: None,
            prefer_dynamic: None,
            split_debuginfo: None,
            strip: Some("debuginfo".to_string()),
        },
        color: None,
        crate_name: "distributed_fixture".to_string(),
        crate_type: "rlib".to_string(),
        cfg: Vec::new(),
        dependencies: Vec::new(),
        diagnostic_width: None,
        dep_info_name: "distributed_fixture-0123456789abcdef.d".to_string(),
        edition: "2024".to_string(),
        emission: emission.to_string(),
        extra_filename: "-0123456789abcdef".to_string(),
        lints: Vec::new(),
        metadata: "0123456789abcdef".to_string(),
        metadata_name: "libdistributed_fixture-0123456789abcdef.rmeta".to_string(),
        operation_class: "rust_library".to_string(),
        output_relative_directory: "target/release/deps".to_string(),
        output_dependency_search: true,
        rlib_name: (emission == "metadata_and_link")
            .then(|| "libdistributed_fixture-0123456789abcdef.rlib".to_string()),
        source_virtual_path: format!("{VIRTUAL_WORKSPACE}/src/lib.rs"),
        test_mode: false,
        toolchain_proc_macro: false,
    };
    let inputs = vec![InputFrame {
        bytes: source.len() as u64,
        content_digest: digest(source),
        kind: "source".to_string(),
        virtual_path: format!("{VIRTUAL_WORKSPACE}/src/lib.rs"),
    }];
    let action = serde_json::to_vec(&(&capability.capability_id, &inputs, limits, &operation, 3_u32))?;
    Ok(ExecutionRequest {
        action_id: format!("execution-action-v3:sha256:{}", ContentDigest::sha256(&action)),
        capability_id: capability.capability_id.clone(),
        inputs,
        lease_id: format!("execution-lease-v3:sha256:{}", "a".repeat(64)),
        limits,
        operation,
        protocol_version: 3,
        workload_identity: format!("workload-v1:sha256:{}", "b".repeat(64)),
    })
}

fn run_worker(
    worker: &Path,
    rustc: &Path,
    request: &ExecutionRequest,
    source: &[u8],
    cancel: bool,
) -> Result<DecodedResponse> {
    let mut child = Command::new(worker)
        .arg("execute")
        .arg(rustc)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("worker stdin")?;
    let mut stdout = child.stdout.take().context("worker stdout")?;
    let mut stderr = child.stderr.take().context("worker stderr")?;
    write_request(&mut stdin, request, source)?;
    if cancel {
        stdin.write_all(CANCEL_MAGIC)?;
        let lease = request.lease_id.as_bytes();
        stdin.write_all(&u32::try_from(lease.len())?.to_le_bytes())?;
        stdin.write_all(lease)?;
        stdin.write_all(CANCEL_TRAILER)?;
        stdin.flush()?;
    }

    // Keep the request pipe open while reading. EOF is a client-loss signal, not
    // a normal half-close in this protocol.
    let mut response = Vec::new();
    stdout.read_to_end(&mut response)?;
    drop(stdin);
    let mut worker_stderr = Vec::new();
    stderr.read_to_end(&mut worker_stderr)?;
    let status = child.wait()?;
    anyhow::ensure!(
        status.success(),
        "worker failed: status={status}, stderr={}",
        String::from_utf8_lossy(&worker_stderr)
    );
    decode_response(&response)
}

fn write_request(writer: &mut impl Write, request: &ExecutionRequest, source: &[u8]) -> Result<()> {
    let header = serde_json::to_vec(request)?;
    writer.write_all(REQUEST_MAGIC)?;
    writer.write_all(&u32::try_from(header.len())?.to_le_bytes())?;
    writer.write_all(&header)?;
    let [input] = request.inputs.as_slice() else {
        anyhow::bail!("direct protocol request did not contain one input");
    };
    anyhow::ensure!(input.bytes == source.len() as u64 && input.content_digest == digest(source));
    writer.write_all(source)?;
    writer.write_all(REQUEST_TRAILER)?;
    writer.flush()?;
    Ok(())
}

fn decode_response(encoded: &[u8]) -> Result<DecodedResponse> {
    let mut reader = std::io::Cursor::new(encoded);
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == RESPONSE_MAGIC, "invalid response magic");
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let mut header = vec![0_u8; u32::from_le_bytes(length) as usize];
    reader.read_exact(&mut header)?;
    let header: ExecutionResponse = serde_json::from_slice(&header)?;
    let mut frames = BTreeMap::new();
    for frame in &header.frames {
        let mut payload = vec![0_u8; usize::try_from(frame.bytes)?];
        reader.read_exact(&mut payload)?;
        anyhow::ensure!(
            frames.insert(frame.slot.clone(), payload).is_none(),
            "duplicate response slot"
        );
    }
    reader.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == RESPONSE_TRAILER, "invalid response trailer");
    anyhow::ensure!(reader.position() == encoded.len() as u64, "trailing response bytes");
    Ok(DecodedResponse { header, frames })
}

fn assert_success_authority(response: &ExecutionResponse, request: &ExecutionRequest) -> Result<()> {
    anyhow::ensure!(
        response.status == "success",
        "worker rejected execution: {:?}",
        response.reason
    );
    anyhow::ensure!(
        response.reason.is_none(),
        "successful response carried a rejection reason"
    );
    anyhow::ensure!(
        response.termination == Some(CompilerTermination::Exit { code: 0 }),
        "successful response carried a failing termination"
    );
    anyhow::ensure!(
        response.protocol_version == request.protocol_version,
        "protocol authority changed"
    );
    anyhow::ensure!(response.action_id == request.action_id, "action authority changed");
    anyhow::ensure!(
        response.capability_id == request.capability_id,
        "capability authority changed"
    );
    anyhow::ensure!(response.lease_id == request.lease_id, "lease authority changed");
    anyhow::ensure!(
        response.workload_identity == request.workload_identity,
        "workload authority changed"
    );
    let slots = response
        .frames
        .iter()
        .map(|frame| frame.slot.as_str())
        .collect::<Vec<_>>();
    let expected = if request.operation.emission == "metadata" {
        &["dep_info", "metadata", "stderr", "stdout"][..]
    } else {
        &["dep_info", "metadata", "rlib", "stderr", "stdout"][..]
    };
    anyhow::ensure!(slots == expected, "slot order changed");
    Ok(())
}

fn assert_frame_digests(response: &DecodedResponse) -> Result<()> {
    for descriptor in &response.header.frames {
        let payload = response.frames.get(&descriptor.slot).context("response payload")?;
        anyhow::ensure!(
            descriptor.content_digest == digest(payload),
            "{} digest mismatch",
            descriptor.slot
        );
        let expected_mode = if matches!(descriptor.slot.as_str(), "stderr" | "stdout") {
            0
        } else {
            0o644
        };
        anyhow::ensure!(descriptor.mode == expected_mode, "{} mode mismatch", descriptor.slot);
    }
    Ok(())
}

fn compile_locally(
    rustc: &Path,
    operation: &RustLibraryOperation,
    source_bytes: &[u8],
) -> Result<BTreeMap<String, Vec<u8>>> {
    let root = tempfile::tempdir()?;
    let root_path = fs::canonicalize(root.path())?;
    let workspace_directory = root_path.join("workspace");
    let output_directory = workspace_directory.join(&operation.output_relative_directory);
    let temporary_directory = root.path().join("tmp");
    fs::create_dir(&workspace_directory)?;
    fs::create_dir_all(&output_directory)?;
    fs::create_dir(&temporary_directory)?;
    let source_directory = workspace_directory.join("src");
    fs::create_dir(&source_directory)?;
    let source = source_directory.join("lib.rs");
    fs::write(&source, source_bytes)?;
    let dep_info = output_directory.join(format!("{}{}.d", operation.crate_name, operation.extra_filename));
    let metadata = output_directory.join(format!("lib{}{}.rmeta", operation.crate_name, operation.extra_filename));
    let rlib = output_directory.join(format!("lib{}{}.rlib", operation.crate_name, operation.extra_filename));
    let mut command = Command::new(rustc);
    command.arg("src/lib.rs").args([
        "--crate-name",
        &operation.crate_name,
        "--crate-type",
        &operation.crate_type,
        "--edition",
        &operation.edition,
    ]);
    if operation.cargo_json_diagnostics {
        command
            .arg("--error-format=json")
            .arg("--json=diagnostic-rendered-ansi,artifacts,future-incompat");
    }
    let emit = if operation.emission == "metadata" {
        format!("dep-info={},metadata={}", dep_info.display(), metadata.display())
    } else {
        format!(
            "dep-info={},metadata={},link={}",
            dep_info.display(),
            metadata.display(),
            rlib.display()
        )
    };
    command.arg("--out-dir").arg(&output_directory).arg("--emit").arg(emit);
    if let Some(opt_level) = &operation.codegen.opt_level {
        command.arg(format!("-Copt-level={opt_level}"));
    }
    if let Some(embed_bitcode) = operation.codegen.embed_bitcode {
        command.arg(format!("-Cembed-bitcode={}", if embed_bitcode { "yes" } else { "no" }));
    }
    if let Some(debuginfo) = &operation.codegen.debuginfo {
        command.arg(format!("-Cdebuginfo={debuginfo}"));
    }
    if let Some(split_debuginfo) = &operation.codegen.split_debuginfo {
        command.arg(format!("-Csplit-debuginfo={split_debuginfo}"));
    }
    if let Some(strip) = &operation.codegen.strip {
        command.arg(format!("-Cstrip={strip}"));
    }
    for check_cfg in &operation.check_cfg {
        command.arg("--check-cfg").arg(check_cfg);
    }
    command
        .arg(format!("-Cmetadata={}", operation.metadata))
        .arg(format!("-Cextra-filename={}", operation.extra_filename));
    if operation.output_dependency_search {
        command
            .arg("-L")
            .arg(format!("dependency={}", output_directory.display()));
    }
    command
        .arg("--remap-path-prefix")
        .arg(format!("{}={VIRTUAL_WORKSPACE}", workspace_directory.display()))
        .current_dir(&workspace_directory)
        .stdin(Stdio::null())
        .env_clear()
        .env("TEMP", &temporary_directory)
        .env("TMP", &temporary_directory)
        .env("TMPDIR", &temporary_directory);
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").context("SystemRoot")?);
    let output = command.output()?;
    anyhow::ensure!(output.status.success(), "local rustc failed: {output:?}");

    let dep_info_bytes = fs::read(dep_info)?;
    let dep_info_bytes = replace_bytes(
        &dep_info_bytes,
        root_path.as_os_str().as_encoded_bytes(),
        VIRTUAL_ROOT.as_bytes(),
    );
    let stdout = replace_bytes(
        &output.stdout,
        root_path.as_os_str().as_encoded_bytes(),
        VIRTUAL_ROOT.as_bytes(),
    );
    let stderr = replace_bytes(
        &output.stderr,
        root_path.as_os_str().as_encoded_bytes(),
        VIRTUAL_ROOT.as_bytes(),
    );
    let mut outputs = BTreeMap::from([
        ("dep_info".to_string(), dep_info_bytes),
        ("metadata".to_string(), fs::read(metadata)?),
        ("stderr".to_string(), stderr),
        ("stdout".to_string(), stdout),
    ]);
    if operation.emission == "metadata_and_link" {
        outputs.insert("rlib".to_string(), fs::read(rlib)?);
    }
    Ok(outputs)
}

fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut remaining = input;
    while let Some(index) = remaining.windows(needle.len()).position(|window| window == needle) {
        output.extend_from_slice(&remaining[..index]);
        output.extend_from_slice(replacement);
        remaining = &remaining[index + needle.len()..];
    }
    output.extend_from_slice(remaining);
    output
}
