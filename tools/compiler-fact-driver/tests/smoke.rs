//! Native end-to-end proof for the exact matched driver.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use sha2::{Digest as _, Sha256};
#[path = "../../../src/compiler/fact_protocol.rs"]
mod fact_protocol;

use fact_protocol::{
    COMPILER_FACT_ANNOUNCEMENT_CODE, COMPILER_FACT_ANNOUNCEMENT_PREFIX, COMPILER_FACT_INVOCATION_ENV,
    COMPILER_FACT_PROTOCOL_VERSION, CompilerFactCoverage, CompilerFactDomain, CompilerFactFragment,
    CompilerFactInvocation, CompilerFactPackage, CompilerFactProducerAuthority, CompilerFactRole,
    CompilerFactRunAuthority, CompilerFactTargetKind, CompilerFactUnit,
};

#[test]
fn matched_driver_emits_canonical_typed_fragment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let temporary = tempfile::tempdir().expect("temporary driver tree");
    let bin = temporary.path().join("bin");
    let output = temporary.path().join("facts");
    fs::create_dir_all(&bin).expect("driver bin directory");
    fs::create_dir_all(&output).expect("fact directory");

    let rustc = rustc_program();
    let sysroot_output = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("rustc sysroot");
    assert!(sysroot_output.status.success());
    let sysroot = PathBuf::from(String::from_utf8(sysroot_output.stdout).expect("UTF-8 sysroot").trim());
    #[cfg(unix)]
    symlink(sysroot.join("lib"), temporary.path().join("lib")).expect("toolchain library link");
    let staged = bin.join(format!("cargo-rail-fact-driver{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_cargo-rail-fact-driver"), &staged).expect("stage driver");

    let host = rustc_host(&rustc);
    let unit = bind_unit_identity(CompilerFactUnit {
        identity: String::new(),
        invocation_identity: identity("compiler-fact-invocation-v1-sha256-", '5'),
        package: CompilerFactPackage {
            name: "fact-probe".to_string(),
            version: "0.0.0".to_string(),
            source: None,
        },
        cargo_target: "fact-probe".to_string(),
        crate_name: "fact_probe".to_string(),
        target_kind: CompilerFactTargetKind::Library,
        domain: CompilerFactDomain::Production,
        role: CompilerFactRole::Target,
        platform: host,
        features: Vec::new(),
        cfg: Vec::new(),
    });
    let invocation = CompilerFactInvocation {
        version: COMPILER_FACT_PROTOCOL_VERSION,
        observation_directory: output.to_string_lossy().into_owned(),
        source_root: root.to_string_lossy().into_owned(),
        generated_roots: vec![temporary.path().join("cargo-target").to_string_lossy().into_owned()],
        run_authority: CompilerFactRunAuthority {
            run_identity: identity("compiler-fact-run-v1-sha256-", '1'),
            view_identity: identity("compiler-fact-view-v1-sha256-", '2'),
        },
        producer_authority: CompilerFactProducerAuthority {
            compiler_identity: identity("compiler-fact-compiler-v1-sha256-", '3'),
            driver_identity: identity("compiler-fact-driver-v1-sha256-", '4'),
        },
        unit,
        required_coverage: all_coverage(),
    };
    let capability = temporary.path().join("invocation.json");
    fs::write(&capability, serde_json::to_vec(&invocation).expect("encode invocation")).expect("write invocation");

    let mut command = Command::new("cargo");
    command
        .current_dir(root.join("tools/compiler-fact-driver/tests/fixtures/workspace"))
        .args(["check", "--locked", "--message-format=json"])
        .env("CARGO_TARGET_DIR", temporary.path().join("cargo-target"))
        .env("RUSTFLAGS", "--warn=unused-crate-dependencies")
        .env("RUSTC_WORKSPACE_WRAPPER", &staged)
        .env(COMPILER_FACT_INVOCATION_ENV, &capability);
    #[cfg(windows)]
    {
        let mut paths = vec![sysroot.join("bin"), sysroot.join("lib")];
        if let Some(inherited) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&inherited));
        }
        command.env("PATH", std::env::join_paths(paths).expect("compiler driver PATH"));
    }
    let result = command.output().expect("run Cargo through driver");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let stdout = String::from_utf8(result.stdout).expect("UTF-8 Cargo messages");
    assert!(
        stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|value| value["reason"] == "compiler-message"
                && value["message"]["code"]["code"] == "unused_crate_dependencies")
    );
    let diagnostic = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| {
            value["reason"] == "compiler-message" && value["message"]["code"]["code"] == COMPILER_FACT_ANNOUNCEMENT_CODE
        })
        .expect("Cargo fact announcement message");
    let message = diagnostic["message"]["message"].as_str().expect("announcement message");
    let announcement: fact_protocol::CompilerFactAnnouncement = serde_json::from_str(
        message
            .strip_prefix(COMPILER_FACT_ANNOUNCEMENT_PREFIX)
            .expect("announcement envelope"),
    )
    .expect("decode announcement");
    let digest = announcement
        .content_digest
        .strip_prefix("sha256:")
        .expect("content digest");
    let fragment_path = output.join(format!("compiler-fact-fragment-sha256-{digest}.json"));
    let bytes = fs::read(fragment_path).expect("fact fragment");
    assert_eq!(bytes.len() as u64, announcement.bytes);
    let fragment: CompilerFactFragment = serde_json::from_slice(&bytes).expect("decode fragment");
    assert_eq!(serde_json::to_vec(&fragment).expect("canonical fragment"), bytes);
    assert_eq!(fragment.object.unit.crate_name, "fact_probe");
    assert!(fragment.object.items.len() >= 4);
    assert!(
        fragment
            .object
            .edges
            .iter()
            .any(|edge| edge.kind == fact_protocol::CompilerFactEdgeKind::Body)
    );
    assert!(fragment.object.completion.complete);
    assert_eq!(fragment.object.completion.coverage, all_coverage());
}

