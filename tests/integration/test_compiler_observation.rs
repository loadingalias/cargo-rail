use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest as _, Sha256};

use crate::helpers::TestWorkspace;

const CACHE_WRAPPER_MARKER: &str = "CARGO_RAIL_COMPILER_CACHE_WRAPPER";
const CACHE_CONTROL_ENV: &str = "CARGO_RAIL_CACHE";
const BENCH_COVERAGE_CONTROL: &str = "__cargo_rail_benchmark_coverage_v1";
const BENCH_COVERAGE_DIRECTORY_ENV: &str = "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY";

fn compiler_fact_path_identity(path: &Path) -> Result<String> {
    let path = cargo_rail::utils::canonicalize_existing(path)?;
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-compiler-fact-path-v1\0");
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256:{digest}"))
}

fn write_compiler_fact_capability(observation_directory: &Path, source_root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(observation_directory)?;
    let capability = observation_directory.join("test-fact-session.cap");
    let encoded = serde_json::to_vec(&serde_json::json!({
      "version": 4,
      "observation_directory_identity": compiler_fact_path_identity(observation_directory)?,
      "source_root_identity": compiler_fact_path_identity(source_root)?,
      "fact_families": ["StableDiagnostics"],
    }))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    use std::io::Write as _;
    options.open(&capability)?.write_all(&encoded)?;
    Ok(capability)
}

