//! Native end-to-end proof for the exact matched driver.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use rscrypto::Sha256;
#[path = "../../../src/compiler/fact_protocol.rs"]
mod fact_protocol;

use fact_protocol::{
    COMPILER_FACT_ANNOUNCEMENT_CODE, COMPILER_FACT_ANNOUNCEMENT_PREFIX, COMPILER_FACT_INVOCATION_ENV,
    COMPILER_FACT_PROTOCOL_VERSION, CompilerFactCoverage, CompilerFactDomain, CompilerFactEdgeKind,
    CompilerFactFragment, CompilerFactInvocation, CompilerFactItemKind, CompilerFactMacroProvenance,
    CompilerFactNamespace, CompilerFactPackage, CompilerFactProducerAuthority, CompilerFactRole,
    CompilerFactRunAuthority, CompilerFactSourceIdentity, CompilerFactSourcePath, CompilerFactTargetKind,
    CompilerFactUnit, CompilerFactVisibility, CompilerItemFact,
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
    let first_target = temporary.path().join("cargo-target-a");
    let second_target = temporary.path().join("cargo-target-b");
    fs::create_dir_all(&bin).expect("driver bin directory");
    fs::create_dir_all(&output).expect("fact directory");
    fs::create_dir_all(&first_target).expect("first Cargo target directory");
    fs::create_dir_all(&second_target).expect("second Cargo target directory");

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
        generated_roots: vec![
            first_target.to_string_lossy().into_owned(),
            second_target.to_string_lossy().into_owned(),
        ],
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

    let acquire = |target: &Path| {
        let mut command = Command::new("cargo");
        command
            .current_dir(root.join("tools/compiler-fact-driver/tests/fixtures/workspace"))
            .args(["check", "--locked", "--message-format=json"])
            .env("CARGO_TARGET_DIR", target)
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
        assert!(
            result.status.success(),
            "Cargo fact acquisition failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8(result.stdout).expect("UTF-8 Cargo messages");
        let diagnostic = stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|value| {
                value["reason"] == "compiler-message"
                    && value["message"]["code"]["code"] == COMPILER_FACT_ANNOUNCEMENT_CODE
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
        (stdout, bytes, fragment)
    };

    let (stdout, first_bytes, fragment) = acquire(&first_target);
    assert!(
        stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|value| value["reason"] == "compiler-message"
                && value["message"]["code"]["code"] == "unused_crate_dependencies")
    );
    assert_eq!(serde_json::to_vec(&fragment).expect("canonical fragment"), first_bytes);
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

    assert_compiler_item_identities(&fragment);
    assert_task_local_expansion(&fragment);
    assert_std_thread_local_expansion(&fragment);

    let (_, second_bytes, second_fragment) = acquire(&second_target);
    assert_eq!(second_bytes, first_bytes);
    assert_eq!(second_fragment, fragment);
}

fn assert_task_local_expansion(fragment: &CompilerFactFragment) {
    // This is the release-readiness reproducer. Rustc may expose the generated
    // bytes directly or only its stable compiler-owned source identity.
    let task_items = items(fragment)
        .filter(|item| diagnostic_path(fragment, item).starts_with("TASK_CONTEXT"))
        .collect::<Vec<_>>();
    assert!(
        task_items.len() >= 5,
        "Tokio task-local expansion was not collected completely"
    );

    let task_context = task_items
        .iter()
        .find(|item| diagnostic_path(fragment, item) == "TASK_CONTEXT")
        .expect("source-visible Tokio task-local definition");
    let generated_key = task_items
        .iter()
        .find(|item| diagnostic_path(fragment, item) == "TASK_CONTEXT::__KEY")
        .expect("compiler-generated Tokio task-local key");
    assert!(matches!(
        fragment.object.sources[task_context.physical.span.source as usize].path,
        CompilerFactSourcePath::Repository(_)
    ));

    for item in task_items
        .iter()
        .filter(|item| diagnostic_path(fragment, item) != "TASK_CONTEXT")
    {
        let source = &fragment.object.sources[item.physical.span.source as usize];
        assert!(
            matches!(source.path, CompilerFactSourcePath::Generated(_)),
            "Tokio expansion '{}' must not invent repository coordinates",
            diagnostic_path(fragment, item)
        );
        assert!(source.bytes > 0, "Tokio expansion must retain a nonempty source extent");
        assert!(item.physical.span.start < item.physical.span.end);
        assert!(item.physical.span.end <= source.bytes);
        assert!(matches!(
            &source.identity,
            CompilerFactSourceIdentity::Exact(digest) | CompilerFactSourceIdentity::CompilerOwned(digest)
                if digest.starts_with("sha256:")
        ));
    }

    assert!(fragment.object.edges.iter().any(|edge| {
        edge.source == task_context.id && edge.target == generated_key.id && edge.kind == CompilerFactEdgeKind::Body
    }));
    let task_ids = task_items.iter().map(|item| item.id).collect::<HashSet<_>>();
    assert!(fragment.object.edges.iter().any(|edge| {
        edge.source == generated_key.id && task_ids.contains(&edge.target) && edge.kind == CompilerFactEdgeKind::Body
    }));
}

fn assert_std_thread_local_expansion(fragment: &CompilerFactFragment) {
    let generated = items(fragment)
        .find(|item| diagnostic_path(fragment, item).ends_with("STD_THREAD_LOCAL::__RUST_STD_INTERNAL_INIT"))
        .expect("std thread-local compiler-generated initializer");
    let source = &fragment.object.sources[generated.physical.span.source as usize];
    assert!(matches!(source.path, CompilerFactSourcePath::Generated(_)));
    assert!(matches!(
        source.identity,
        CompilerFactSourceIdentity::Exact(_) | CompilerFactSourceIdentity::CompilerOwned(_)
    ));
    assert!(generated.physical.span.start < generated.physical.span.end);
    assert!(generated.physical.span.end <= source.bytes);
    assert!(matches!(
        generated.macro_provenance,
        CompilerFactMacroProvenance::Expansion(_)
    ));
}

fn assert_compiler_item_identities(fragment: &CompilerFactFragment) {
    let item = |path: &str, kind: CompilerFactItemKind| {
        items(fragment)
            .find(|item| diagnostic_path(fragment, item) == path && item.physical.kind == kind)
            .unwrap_or_else(|| {
                panic!(
                    "missing {kind:?} compiler fact for {path}; collected: {:?}",
                    items(fragment)
                        .map(|item| (diagnostic_path(fragment, item), item.physical.kind))
                        .collect::<Vec<_>>()
                )
            })
    };

    let module = item("same_name", CompilerFactItemKind::Module);
    let function = item("same_name", CompilerFactItemKind::Function);
    assert_ne!(module.id, function.id);
    assert_eq!(module.id.0[0], function.id.0[0]);
    assert_ne!(module.id.0[1], function.id.0[1]);
    assert_eq!(module.physical.namespace, CompilerFactNamespace::Type);
    assert_eq!(function.physical.namespace, CompilerFactNamespace::Value);
    assert_eq!(module.parent, function.parent);
    assert_eq!(module.written_visibility, CompilerFactVisibility::Public);
    assert_eq!(function.written_visibility, CompilerFactVisibility::Public);

    let scope = item("scoped", CompilerFactItemKind::Module);
    let scoped_module = item("scoped::twin", CompilerFactItemKind::Module);
    let scoped_function = item("scoped::twin", CompilerFactItemKind::Function);
    assert_ne!(scoped_module.id, scoped_function.id);
    assert_eq!(scoped_module.parent, Some(scope.id));
    assert_eq!(scoped_function.parent, Some(scope.id));
    assert_eq!(
        scoped_module.written_visibility,
        CompilerFactVisibility::Restricted(scope.id)
    );
    assert_eq!(
        scoped_function.written_visibility,
        CompilerFactVisibility::Restricted(scope.id)
    );
    for source in [scoped_module.id, scoped_function.id] {
        assert!(fragment.object.edges.iter().any(|edge| {
            edge.source == source && edge.target == scope.id && edge.kind == CompilerFactEdgeKind::VisibilityParent
        }));
    }
    let nested = item("scoped::twin::nested", CompilerFactItemKind::Function);
    assert_eq!(nested.parent, Some(scoped_module.id));
    assert!(fragment.object.edges.iter().any(|edge| {
        edge.source == scoped_function.id && edge.target == nested.id && edge.kind == CompilerFactEdgeKind::Body
    }));

    let namespace_items = items(fragment)
        .filter(|item| diagnostic_path(fragment, item) == "namespace_coexistence::Shared")
        .collect::<Vec<_>>();
    assert_eq!(namespace_items.len(), 3);
    assert_eq!(
        namespace_items
            .iter()
            .map(|item| item.physical.namespace)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            CompilerFactNamespace::Type,
            CompilerFactNamespace::Value,
            CompilerFactNamespace::Macro,
        ])
    );
    assert_unique_ids(&namespace_items);

    let associated = items(fragment)
        .filter(|item| diagnostic_path(fragment, item) == "AssociatedNames::Shared")
        .collect::<Vec<_>>();
    assert_eq!(associated.len(), 2);
    assert_eq!(
        associated
            .iter()
            .map(|item| item.physical.kind)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            CompilerFactItemKind::AssociatedConstant,
            CompilerFactItemKind::AssociatedType,
        ])
    );
    assert_unique_ids(&associated);

    let reexport_scope = item("same_name_reexports", CompilerFactItemKind::Module);
    let reexports = items(fragment)
        .filter(|item| {
            item.physical.kind == CompilerFactItemKind::Reexport
                && item_name(fragment, item) == "Shared"
                && item.parent == Some(reexport_scope.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(reexports.len(), 2);
    assert_unique_ids(&reexports);

    let anonymous = items(fragment)
        .filter(|item| {
            item.physical.kind == CompilerFactItemKind::Constant
                && matches!(item.macro_provenance, CompilerFactMacroProvenance::Expansion(_))
                && item_name(fragment, item) == "_"
        })
        .collect::<Vec<_>>();
    assert_eq!(anonymous.len(), 2);
    assert_unique_ids(&anonymous);

    let dependency_versions = item("dependency_versions", CompilerFactItemKind::Function);
    let local_ids = items(fragment).map(|item| item.id).collect::<HashSet<_>>();
    let external_targets = fragment
        .object
        .edges
        .iter()
        .filter(|edge| {
            edge.source == dependency_versions.id
                && edge.kind == CompilerFactEdgeKind::Body
                && !local_ids.contains(&edge.target)
        })
        .map(|edge| edge.target)
        .collect::<Vec<_>>();
    // Cargo does not expose a portable package identity to rustc. Equivalent
    // paths in same-named dependency versions therefore share one logical
    // compiler identity. Surface resolves that identity to every matching
    // physical declaration rather than trusting Cargo's root-sensitive
    // `-Cmetadata` disambiguator.
    assert_eq!(external_targets.len(), 1);
}

fn items(fragment: &CompilerFactFragment) -> impl Iterator<Item = &CompilerItemFact> {
    fragment.object.items.iter()
}

fn diagnostic_path<'a>(fragment: &'a CompilerFactFragment, item: &CompilerItemFact) -> &'a str {
    &fragment.object.strings[item.diagnostic_path.0 as usize]
}

fn item_name<'a>(fragment: &'a CompilerFactFragment, item: &CompilerItemFact) -> &'a str {
    &fragment.object.strings[item.name.0 as usize]
}

fn assert_unique_ids(items: &[&CompilerItemFact]) {
    assert_eq!(
        items.iter().map(|item| item.id).collect::<HashSet<_>>().len(),
        items.len(),
        "compiler item identities collided: {:?}",
        items
            .iter()
            .map(|item| (item.id, item.physical.kind, item.physical.namespace))
            .collect::<Vec<_>>()
    );
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
    let digest = Sha256::digest(&bytes)
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
