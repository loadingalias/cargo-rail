//! The matched compiler observes native inputs without changing compilation.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "../../../src/compiler/fact_protocol.rs"]
mod fact_protocol;
#[path = "../../../src/compiler/native_input_protocol.rs"]
mod native_input_protocol;

use native_input_protocol::{
    NATIVE_INPUT_INVOCATION_ARGUMENT, NATIVE_INPUT_INVOCATION_ENV, NATIVE_INPUT_PROTOCOL_VERSION,
    NATIVE_INPUT_PROTOCOL_VERSION_ARGUMENT, NativeAssemblyObservation, NativeCodegenObservation, NativeInputInvocation,
    NativeInputObservation, native_invocation_digest,
};

struct Driver {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    rustc: PathBuf,
    sysroot: PathBuf,
}

impl Driver {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("driver fixture");
        let root = temporary.path().canonicalize().expect("fixture root");
        let rustc_output = Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .expect("selected rustc");
        assert!(rustc_output.status.success());
        let rustc = PathBuf::from(String::from_utf8(rustc_output.stdout).unwrap().trim());
        let sysroot_output = Command::new(&rustc)
            .args(["--print", "sysroot"])
            .output()
            .expect("rustc sysroot");
        assert!(sysroot_output.status.success());
        let sysroot = PathBuf::from(String::from_utf8(sysroot_output.stdout).unwrap().trim());
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(sysroot.join("lib"), root.join("lib")).expect("runtime libraries");
        let program = bin.join(format!("cargo-rail-fact-driver{}", std::env::consts::EXE_SUFFIX));
        fs::copy(env!("CARGO_BIN_EXE_cargo-rail-fact-driver"), &program).expect("stage driver");
        Self {
            _temporary: temporary,
            root,
            program,
            rustc,
            sysroot,
        }
    }

    fn command(&self, driver: bool) -> Command {
        let mut command = Command::new(if driver { &self.program } else { &self.rustc });
        command
            .current_dir(&self.root)
            .env_remove("CARGO_RAIL_COMPILER_FACT_INVOCATION")
            .env_remove(NATIVE_INPUT_INVOCATION_ENV);
        #[cfg(windows)]
        {
            let mut paths = vec![self.sysroot.join("bin"), self.sysroot.join("lib")];
            if let Some(inherited) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&inherited));
            }
            command.env("PATH", std::env::join_paths(paths).unwrap());
        }
        #[cfg(not(windows))]
        let _ = &self.sysroot;
        command
    }

    fn run(&self, name: &str, args: &[String]) -> (Output, Option<NativeInputObservation>) {
        let request_path = self.root.join(format!("{name}-request.json"));
        let result_path = self.root.join(format!("{name}-result.json"));
        let request = NativeInputInvocation {
            version: NATIVE_INPUT_PROTOCOL_VERSION,
            source_working_directory: None,
            nonce: "1".repeat(64),
            action_identity: name.into(),
            invocation_digest: native_invocation_digest(args, &self.root).unwrap(),
            result_path: result_path.to_str().unwrap().into(),
        };
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        let output = self
            .command(true)
            .arg(NATIVE_INPUT_INVOCATION_ARGUMENT)
            .arg(&request_path)
            .arg(&self.rustc)
            .args(args)
            .output()
            .expect("matched compiler");
        let observation = fs::read(result_path)
            .ok()
            .map(|bytes| NativeInputObservation::decode(&bytes, &request).expect("bound native observation"));
        (output, observation)
    }

    fn compile_dependency(&self, name: &str, source: &str, extra: &[String]) -> PathBuf {
        let input = self.root.join(format!("{name}.rs"));
        let output = self.root.join(format!("lib{name}.rlib"));
        fs::write(&input, source).unwrap();
        let result = self
            .command(false)
            .arg(input)
            .args(["--crate-type=rlib", "--edition=2024"])
            .arg("-o")
            .arg(&output)
            .args(extra)
            .output()
            .unwrap();
        assert_success(&result);
        output
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "compiler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn arguments(input: &str, output: &str, emit: &str) -> Vec<String> {
    [
        input,
        "--crate-type=rlib",
        "--edition=2024",
        "--crate-name=consumer",
        emit,
        "-o",
        output,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn relocated_execution_preserves_unremapped_source_identity_and_reads_only_staged_sources() {
    let driver = Driver::new();
    let original = driver.root.join("original");
    let staged = driver.root.join("staged");
    for root in [&original, &staged] {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "mod child; pub const VALUE: u8 = child::VALUE; pub const FILE: &str = file!();\n",
        )
        .unwrap();
        fs::write(root.join("src/child.rs"), "pub const VALUE: u8 = 42;\n").unwrap();
    }
    let args = [
        "src/lib.rs",
        "--crate-name=relocated",
        "--crate-type=rlib",
        "--edition=2024",
        "--emit=metadata,link",
        "--out-dir=out",
        "-Cmetadata=0123456789abcdef",
    ]
    .map(str::to_string);
    let baseline = driver
        .command(false)
        .current_dir(&original)
        .env("CARGO_MANIFEST_DIR", &original)
        .args(&args)
        .output()
        .unwrap();
    assert_success(&baseline);
    fs::remove_dir_all(original.join("src")).unwrap();
    let request = NativeInputInvocation {
        version: NATIVE_INPUT_PROTOCOL_VERSION,
        nonce: "1".repeat(64),
        action_identity: "relocated-source".into(),
        invocation_digest: native_invocation_digest(&args, &staged).unwrap(),
        result_path: staged.join("result.json").to_str().unwrap().into(),
        source_working_directory: Some(original.to_str().unwrap().into()),
    };
    let capability = staged.join("request.json");
    fs::write(&capability, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = driver
        .command(true)
        .current_dir(&staged)
        .env("CARGO_MANIFEST_DIR", &original)
        .arg(NATIVE_INPUT_INVOCATION_ARGUMENT)
        .arg(capability)
        .arg(&driver.rustc)
        .args(&args)
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(output.stdout, baseline.stdout);
    assert_eq!(output.stderr, baseline.stderr);
    for name in ["librelocated.rmeta", "librelocated.rlib"] {
        assert_eq!(
            fs::read(staged.join("out").join(name)).unwrap(),
            fs::read(original.join("out").join(name)).unwrap(),
            "{name}"
        );
    }
    let observed = NativeInputObservation::decode(&fs::read(&request.result_path).unwrap(), &request).unwrap();
    assert_eq!(observed.assembly, NativeAssemblyObservation::Absent);
    assert_eq!(observed.codegen, NativeCodegenObservation::Llvm);
}

#[test]
fn native_observation_binds_transitive_crates_and_preserves_compiler_outputs() {
    let driver = Driver::new();
    let a = driver.compile_dependency("dependency_a", "pub fn value() -> u8 { 41 }", &[]);
    let b = driver.compile_dependency(
        "dependency_b",
        "pub fn value() -> u8 { dependency_a::value() }",
        &["--extern".into(), format!("dependency_a={}", a.display())],
    );
    fs::write(
        driver.root.join("consumer.rs"),
        "pub fn value() -> u8 { dependency_b::value() }\n",
    )
    .unwrap();
    let mut args = arguments("consumer.rs", "consumer.rmeta", "--emit=metadata");
    args.extend([
        "--extern".into(),
        format!("dependency_b={}", b.display()),
        format!("-Ldependency={}", driver.root.display()),
    ]);
    fs::write(driver.root.join("libdependency_b-shadow.rlib"), b"not an archive").unwrap();
    let baseline = driver.command(false).args(&args).output().unwrap();
    assert_success(&baseline);
    let baseline_bytes = fs::read(driver.root.join("consumer.rmeta")).unwrap();
    fs::remove_file(driver.root.join("consumer.rmeta")).unwrap();
    let (observed, observation) = driver.run("transitive", &args);
    assert_success(&observed);
    assert_eq!(observed.stdout, baseline.stdout);
    assert_eq!(observed.stderr, baseline.stderr);
    assert_eq!(fs::read(driver.root.join("consumer.rmeta")).unwrap(), baseline_bytes);
    let observation = observation.expect("complete native observation");
    assert_eq!(observation.assembly, NativeAssemblyObservation::NoCodegen);
    let search = observation
        .searches
        .iter()
        .find(|search| search.directory == driver.root.to_str().unwrap())
        .expect("transitive library search");
    assert!(
        search
            .patterns
            .iter()
            .any(|pattern| pattern.matches("libdependency_a.rlib"))
    );
    assert!(
        !search
            .patterns
            .iter()
            .any(|pattern| pattern.matches("libdependency_b-shadow.rlib"))
    );
    for (name, file) in [("dependency_a", a), ("dependency_b", b)] {
        assert_eq!(
            observation
                .crates
                .iter()
                .find(|source| source.name == name)
                .expect("selected transitive crate")
                .files,
            [file.to_str().unwrap()]
        );
    }

    driver.compile_dependency("dependency_a", "pub fn value() -> u8 { 42 }", &[]);
    let baseline = driver.command(false).args(&args).output().unwrap();
    let (observed, observation) = driver.run("changed-transitive", &args);
    assert!(!baseline.status.success());
    assert!(String::from_utf8_lossy(&baseline.stderr).contains("E0460"));
    assert_eq!(observed.status.code(), baseline.status.code());
    assert_eq!(observed.stderr, baseline.stderr);
    assert_eq!(observed.stdout, baseline.stdout);
    assert_eq!(observation, None);
}

#[test]
fn direct_sysroot_load_does_not_search_dependency_only_directories() {
    let driver = Driver::new();
    fs::write(
        driver.root.join("consumer.rs"),
        "extern crate proc_macro; pub fn value() -> u8 { 7 }\n",
    )
    .unwrap();
    fs::write(driver.root.join("libproc_macro-shadow.rlib"), b"not an archive").unwrap();
    for (kind, expected) in [("dependency", false), ("crate", true)] {
        let mut args = arguments("consumer.rs", "consumer.rmeta", "--emit=metadata");
        args.push(format!("-L{kind}={}", driver.root.display()));
        let baseline = driver.command(false).args(&args).output().unwrap();
        assert_success(&baseline);
        let bytes = fs::read(driver.root.join("consumer.rmeta")).unwrap();
        let (actual, observation) = driver.run(kind, &args);
        assert_success(&actual);
        assert_eq!(actual.stdout, baseline.stdout);
        assert_eq!(actual.stderr, baseline.stderr);
        assert_eq!(fs::read(driver.root.join("consumer.rmeta")).unwrap(), bytes);
        let observation = observation.expect("complete observation");
        assert!(observation.crates.iter().any(|source| source.name == "proc_macro"));
        let searches = observation
            .searches
            .iter()
            .filter(|search| search.directory == driver.root.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            searches.iter().any(|search| search
                .patterns
                .iter()
                .any(|pattern| pattern.matches("libproc_macro-shadow.rlib"))),
            expected
        );
        assert_eq!(
            searches
                .iter()
                .any(|search| search.files.contains(&"libproc_macro-shadow.rlib".into())),
            expected
        );
    }
}

#[test]
fn linked_proc_macro_retains_complete_native_inputs() {
    let driver = Driver::new();
    fs::write(driver.root.join("consumer.rs"),
        "extern crate proc_macro; #[proc_macro] pub fn identity(input: proc_macro::TokenStream) -> proc_macro::TokenStream { input }\n").unwrap();
    let output = format!(
        "{}consumer{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let args = vec![
        "consumer.rs".into(),
        "--crate-type=proc-macro".into(),
        "--edition=2024".into(),
        "--emit=link".into(),
        "-Cprefer-dynamic".into(),
        "--extern".into(),
        "proc_macro".into(),
        format!("-Ldependency={}", driver.root.display()),
        "-o".into(),
        output.clone(),
    ];
    let baseline = driver.command(false).args(&args).output().unwrap();
    assert_success(&baseline);
    let bytes = fs::read(driver.root.join(&output)).unwrap();
    let (actual, observation) = driver.run("linked-macro", &args);
    assert_success(&actual);
    assert_eq!(actual.stdout, baseline.stdout);
    assert_eq!(actual.stderr, baseline.stderr);
    assert_eq!(fs::read(driver.root.join(output)).unwrap(), bytes);
    let observation = observation.expect("complete linked proc-macro inputs");
    assert!(observation.crates.iter().any(|source| source.name == "proc_macro"));
    assert_eq!(observation.assembly, NativeAssemblyObservation::Absent);
}

#[test]
fn transitive_search_uses_requested_suffix_and_retains_renamed_fallback() {
    let driver = Driver::new();
    let a = driver.compile_dependency(
        "dependency_a",
        "pub fn value() -> u8 { 7 }",
        &["-Cextra-filename=-requested".into()],
    );
    let requested = driver.root.join("libdependency_a-requested.rlib");
    fs::rename(a, &requested).unwrap();
    let b = driver.compile_dependency(
        "dependency_b",
        "pub fn value() -> u8 { dependency_a::value() }",
        &["--extern".into(), format!("dependency_a={}", requested.display())],
    );
    // extra-filename does not change the crate hash. The parent still requests
    // the original suffix even though the selected metadata now declares another.
    let rebuilt = driver.compile_dependency(
        "dependency_a",
        "pub fn value() -> u8 { 7 }",
        &["-Cextra-filename=-different".into()],
    );
    fs::remove_file(&requested).unwrap();
    fs::rename(rebuilt, &requested).unwrap();
    fs::write(
        driver.root.join("consumer.rs"),
        "pub fn value() -> u8 { dependency_b::value() }\n",
    )
    .unwrap();
    let mut args = arguments("consumer.rs", "consumer.rmeta", "--emit=metadata");
    args.extend([
        "--extern".into(),
        format!("dependency_b={}", b.display()),
        format!("-Ldependency={}", driver.root.display()),
    ]);
    fs::write(
        driver.root.join("libdependency_a-unrelated.rlib"),
        b"unrelated concurrent artifact",
    )
    .unwrap();
    for (phase, filename, broad) in [
        ("requested", "libdependency_a-requested.rlib", false),
        ("fallback", "libdependency_a-renamed.rlib", true),
    ] {
        if broad {
            fs::rename(&requested, driver.root.join(filename)).unwrap();
        }
        let baseline = driver.command(false).args(&args).output().unwrap();
        assert_success(&baseline);
        let bytes = fs::read(driver.root.join("consumer.rmeta")).unwrap();
        fs::remove_file(driver.root.join("consumer.rmeta")).unwrap();
        let (actual, observation) = driver.run(phase, &args);
        assert_success(&actual);
        assert_eq!(actual.stdout, baseline.stdout);
        assert_eq!(actual.stderr, baseline.stderr);
        assert_eq!(fs::read(driver.root.join("consumer.rmeta")).unwrap(), bytes);
        let observation = observation.expect("complete search observation");
        let search = observation
            .searches
            .iter()
            .find(|search| search.directory == driver.root.to_str().unwrap())
            .unwrap();
        assert!(search.patterns.iter().any(|pattern| pattern.matches(filename)));
        assert_eq!(
            search
                .patterns
                .iter()
                .any(|pattern| pattern.matches("libdependency_a-unrelated.rlib")),
            broad
        );
        assert_eq!(search.files.contains(&"libdependency_a-unrelated.rlib".into()), broad);
    }
}

#[test]
fn native_observation_distinguishes_metadata_and_actual_monomorphized_assembly() {
    let driver = Driver::new();
    for (name, source, expected) in [
        (
            "plain",
            "pub fn value() -> u8 { 42 }",
            NativeAssemblyObservation::Absent,
        ),
        (
            "global",
            "core::arch::global_asm!(\".byte 0\");",
            NativeAssemblyObservation::Present,
        ),
        (
            "macro",
            "macro_rules! make { () => { pub fn value() { unsafe { core::arch::asm!(\"\"); } } } } make!();",
            NativeAssemblyObservation::Present,
        ),
        (
            "naked",
            "#[unsafe(naked)] pub unsafe extern \"C\" fn value() { core::arch::naked_asm!(\"ret\"); }",
            NativeAssemblyObservation::Present,
        ),
    ] {
        let input = format!("{name}.rs");
        fs::write(driver.root.join(&input), source).unwrap();
        let args = arguments(&input, &format!("{name}.rlib"), "--emit=link");
        let (output, observation) = driver.run(name, &args);
        assert_success(&output);
        assert_eq!(observation.expect("codegen observation").assembly, expected, "{name}");
    }
    let args = arguments("global.rs", "global.rmeta", "--emit=metadata");
    let (output, observation) = driver.run("global-metadata", &args);
    assert_success(&output);
    assert_eq!(observation.unwrap().assembly, NativeAssemblyObservation::NoCodegen);

    let library = driver.compile_dependency(
        "generic_assembly",
        "pub fn value<T>(value: T) -> T { unsafe { core::arch::asm!(\"\"); } value }",
        &[],
    );
    fs::write(
        driver.root.join("imported.rs"),
        "pub fn value() -> u8 { generic_assembly::value(42) }",
    )
    .unwrap();
    let mut args = arguments("imported.rs", "imported.rlib", "--emit=link");
    args.extend(["--extern".into(), format!("generic_assembly={}", library.display())]);
    let (output, observation) = driver.run("imported", &args);
    assert_success(&output);
    assert_eq!(observation.unwrap().assembly, NativeAssemblyObservation::Present);
}

#[test]
fn nested_assembly_role_rejects_linking_and_recursive_backend_selection() {
    let driver = Driver::new();
    let output_path = driver.root.join("untouched-output");
    fs::write(&output_path, b"original output").unwrap();
    let mut args = vec![
        "--target".to_string(),
        "aarch64-apple-darwin".to_string(),
        "--crate-type".to_string(),
        "staticlib".to_string(),
        "--emit".to_string(),
        "obj".to_string(),
        "-o".to_string(),
        output_path.to_str().unwrap().to_string(),
        "-".to_string(),
        "-Abad_asm_style".to_string(),
        "-Zcodegen-backend=llvm".to_string(),
        "-Zunstable-options".to_string(),
    ];
    for (index, replacement) in [(5, "link"), (10, "-Zcodegen-backend=cranelift")] {
        let original = std::mem::replace(&mut args[index], replacement.to_string());
        let output = driver.command(true).args(&args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"cargo-rail fact driver: missing per-invocation capability\n"
        );
        assert_eq!(fs::read(&output_path).unwrap(), b"original output");
        args[index] = original;
    }
}

#[test]
#[ignore = "requires a matched nightly with the Cranelift component; run the backend contract explicitly"]
fn cranelift_nested_compiler_preserves_emitted_library_and_parent_observation() {
    let driver = Driver::new();
    fs::write(driver.root.join("assembly.rs"), "core::arch::global_asm!(\".byte 0\");").unwrap();
    let outputs = driver.root.join("outputs");
    fs::create_dir(&outputs).unwrap();
    let mut args = arguments("assembly.rs", "outputs/assembly.rlib", "--emit=link");
    args.push("-Zcodegen-backend=cranelift".into());
    let baseline = driver.command(false).args(&args).output().unwrap();
    assert_success(&baseline);
    let artifact = outputs.join("assembly.rlib");
    let bytes = fs::read(&artifact).unwrap();
    let permissions = fs::metadata(&artifact).unwrap().permissions();
    fs::remove_file(&artifact).unwrap();
    let (observed, observation) = driver.run("cranelift-assembly", &args);
    assert_eq!(observed.status.code(), baseline.status.code());
    assert_eq!(observed.stdout, baseline.stdout);
    assert_eq!(observed.stderr, baseline.stderr);
    assert_eq!(fs::read(&artifact).unwrap(), bytes);
    assert_eq!(fs::metadata(&artifact).unwrap().permissions(), permissions);
    assert_eq!(fs::read_dir(&outputs).unwrap().count(), 1);
    let observation = observation.expect("parent input authority");
    assert_eq!(observation.assembly, NativeAssemblyObservation::Present);
    assert_eq!(
        observation.codegen,
        NativeCodegenObservation::Cranelift {
            separate_assembly: true
        }
    );

    fs::write(driver.root.join("assembly.rs"), "pub fn value() -> u8 { 7 }").unwrap();
    fs::remove_file(&artifact).unwrap();
    let baseline = driver.command(false).args(&args).output().unwrap();
    assert_success(&baseline);
    let bytes = fs::read(&artifact).unwrap();
    fs::remove_file(&artifact).unwrap();
    let (observed, observation) = driver.run("cranelift-no-assembly", &args);
    assert_eq!(observed.status.code(), baseline.status.code());
    assert_eq!(observed.stdout, baseline.stdout);
    assert_eq!(observed.stderr, baseline.stderr);
    assert_eq!(fs::read(&artifact).unwrap(), bytes);
    assert_eq!(fs::metadata(&artifact).unwrap().permissions(), permissions);
    assert_eq!(fs::read_dir(&outputs).unwrap().count(), 1);
    let observation = observation.expect("completed code generation");
    assert_eq!(observation.assembly, NativeAssemblyObservation::Absent);
    assert_eq!(
        observation.codegen,
        NativeCodegenObservation::Cranelift {
            separate_assembly: false
        }
    );
}

#[test]
fn native_observation_declines_unobserved_imported_lto() {
    let driver = Driver::new();
    fs::write(driver.root.join("plain.rs"), "pub fn value() -> u8 { 42 }").unwrap();
    let mut args = arguments("plain.rs", "plain.rlib", "--emit=link");
    args.push("-Clto=thin".into());
    let (output, observation) = driver.run("rlib-lto", &args);
    assert_success(&output);
    assert_eq!(observation.unwrap().assembly, NativeAssemblyObservation::Absent);
    args[1] = "--crate-type=staticlib".into();
    args[6] = "libplain.a".into();
    let (output, observation) = driver.run("staticlib-lto", &args);
    assert_success(&output);
    assert_eq!(
        observation.unwrap().assembly,
        NativeAssemblyObservation::ImportedLtoUnobserved
    );
}

#[test]
fn invalid_native_capability_preserves_diagnostics_and_does_not_publish() {
    let driver = Driver::new();
    let protocol = Command::new(&driver.program)
        .arg(NATIVE_INPUT_PROTOCOL_VERSION_ARGUMENT)
        .output()
        .unwrap();
    assert_success(&protocol);
    assert_eq!(protocol.stdout, b"2\n");
    fs::write(
        driver.root.join("warning.rs"),
        "fn unused() {}\npub fn value() -> u8 { 42 }\nconst _: () = assert!(option_env!(\"CARGO_RAIL_NATIVE_INPUT_INVOCATION\").is_none());",
    )
    .unwrap();
    let args = arguments("warning.rs", "warning.rmeta", "--emit=metadata");
    let baseline = driver.command(false).args(&args).output().unwrap();
    assert_success(&baseline);
    assert!(!baseline.stderr.is_empty());
    let result = driver
        .command(true)
        .arg(NATIVE_INPUT_INVOCATION_ARGUMENT)
        .arg(driver.root.join("missing.json"))
        .arg(&driver.rustc)
        .args(&args)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), baseline.status.code());
    assert_eq!(result.stdout, baseline.stdout);
    assert_eq!(result.stderr, baseline.stderr);
    assert!(!driver.root.join("missing-result.json").exists());
}