#[cfg(any(unix, windows))]
#[test]
#[ignore = "requires the separately manufactured exact-toolchain companion"]
fn stable_wrapper_authorizes_the_matched_driver_per_compilation_unit() {
    let result: Result<()> = (|| {
        let first = stable_wrapper_typed_fixture(false)?;
        let second = stable_wrapper_typed_fixture(false)?;
        let reusable_objects = |fragments: Vec<serde_json::Value>| {
            fragments
                .into_iter()
                .map(|fragment| {
                    let object = fragment["object"].clone();
                    let mut logical_unit = object["unit"].clone();
                    logical_unit
                        .as_object_mut()
                        .context("compiler fact unit")?
                        .remove("identity");
                    logical_unit
                        .as_object_mut()
                        .context("compiler fact unit")?
                        .remove("invocation_identity");
                    Ok((serde_json::to_vec(&logical_unit)?, object))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>>>()
        };
        let first = reusable_objects(first)?;
        let second = reusable_objects(second)?;
        assert_eq!(first.keys().collect::<Vec<_>>(), second.keys().collect::<Vec<_>>());
        for (unit, left) in first {
            let right = second.get(&unit).context("second compiler fact unit")?;
            if let Some(path) = json_difference(&left, right) {
                anyhow::bail!(
                    "moved-root object changed at {path}: {} != {}",
                    left.pointer(&path).unwrap_or(&serde_json::Value::Null),
                    right.pointer(&path).unwrap_or(&serde_json::Value::Null)
                );
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(any(unix, windows))]
fn json_difference(left: &serde_json::Value, right: &serde_json::Value) -> Option<String> {
    fn find(left: &serde_json::Value, right: &serde_json::Value, path: &mut String) -> Option<String> {
        match (left, right) {
            (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
                for key in left
                    .keys()
                    .chain(right.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    let length = path.len();
                    path.push('/');
                    path.push_str(key);
                    match (left.get(key), right.get(key)) {
                        (Some(left), Some(right)) => {
                            if let Some(difference) = find(left, right, path) {
                                return Some(difference);
                            }
                        }
                        _ => return Some(path.clone()),
                    }
                    path.truncate(length);
                }
                None
            }
            (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
                for index in 0..left.len().max(right.len()) {
                    let length = path.len();
                    path.push('/');
                    path.push_str(&index.to_string());
                    match (left.get(index), right.get(index)) {
                        (Some(left), Some(right)) => {
                            if let Some(difference) = find(left, right, path) {
                                return Some(difference);
                            }
                        }
                        _ => return Some(path.clone()),
                    }
                    path.truncate(length);
                }
                None
            }
            _ if left == right => None,
            _ => Some(path.clone()),
        }
    }

    find(left, right, &mut String::new())
}

#[cfg(unix)]
#[test]
#[ignore = "requires the separately manufactured exact-toolchain companion"]
fn stable_rustdoc_proxy_authorizes_generated_doctest_units() -> Result<()> {
    let _ = stable_wrapper_typed_fixture(true)?;
    Ok(())
}

#[cfg(any(unix, windows))]
fn stable_wrapper_typed_fixture(doctest: bool) -> Result<Vec<serde_json::Value>> {
    let driver = std::env::var_os("CARGO_RAIL_TEST_FACT_DRIVER")
        .map(PathBuf::from)
        .context("CARGO_RAIL_TEST_FACT_DRIVER")?;
    let workspace = tempfile::tempdir()?;
    fs::create_dir(workspace.path().join("src"))?;
    if doctest {
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"fact-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n",
        )?;
        fs::write(
            workspace.path().join("src/lib.rs"),
            "/// Returns seven.\n///\n/// ```\n/// assert_eq!(fact_probe::value(), 7);\n/// ```\npub fn value() -> u8 { helper() }\nfn helper() -> u8 { 7 }\n",
        )?;
    } else {
        write_compiler_fact_corpus(workspace.path())?;
    }
    let lock = Command::new("cargo")
        .current_dir(workspace.path())
        .arg("generate-lockfile")
        .output()?;
    anyhow::ensure!(lock.status.success(), "{}", String::from_utf8_lossy(&lock.stderr));

    let source_root = fs::canonicalize(workspace.path())?;
    let observation_directory = source_root.join("observations");
    fs::create_dir(&observation_directory)?;
    let sysroot = Command::new("rustc").args(["--print", "sysroot"]).output()?;
    anyhow::ensure!(sysroot.status.success());
    let toolchain_sysroot = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim());
    let compiler_library_directory = toolchain_sysroot.join(if cfg!(windows) { "bin" } else { "lib" });
    #[cfg(unix)]
    let doctest_sysroot = doctest
        .then(|| stage_test_doctest_sysroot(&source_root, &toolchain_sysroot))
        .transpose()?;
    #[cfg(windows)]
    let doctest_sysroot: Option<PathBuf> = {
        anyhow::ensure!(!doctest, "the Windows corpus does not use the Unix doctest fixture");
        None
    };
    let capability = observation_directory.join("typed-session.cap");
    let coverage = [
        "definitions",
        "visibility",
        "exact_spans",
        "macro_provenance",
        "body_edges",
        "interface_edges",
        "reexport_edges",
        "privacy_edges",
        "trait_dispatch",
        "foreign_exports",
        "generated_sources",
        "entry_points",
        "conservative_retention",
    ];
    let targets = if doctest {
        serde_json::json!([{
          "package": { "name": "fact-probe", "version": "0.0.0", "source": null },
          "manifest_directory": "",
          "cargo_target": "fact_probe",
          "crate_name": "fact_probe",
          "target_kind": { "kind": "library" },
          "source": "src/lib.rs",
          "doctest": true,
        }])
    } else {
        compiler_fact_corpus_targets()
    };
    let encoded = serde_json::to_vec(&serde_json::json!({
      "version": 4,
      "observation_directory_identity": compiler_fact_path_identity(&observation_directory)?,
      "source_root_identity": compiler_fact_path_identity(&source_root)?,
      "fact_families": ["TypedRustItems"],
      "typed": {
        "run_authority": {
          "run_identity": format!("compiler-fact-run-v1-sha256-{}", "1".repeat(64)),
          "view_identity": format!("compiler-fact-view-v1-sha256-{}", "2".repeat(64)),
        },
        "producer_authority": {
          "compiler_identity": format!("compiler-fact-compiler-v1-sha256-{}", "3".repeat(64)),
          "driver_identity": format!("compiler-fact-driver-v1-sha256-{}", "4".repeat(64)),
        },
        "driver_program": fs::canonicalize(&driver)?,
        "rustc_program": fs::canonicalize(which_rustc()?)?,
        "compiler_library_directory": fs::canonicalize(&compiler_library_directory)?,
        "host_platform": rustc_host()?,
        "target_platform": rustc_host()?,
        "doctest": doctest,
        "doctest_sysroot": doctest_sysroot,
        "generated_roots": [source_root.join("target")],
        "required_coverage": coverage,
        "targets": targets,
      },
    }))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    use std::io::Write as _;
    options.open(&capability)?.write_all(&encoded)?;

    let cargo_arguments = if doctest {
        vec!["test", "--locked", "--doc", "--message-format=json"]
    } else {
        vec![
            "check",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--message-format=json",
        ]
    };
    let output = Command::new("cargo")
        .current_dir(&source_root)
        .args(cargo_arguments)
        .env("RUSTC_WORKSPACE_WRAPPER", env!("CARGO_BIN_EXE_cargo-rail"))
        .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
        .env("RUSTDOC", env!("CARGO_BIN_EXE_cargo-rail"))
        .env("CARGO_RAIL_INNER_RUSTDOC", fs::canonicalize(which_rustdoc()?)?)
        .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
        .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observation_directory)
        .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", &source_root)
        .env("CARGO_RAIL_COMPILER_FACT_SESSION", &capability)
        .env_remove("RUSTC_BOOTSTRAP")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let announcements = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| {
            event["reason"] == "compiler-message" && event["message"]["code"]["code"] == "cargo_rail_compiler_fact_v1"
        })
        .collect::<Vec<_>>();
    if doctest {
        anyhow::ensure!(
            announcements.is_empty(),
            "rustdoc-child facts must not claim a Cargo compiler-message envelope"
        );
    } else {
        let announced_targets = announcements
            .iter()
            .filter_map(|announcement| announcement["target"]["name"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        anyhow::ensure!(announced_targets.contains("fact_macro"));
        anyhow::ensure!(announced_targets.contains("fact_probe"));
    }
    let observation_files = fs::read_dir(&observation_directory)?.collect::<Result<Vec<_>, _>>()?;
    let sidecars = observation_files
        .iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("compiler-fact-fragment-sha256-")
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !sidecars.is_empty(),
        "no sidecars; observations: {:?}; cargo stdout: {stdout}; cargo stderr: {}",
        observation_files
            .iter()
            .map(std::fs::DirEntry::file_name)
            .collect::<Vec<_>>(),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut fragments = sidecars
        .iter()
        .map(|sidecar| fs::read(sidecar.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes))
        .collect::<Result<Vec<_>, _>>()?;
    fragments.sort_by(|left, right| {
        left["object"]["unit"]["identity"]
            .as_str()
            .cmp(&right["object"]["unit"]["identity"].as_str())
    });
    if doctest {
        for fragment in &fragments {
            anyhow::ensure!(
                fragment["object"]["unit"]["domain"] == "doctest",
                "unexpected doctest unit domain: {}",
                fragment["object"]["unit"]["domain"]
            );
        }
    } else {
        assert_compiler_fact_corpus(&fragments);
    }
    Ok(fragments)
}

#[cfg(any(unix, windows))]
fn write_compiler_fact_corpus(root: &Path) -> Result<()> {
    for directory in ["src/bin", "tests", "examples", "benches", "fact-macro/src"] {
        fs::create_dir_all(root.join(directory))?;
    }
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fact-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\nbuild = \"build.rs\"\n\n[workspace]\nmembers = [\"fact-macro\"]\nresolver = \"3\"\n\n[features]\ndefault = []\nextra = []\n\n[dependencies]\nfact-macro = { path = \"fact-macro\" }\n\n[[bench]]\nname = \"fact_bench\"\nharness = false\n",
    )?;
    fs::write(
        root.join("src/lib.rs"),
        "#![allow(dead_code)]\n\npub mod nested { pub fn reexported() -> usize { 1 } }\npub use nested::reexported;\n\npub trait Compute { fn compute(&self) -> usize; }\npub struct PublicType { pub visible: usize, private: usize }\nimpl Compute for PublicType { fn compute(&self) -> usize { self.visible + self.private } }\npub fn dispatch(value: &dyn Compute) -> usize { value.compute() }\n\nmacro_rules! generated_item { () => { pub fn expanded() -> usize { 2 } } }\ngenerated_item!();\ninclude!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n\n#[fact_macro::retain]\npub fn attributed() -> usize { generated_from_build() }\n\n#[cfg(feature = \"extra\")]\npub fn feature_item() -> usize { 3 }\n#[cfg(any(unix, windows))]\npub fn platform_item() -> usize { 4 }\n\n#[unsafe(no_mangle)]\npub extern \"C\" fn exported_symbol() -> usize { 5 }\n#[unsafe(export_name = \"cargo_rail_named_export\")]\npub extern \"C\" fn named_export() -> usize { 6 }\nunsafe extern \"C\" { pub fn external_symbol(); }\n",
    )?;
    fs::write(
        root.join("build.rs"),
        "fn main() { let out = std::env::var_os(\"OUT_DIR\").unwrap(); std::fs::write(std::path::PathBuf::from(out).join(\"generated.rs\"), \"pub fn generated_from_build() -> usize { 6 }\\n\").unwrap(); }\n",
    )?;
    fs::write(
        root.join("src/bin/fact_bin.rs"),
        "fn helper() {}\nfn main() { helper(); }\n",
    )?;
    fs::write(
        root.join("tests/fact_test.rs"),
        "#[test]\nfn integration_entry() { assert_eq!(fact_probe::expanded(), 2); }\n",
    )?;
    fs::write(
        root.join("examples/fact_example.rs"),
        "fn main() { let _ = fact_probe::feature_item(); }\n",
    )?;
    fs::write(
        root.join("benches/fact_bench.rs"),
        "fn main() { let _ = fact_probe::platform_item(); }\n",
    )?;
    fs::write(
        root.join("fact-macro/Cargo.toml"),
        "[package]\nname = \"fact-macro\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nproc-macro = true\n",
    )?;
    fs::write(
        root.join("fact-macro/src/lib.rs"),
        "extern crate proc_macro;\nuse proc_macro::TokenStream;\n#[proc_macro_attribute]\npub fn retain(_attribute: TokenStream, item: TokenStream) -> TokenStream { item }\n",
    )?;
    Ok(())
}

#[cfg(any(unix, windows))]
fn compiler_fact_corpus_targets() -> serde_json::Value {
    let package = || serde_json::json!({ "name": "fact-probe", "version": "0.0.0", "source": null });
    serde_json::json!([
      { "package": { "name": "fact-macro", "version": "0.0.0", "source": null }, "manifest_directory": "fact-macro", "cargo_target": "fact_macro", "crate_name": "fact_macro", "target_kind": { "kind": "proc_macro" }, "source": "fact-macro/src/lib.rs", "doctest": true },
      { "package": package(), "manifest_directory": "", "cargo_target": "build-script-build", "crate_name": "build_script_build", "target_kind": { "kind": "build_script" }, "source": "build.rs", "doctest": false },
      { "package": package(), "manifest_directory": "", "cargo_target": "fact_bench", "crate_name": "fact_bench", "target_kind": { "kind": "benchmark" }, "source": "benches/fact_bench.rs", "doctest": false },
      { "package": package(), "manifest_directory": "", "cargo_target": "fact_bin", "crate_name": "fact_bin", "target_kind": { "kind": "binary" }, "source": "src/bin/fact_bin.rs", "doctest": false },
      { "package": package(), "manifest_directory": "", "cargo_target": "fact_example", "crate_name": "fact_example", "target_kind": { "kind": "example" }, "source": "examples/fact_example.rs", "doctest": false },
      { "package": package(), "manifest_directory": "", "cargo_target": "fact_probe", "crate_name": "fact_probe", "target_kind": { "kind": "library" }, "source": "src/lib.rs", "doctest": true },
      { "package": package(), "manifest_directory": "", "cargo_target": "fact_test", "crate_name": "fact_test", "target_kind": { "kind": "test" }, "source": "tests/fact_test.rs", "doctest": false }
    ])
}

#[cfg(any(unix, windows))]
fn assert_compiler_fact_corpus(fragments: &[serde_json::Value]) {
    let objects = fragments.iter().map(|fragment| &fragment["object"]).collect::<Vec<_>>();
    let domains = objects
        .iter()
        .filter_map(|object| object["unit"]["domain"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        domains,
        std::collections::BTreeSet::from(["build_script", "non_production", "proc_macro", "production"])
    );
    assert!(objects.iter().any(|object| {
        object["unit"]["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|feature| feature == "extra"))
    }));

    let item_kinds = object_nested_values(&objects, "items", "physical", "kind");
    for required in [
        "field",
        "foreign_function",
        "function",
        "impl",
        "method",
        "module",
        "reexport",
        "struct",
        "trait",
    ] {
        assert!(
            item_kinds.contains(required),
            "missing item kind {required}: {item_kinds:?}"
        );
    }
    let edge_kinds = object_values(&objects, "edges", "kind");
    for required in ["body", "interface", "privacy_parent", "reexport"] {
        let accepted = required != "privacy_parent"
            || edge_kinds.contains("visibility_parent")
            || edge_kinds.contains("visibility_requirement");
        assert!(
            accepted || edge_kinds.contains(required),
            "missing edge kind {required}: {edge_kinds:?}"
        );
    }
    let entry_kinds = object_values(&objects, "entry_points", "kind");
    for required in ["benchmark_harness", "build_script", "main", "test_harness"] {
        assert!(
            entry_kinds.contains(required),
            "missing entry kind {required}: {entry_kinds:?}"
        );
    }
    let retention_kinds = object_nested_values(&objects, "retentions", "reason", "kind");
    for required in ["export_name", "no_mangle", "proc_macro", "unresolved_trait_dispatch"] {
        assert!(
            retention_kinds.contains(required),
            "missing retention reason {required}: {retention_kinds:?}"
        );
    }
    let provenance = object_nested_values(&objects, "items", "macro_provenance", "kind");
    assert!(provenance.contains("expansion"), "missing macro expansion provenance");
    let source_roots = object_nested_values(&objects, "sources", "path", "root");
    assert!(source_roots.contains("generated"), "missing generated source identity");
}

#[cfg(any(unix, windows))]
fn object_values<'a>(
    objects: &[&'a serde_json::Value],
    table: &str,
    field: &str,
) -> std::collections::BTreeSet<&'a str> {
    objects
        .iter()
        .flat_map(|object| object[table].as_array().into_iter().flatten())
        .filter_map(|entry| entry[field].as_str())
        .collect()
}

#[cfg(any(unix, windows))]
fn object_nested_values<'a>(
    objects: &[&'a serde_json::Value],
    table: &str,
    field: &str,
    nested: &str,
) -> std::collections::BTreeSet<&'a str> {
    objects
        .iter()
        .flat_map(|object| object[table].as_array().into_iter().flatten())
        .filter_map(|entry| entry[field][nested].as_str())
        .collect()
}

#[cfg(unix)]
fn stage_test_doctest_sysroot(source_root: &Path, toolchain_sysroot: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::symlink;

    let root = source_root.join("doctest-sysroot");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("bin"))?;
    fs::create_dir(root.join("lib"))?;
    symlink(env!("CARGO_BIN_EXE_cargo-rail"), root.join("bin/rustc"))?;
    symlink(fs::canonicalize(which_rustdoc()?)?, root.join("bin/rustdoc"))?;
    for entry in fs::read_dir(toolchain_sysroot.join("lib"))? {
        let entry = entry?;
        let destination = root.join("lib").join(entry.file_name());
        if entry.file_type()?.is_dir() {
            symlink(entry.path(), destination)?;
        } else {
            fs::hard_link(entry.path(), destination)?;
        }
    }
    Ok(root)
}

fn rustc_host() -> Result<String> {
    let output = Command::new("rustc").arg("-vV").output()?;
    anyhow::ensure!(output.status.success(), "rustc -vV failed");
    String::from_utf8(output.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
        .context("rustc host")
}

fn which_rustc() -> Result<PathBuf> {
    let output = Command::new("rustup").args(["which", "rustc"]).output()?;
    anyhow::ensure!(output.status.success(), "rustup which rustc failed");
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}

fn which_rustdoc() -> Result<PathBuf> {
    let output = Command::new("rustup").args(["which", "rustdoc"]).output()?;
    anyhow::ensure!(output.status.success(), "rustup which rustdoc failed");
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}
#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[test]
fn cache_off_bypasses_direct_wrapper_context_and_cas_acquisition() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        let absent_cache = state.path().join("cache-must-not-exist");
        let absent_session = state.path().join("session-must-not-be-read.json");
        let absent_coverage = state.path().join("coverage-must-not-exist");

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .args(["rustc", "--version"])
            .env(CACHE_CONTROL_ENV, "off")
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env("CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION", &absent_session)
            .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &absent_coverage)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert!(output.status.success(), "cache-off compiler bypass failed: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).starts_with("rustc "),
            "selected compiler output was not preserved: {output:?}"
        );
        assert!(!absent_cache.exists(), "cache-off bypass acquired the CAS");
        assert!(
            !absent_session.exists(),
            "cache-off bypass created or changed session state"
        );
        assert!(
            !absent_coverage.exists(),
            "cache-off bypass acquired benchmark coverage state"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cache_off_preserves_the_rustdoc_proxy_role() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        let absent_cache = state.path().join("cache-must-not-exist");

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .arg("--version")
            .env(CACHE_CONTROL_ENV, "off")
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
            .env("CARGO_RAIL_INNER_RUSTDOC", which_rustdoc()?)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .output()?;

        assert!(output.status.success(), "cache-off rustdoc proxy failed: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).starts_with("rustdoc "),
            "selected rustdoc output was not preserved: {output:?}"
        );
        assert!(!absent_cache.exists(), "cache-off rustdoc proxy acquired the CAS");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unsupported_incremental_invocation_bypasses_before_direct_context_load() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        fs::create_dir_all(state.path().join("src"))?;
        fs::create_dir_all(state.path().join("out"))?;
        fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
        let absent_cache = state.path().join("cache-must-not-exist");

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .current_dir(state.path())
            .args([
                "rustc",
                "--crate-name",
                "fixture",
                "--crate-type=lib",
                "--emit=dep-info,metadata",
                "--error-format=json",
                "--out-dir",
                "out",
                "-Cincremental=incremental",
                "src/lib.rs",
            ])
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env_remove(CACHE_CONTROL_ENV)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert!(
            output.status.success(),
            "incremental compiler bypass failed: {output:?}"
        );
        assert!(!absent_cache.exists(), "unsupported invocation acquired the CAS");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn benchmark_coverage_records_fast_bypass_without_cache_context() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        let state_root = fs::canonicalize(state.path())?;
        let coverage = state_root.join("coverage");
        fs::create_dir(&coverage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&coverage, fs::Permissions::from_mode(0o700))?;
        }
        let absent_cache = state_root.join("cache-must-not-exist");

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .args(["rustc", "--version"])
            .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
            .env(BENCH_COVERAGE_DIRECTORY_ENV, &coverage)
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert!(output.status.success(), "benchmark compiler bypass failed: {output:?}");
        assert!(!absent_cache.exists(), "benchmark fast bypass acquired the CAS");
        let events = fs::read_dir(&coverage)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(events.len(), 1, "benchmark bypass did not retain one event");
        let event: serde_json::Value = serde_json::from_slice(&fs::read(events[0].path())?)?;
        assert_eq!(event["status"], "bypassed");
        assert_eq!(event["reason"], "compiler_information_request");
        assert_eq!(event["compiler"], "rustc");
        assert_eq!(event["arguments"], serde_json::json!(["--version"]));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn benchmark_coverage_records_compiler_mode_cold_boundaries() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        let state_root = fs::canonicalize(state.path())?;
        let coverage = state_root.join("coverage");
        let docs = state_root.join("docs");
        fs::create_dir(&coverage)?;
        fs::create_dir(&docs)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&coverage, fs::Permissions::from_mode(0o700))?;
        }
        fs::write(
            state_root.join("lib.rs"),
            "/// Returns seven.\n///\n/// ```\n/// assert_eq!(7, 7);\n/// ```\npub fn value() -> u8 { 7 }\n",
        )?;
        let absent_cache = state_root.join("cache-must-not-exist");
        let wrapper = env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper");
        let run_rustdoc = |arguments: &[&str]| -> Result<std::process::Output> {
            Ok(Command::new(wrapper)
                .current_dir(&state_root)
                .args(arguments)
                .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
                .env("CARGO_RAIL_INNER_RUSTDOC", which_rustdoc()?)
                .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
                .env(BENCH_COVERAGE_DIRECTORY_ENV, &coverage)
                .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
                .env_remove(CACHE_WRAPPER_MARKER)
                .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
                .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
                .output()?)
        };

        let rustc_probe = Command::new(wrapper)
            .arg(which_rustc()?)
            .arg("-vV")
            .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
            .env("CARGO_RAIL_INNER_RUSTDOC", which_rustdoc()?)
            .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
            .env(BENCH_COVERAGE_DIRECTORY_ENV, &coverage)
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
            .output()?;
        assert!(
            rustc_probe.status.success(),
            "rustc probe inherited by the rustdoc proxy failed: {rustc_probe:?}"
        );

        let mut clippy = Command::new(wrapper);
        clippy
            .current_dir(&state_root)
            .arg("clippy-driver")
            .arg(which_rustc()?)
            .args([
                "--crate-name",
                "fixture_clippy",
                "--crate-type=lib",
                "--edition=2024",
                "--emit=metadata",
                "--out-dir",
                "docs",
                "lib.rs",
            ])
            .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
            .env(BENCH_COVERAGE_DIRECTORY_ENV, &coverage)
            .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION");
        let clippy = clippy.output()?;
        assert!(clippy.status.success(), "Clippy bypass failed: {clippy:?}");

        let documentation = run_rustdoc(&[
            "--crate-name",
            "fixture",
            "--crate-type=lib",
            "--edition=2024",
            "-o",
            "docs",
            "lib.rs",
        ])?;
        assert!(
            documentation.status.success(),
            "rustdoc bypass failed: {documentation:?}"
        );
        assert!(docs.join("fixture/index.html").is_file());
        let doctest = run_rustdoc(&["--test", "--crate-name", "fixture", "--edition=2024", "lib.rs"])?;
        assert!(doctest.status.success(), "doctest bypass failed: {doctest:?}");
        assert!(!absent_cache.exists(), "documentation bypass acquired the CAS");

        let mut events = fs::read_dir(&coverage)?
            .map(|entry| -> Result<serde_json::Value> { Ok(serde_json::from_slice(&fs::read(entry?.path())?)?) })
            .collect::<Result<Vec<_>>>()?;
        events.sort_by(|left, right| left["reason"].as_str().cmp(&right["reason"].as_str()));
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["reason"], "clippy_diagnostic_result_authority_unavailable");
        assert_eq!(events[0]["action"]["driver"], "clippy");
        assert_eq!(events[0]["action"]["test"], false);
        assert_eq!(events[1]["reason"], "compiler_information_request");
        assert_eq!(events[1]["action"]["driver"], "rustc");
        assert_eq!(events[2]["reason"], "doctest_execution_result_authority_unavailable");
        assert_eq!(events[2]["action"]["driver"], "rustdoc");
        assert_eq!(events[2]["action"]["test"], true);
        assert_eq!(events[3]["reason"], "rustdoc_output_tree_observation_unavailable");
        assert_eq!(events[3]["action"]["driver"], "rustdoc");
        assert_eq!(events[3]["action"]["test"], false);
        assert!(events.iter().all(|event| {
            event["status"] == "bypassed"
                && event["action_key"].is_null()
                && event["result_key"].is_null()
                && event["cache_bytes_read"] == 0
                && event["remote_request_attempts"] == 0
        }));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn benchmark_coverage_rejects_a_symlink_without_changing_compiler_behavior() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let state = tempfile::tempdir()?;
        let state_root = fs::canonicalize(state.path())?;
        let real = state_root.join("real-coverage");
        let selected = state_root.join("selected-coverage");
        fs::create_dir(&real)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&real, fs::Permissions::from_mode(0o700))?;
        }
        symlink(&real, &selected)?;

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .args(["rustc", "--version"])
            .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
            .env(BENCH_COVERAGE_DIRECTORY_ENV, &selected)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert!(
            output.status.success(),
            "hostile benchmark path changed compiler behavior"
        );
        assert!(
            fs::read_dir(real)?.next().is_none(),
            "hostile benchmark path received evidence"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn cache_off_bypass_preserves_compiler_signal_status() {
    let result: Result<()> = (|| {
        use std::os::unix::process::ExitStatusExt as _;

        let state = tempfile::tempdir()?;
        let compiler = state.path().join("signal-compiler");
        write_executable(&compiler, "#!/bin/sh\nkill -TERM $$\n")?;

        let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .arg(&compiler)
            .env(CACHE_CONTROL_ENV, "off")
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .status()?;

        assert_eq!(status.signal(), Some(15));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn cache_off_bypass_preserves_non_utf8_argument_bytes() {
    let result: Result<()> = (|| {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let state = tempfile::tempdir()?;
        let compiler = state.path().join("argument-compiler");
        let captured = state.path().join("captured-argument");
        write_executable(&compiler, "#!/bin/sh\nprintf '%s' \"$1\" > \"$CAPTURE_PATH\"\n")?;
        let argument = vec![b'a', b'r', b'g', b'-', 0x80, 0xff];

        let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
            .arg(&compiler)
            .arg(OsString::from_vec(argument.clone()))
            .env(CACHE_CONTROL_ENV, "off")
            .env("CAPTURE_PATH", &captured)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .status()?;

        assert!(status.success());
        assert_eq!(fs::read(captured)?, argument);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn fact_driver_preserves_compiler_signal_status_after_publication() {
    let result: Result<()> = (|| {
        use std::os::unix::process::ExitStatusExt as _;

        let state = tempfile::tempdir()?;
        fs::create_dir_all(state.path().join("src"))?;
        fs::create_dir_all(state.path().join("out"))?;
        fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
        let compiler = state.path().join("signal-compiler");
        write_executable(&compiler, "#!/bin/sh\nkill -TERM $$\n")?;
        let observations = state.path().join("observations");
        let fact_capability = write_compiler_fact_capability(&observations, state.path())?;

        let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(state.path())
            .arg(&compiler)
            .args([
                "--crate-name",
                "fixture",
                "--crate-type=lib",
                "--emit=dep-info,metadata",
                "--error-format=json",
                "--out-dir",
                "out",
                "src/lib.rs",
            ])
            .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
            .env("CARGO_RAIL_COMPILER_FACT_SESSION", fact_capability)
            .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observations)
            .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", state.path())
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .status()?;

        assert_eq!(status.signal(), Some(15));
        assert!(fs::read_dir(observations)?.any(|entry| {
            entry.ok().is_some_and(|entry| {
                entry
                    .path()
                    .file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"rustc-"))
            })
        }));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn overlapping_observation_markers_dispatch_by_cargo_wrapper_shape() {
    let result: Result<()> = (|| {
        let rustdoc = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .arg("--version")
            .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
            .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
            .env("CARGO_RAIL_INNER_RUSTDOC", which_rustdoc()?)
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove(CACHE_CONTROL_ENV)
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
            .output()?;
        assert!(
            rustdoc.status.success(),
            "Cargo's rustdoc role was not selected: {rustdoc:?}"
        );

        let rustc = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .arg(which_rustc()?)
            .arg("--version")
            .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
            .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
            .env_remove("CARGO_RAIL_INNER_RUSTDOC")
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove(CACHE_CONTROL_ENV)
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
            .output()?;
        assert!(
            rustc.status.success(),
            "Cargo's rustc workspace-wrapper role was not selected: {rustc:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn absent_fact_capability_executes_the_original_compiler_without_collection() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        fs::create_dir_all(state.path().join("src"))?;
        fs::create_dir_all(state.path().join("out"))?;
        fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(state.path())
            .args([
                "rustc",
                "--crate-name",
                "fixture",
                "--crate-type=lib",
                "--emit=metadata",
                "--out-dir",
                "out",
                "src/lib.rs",
            ])
            .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
            .env_remove("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY")
            .env_remove("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT")
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert!(output.status.success(), "fact-free compiler bypass failed: {output:?}");
        assert!(fs::read_dir(state.path().join("out"))?.next().is_some());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn incomplete_fact_capability_fails_before_compiler_execution() {
    let result: Result<()> = (|| {
        let state = tempfile::tempdir()?;
        fs::create_dir_all(state.path().join("src"))?;
        fs::create_dir_all(state.path().join("out"))?;
        fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(state.path())
            .args([
                "rustc",
                "--crate-name",
                "fixture",
                "--crate-type=lib",
                "--emit=metadata",
                "--out-dir",
                "out",
                "src/lib.rs",
            ])
            .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
            .env(
                "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY",
                state.path().join("observations"),
            )
            .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", state.path())
            .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
            .env_remove(CACHE_WRAPPER_MARKER)
            .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
            .output()?;

        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("compiler fact capability is incomplete"));
        assert!(fs::read_dir(state.path().join("out"))?.next().is_none());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn rustdoc_proxy_preserves_cargo_docs_and_records_dep_info() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_single_crate("rustdoc-observation", "0.1.0")?;
        fs::write(
            workspace.path.join("src/lib.rs"),
            "mod nested;\npub use nested::value;\n",
        )?;
        fs::write(workspace.path.join("src/nested.rs"), "pub fn value() -> u8 { 1 }\n")?;
        let observation_directory = workspace.path.join("observations");
        let target_directory = workspace.path.join("target-observation");
        let fact_capability = write_compiler_fact_capability(&observation_directory, &workspace.path)?;

        let output = Command::new("cargo")
            .current_dir(&workspace.path)
            .args(["doc", "--no-deps", "--message-format=json", "--target-dir"])
            .arg(&target_directory)
            .env("RUSTDOC", env!("CARGO_BIN_EXE_cargo-rail"))
            .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
            .env("CARGO_RAIL_INNER_RUSTDOC", "rustdoc")
            .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observation_directory)
            .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", &workspace.path)
            .env("CARGO_RAIL_COMPILER_FACT_SESSION", fact_capability)
            .output()
            .context("run cargo doc through the rustdoc observation proxy")?;
        assert!(
            output.status.success(),
            "cargo doc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let index = target_directory.join("doc/rustdoc_observation/index.html");
        assert!(
            index.is_file(),
            "rustdoc proxy must preserve HTML output at {}",
            index.display()
        );
        let canonical_index = fs::canonicalize(&index)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let cargo_retained_index = stdout.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|message| message["filenames"].as_array().cloned())
                .is_some_and(|filenames| {
                    filenames.iter().any(|filename| {
                        filename
                            .as_str()
                            .and_then(|filename| fs::canonicalize(filename).ok())
                            .is_some_and(|filename| filename == canonical_index)
                    })
                })
        });
        assert!(
            cargo_retained_index,
            "Cargo's stable artifact message must retain the documentation index\n{}",
            stdout
        );

        let records = fs::read_dir(&observation_directory)?
            .map(|entry| -> Result<serde_json::Value> {
                let path = entry?.path();
                Ok(serde_json::from_slice(&fs::read(path)?)?)
            })
            .collect::<Result<Vec<_>>>()?;
        let record = records
            .iter()
            .find(|record| record["crate_name"] == "rustdoc_observation")
            .context("rustdoc crate invocation observation")?;
        assert_eq!(record["mode"], "rustdoc");
        assert_eq!(record["success"], true);
        let records_dep_info = record["compiler_arguments"].as_array().is_some_and(|arguments| {
            arguments.iter().any(|argument| {
                argument
                    .as_str()
                    .is_some_and(|argument| argument.starts_with("--emit=") && argument.contains("dep-info"))
            })
        });
        let observed_paths = record["observed_reads"]
            .as_array()
            .context("observed rustdoc reads")?
            .iter()
            .filter_map(|read| read["path"]["path"].as_str())
            .collect::<Vec<_>>();
        let declared_paths = record["declared_inputs"]
            .as_array()
            .context("declared rustdoc inputs")?
            .iter()
            .filter_map(|input| input["path"]["path"].as_str())
            .collect::<Vec<_>>();
        assert!(
            declared_paths.contains(&"src/lib.rs") || observed_paths.contains(&"src/lib.rs"),
            "crate root missing from {record}"
        );
        if records_dep_info {
            assert!(
                observed_paths.contains(&"src/nested.rs"),
                "module source missing from {record}"
            );
            assert!(
                record["emitted_outputs"].as_array().is_some_and(|outputs| outputs
                    .iter()
                    .any(|output| { output["path"]["path"] == "target-observation/doc/rustdoc_observation.d" })),
                "rustdoc dep-info output missing from {record}"
            );
        } else {
            assert!(
                record["bypasses"]
                    .as_array()
                    .is_some_and(|bypasses| bypasses.iter().any(|reason| reason == "rustdoc_dep_info_unavailable")),
                "rustdoc without stable dep-info must remain an explicit bypass: {record}"
            );
        }
        assert!(
            record["bypasses"].as_array().is_some_and(|bypasses| bypasses
                .iter()
                .any(|reason| reason == "rustdoc_output_tree_unavailable")),
            "Cargo does not enumerate the complete HTML tree, so reuse must remain disabled: {record}"
        );
        let encoded = serde_json::to_string(record)?;
        let canonical_workspace = fs::canonicalize(&workspace.path)?;
        for root in [&workspace.path, &canonical_workspace] {
            assert!(
                !encoded.contains(root.to_string_lossy().as_ref()),
                "captured compiler observation must not retain checkout root '{}': {record}",
                root.display()
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}