fn rustc_program() -> PathBuf {
    let output = Command::new("rustup")
        .args(["which", "rustc"])
        .output()
        .expect("locate rustc");
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).expect("UTF-8 rustc path").trim())
}

fn rustc_host(rustc: &Path) -> String {
    let output = Command::new(rustc).arg("-vV").output().expect("rustc identity");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("UTF-8 rustc identity")
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc host")
        .to_string()
}

fn identity(prefix: &str, digit: char) -> String {
    format!("{prefix}{}", digit.to_string().repeat(64))
}

fn bind_unit_identity(mut unit: CompilerFactUnit) -> CompilerFactUnit {
    let bytes = serde_json::to_vec(&(
        &unit.invocation_identity,
        &unit.package,
        &unit.cargo_target,
        &unit.crate_name,
        &unit.target_kind,
        unit.domain,
        unit.role,
        &unit.platform,
        &unit.features,
        &unit.cfg,
    ))
    .expect("serialize unit identity");
    let digest = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    unit.identity = format!("compiler-fact-unit-v1-sha256-{digest}");
    unit
}

fn all_coverage() -> BTreeSet<CompilerFactCoverage> {
    BTreeSet::from([
        CompilerFactCoverage::Definitions,
        CompilerFactCoverage::Visibility,
        CompilerFactCoverage::ExactSpans,
        CompilerFactCoverage::MacroProvenance,
        CompilerFactCoverage::BodyEdges,
        CompilerFactCoverage::InterfaceEdges,
        CompilerFactCoverage::ReexportEdges,
        CompilerFactCoverage::PrivacyEdges,
        CompilerFactCoverage::TraitDispatch,
        CompilerFactCoverage::ForeignExports,
        CompilerFactCoverage::GeneratedSources,
        CompilerFactCoverage::EntryPoints,
        CompilerFactCoverage::ConservativeRetention,
    ])
}