#[test]
fn native_and_analysis_observations_share_one_unchanged_compilation() {
    let driver = Driver::new();
    fs::write(driver.root.join("combined.rs"), "pub fn value() -> u8 { 42 }\n").unwrap();
    let args = arguments("combined.rs", "combined.rmeta", "--emit=metadata");
    let facts = driver.root.join("facts");
    fs::create_dir(&facts).unwrap();
    let source_root = driver.root.to_str().unwrap();
    let identity = |prefix: &str| format!("{prefix}{}", "1".repeat(64));
    let request: fact_protocol::CompilerFactInvocation = serde_json::from_value(serde_json::json!({
        "version": fact_protocol::COMPILER_FACT_PROTOCOL_VERSION,
        "observation_directory": facts,
        "source_root": source_root,
        "generated_roots": [source_root],
        "run_authority": {"run_identity": identity("compiler-fact-run-v1-sha256-"), "view_identity": identity("compiler-fact-view-v1-sha256-")},
        "producer_authority": {"compiler_identity": identity("compiler-fact-compiler-v1-sha256-"), "driver_identity": identity("compiler-fact-driver-v1-sha256-")},
        "unit": {
            "identity": identity("compiler-fact-unit-v1-sha256-"),
            "invocation_identity": identity("compiler-fact-invocation-v1-sha256-"),
            "package": {"name":"consumer", "version":"0.0.0", "source":null},
            "cargo_target":"consumer", "crate_name":"consumer", "target_kind":{"kind":"library"},
            "domain":"production", "role":"target", "platform":"selected-compiler-host", "features":[], "cfg":[]
        },
        "required_coverage":["definitions"]
    })).unwrap();
    let fact_request = driver.root.join("fact-request.json");
    fs::write(&fact_request, serde_json::to_vec(&request).unwrap()).unwrap();
    let baseline = driver
        .command(true)
        .arg(&driver.rustc)
        .args(&args)
        .env(fact_protocol::COMPILER_FACT_INVOCATION_ENV, &fact_request)
        .output()
        .unwrap();
    assert_success(&baseline);
    let baseline_bytes = fs::read(driver.root.join("combined.rmeta")).unwrap();
    let native_request = NativeInputInvocation {
        version: NATIVE_INPUT_PROTOCOL_VERSION,
        source_working_directory: None,
        nonce: "2".repeat(64),
        action_identity: "combined".into(),
        invocation_digest: native_invocation_digest(&args, &driver.root).unwrap(),
        result_path: driver.root.join("combined-result.json").to_str().unwrap().into(),
    };
    let native_request_path = driver.root.join("combined-request.json");
    fs::write(&native_request_path, serde_json::to_vec(&native_request).unwrap()).unwrap();
    let combined = driver
        .command(true)
        .arg(NATIVE_INPUT_INVOCATION_ARGUMENT)
        .arg(&native_request_path)
        .arg(&driver.rustc)
        .args(&args)
        .env(fact_protocol::COMPILER_FACT_INVOCATION_ENV, &fact_request)
        .output()
        .unwrap();
    assert_success(&combined);
    assert_eq!(combined.stdout, baseline.stdout);
    assert_eq!(combined.stderr, baseline.stderr);
    assert_eq!(fs::read(driver.root.join("combined.rmeta")).unwrap(), baseline_bytes);
    assert_eq!(
        String::from_utf8_lossy(&combined.stderr)
            .lines()
            .filter(|line| line.contains(fact_protocol::COMPILER_FACT_ANNOUNCEMENT_PREFIX))
            .count(),
        1
    );
    let native =
        NativeInputObservation::decode(&fs::read(&native_request.result_path).unwrap(), &native_request).unwrap();
    assert_eq!(native.assembly, NativeAssemblyObservation::NoCodegen);
    assert!(native.crates.iter().any(|source| source.name == "std"));
}
