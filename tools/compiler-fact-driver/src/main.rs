//! Exact-toolchain compiler-fact companion.
//!
//! This crate is deliberately outside cargo-rail's workspace and package. It
//! is built only as a release artifact with crate-scoped bootstrap authority,
//! then authenticated and distributed beside the stable cargo-rail binary.

#![feature(rustc_private)]
#![forbid(unsafe_code)]

extern crate rustc_codegen_ssa;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint_defs;
extern crate rustc_metadata;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;
extern crate tracing;
extern crate tracing_subscriber;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface;
use rustc_middle::ty::TyCtxt;

#[path = "../../../src/compiler/fact_protocol.rs"]
mod fact_protocol;
#[path = "../../../src/compiler/native_input_protocol.rs"]
mod native_input_protocol;

mod codegen;
mod collection;
mod native_inputs;
mod output;

const PROTOCOL_VERSION_ARGUMENT: &str = "--cargo-rail-fact-protocol-version";

struct CompilerCallbacks {
    fact_invocation: Option<fact_protocol::CompilerFactInvocation>,
    native_invocation: Option<native_input_protocol::NativeInputInvocation>,
    native_observation: Option<native_input_protocol::NativeInputObservation>,
    dependency_requests: Option<std::sync::Arc<std::sync::Mutex<native_inputs::DependencyRequests>>>,
    codegen: std::sync::Arc<std::sync::Mutex<native_input_protocol::NativeCodegenObservation>>,
}

impl Callbacks for CompilerCallbacks {
    fn config(&mut self, config: &mut interface::Config) {
        // Keep the compiler-owned defaults used by ordinary rustc, including
        // options that contribute to the emitted crate hash.
        rustc_driver::TimePassesCallbacks::default().config(config);
        if self.native_invocation.is_some() {
            codegen::configure(config, self.codegen.clone());
            self.dependency_requests = Some(native_inputs::observe_dependency_requests());
        }
        if let Some(directory) = self
            .native_invocation
            .as_ref()
            .and_then(|invocation| invocation.source_working_directory.as_deref())
            && native_inputs::configure_source_directory(config, directory).is_err()
        {
            self.native_invocation = None;
        }
    }

    fn after_analysis<'tcx>(&mut self, _compiler: &interface::Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        if let Some(invocation) = &self.fact_invocation {
            let result = collection::collect(tcx, invocation).and_then(|object| output::publish(invocation, object));
            if let Err(error) = result {
                tcx.dcx().fatal(format!(
                    "cargo-rail fact driver could not publish complete compiler facts: {error}"
                ));
            }
        }
        if let Some(invocation) = &self.native_invocation {
            // Cache observation cannot turn a successful ordinary compilation
            // into failure. The wrapper requires a complete result to publish.
            self.native_observation = self
                .dependency_requests
                .as_ref()
                .and_then(|requests| native_inputs::collect(tcx, invocation, requests).ok());
        }
        Compilation::Continue
    }
}

fn main() -> ExitCode {
    let Ok(mut arguments): Result<Vec<String>, _> = std::env::args_os().map(std::ffi::OsString::into_string).collect()
    else {
        eprintln!("cargo-rail fact driver: command-line arguments must be valid UTF-8");
        return ExitCode::FAILURE;
    };
    if arguments.len() == 2
        && arguments
            .get(1)
            .is_some_and(|argument| argument == PROTOCOL_VERSION_ARGUMENT)
    {
        println!("{}", fact_protocol::COMPILER_FACT_PROTOCOL_VERSION);
        return ExitCode::SUCCESS;
    }
    if arguments.len() == 2
        && arguments
            .get(1)
            .is_some_and(|argument| argument == native_input_protocol::NATIVE_INPUT_PROTOCOL_VERSION_ARGUMENT)
    {
        println!("{}", native_input_protocol::NATIVE_INPUT_PROTOCOL_VERSION);
        return ExitCode::SUCCESS;
    }
    if arguments.len() >= 2 && nested_llvm_assembly(&arguments[1..]) {
        // A backend re-enters current_exe for assembly. This child is not the
        // parent compiler invocation and cannot publish its input or fact records.
        let mut callbacks = CompilerCallbacks {
            fact_invocation: None,
            native_invocation: None,
            native_observation: None,
            dependency_requests: None,
            codegen: std::sync::Arc::new(std::sync::Mutex::new(
                native_input_protocol::NativeCodegenObservation::NotRun,
            )),
        };
        return rustc_driver::catch_with_exit_code(move || rustc_driver::run_compiler(&arguments, &mut callbacks));
    }
    if arguments.len() < 2 {
        eprintln!("cargo-rail fact driver: this internal executable must be invoked as a rustc wrapper");
        return ExitCode::FAILURE;
    }

    let native_path = if arguments
        .get(1)
        .is_some_and(|argument| argument == native_input_protocol::NATIVE_INPUT_INVOCATION_ARGUMENT)
    {
        if arguments.len() < 4 {
            eprintln!("cargo-rail fact driver: native invocation requires a capability and rustc program");
            return ExitCode::FAILURE;
        }
        let path = PathBuf::from(&arguments[2]);
        arguments.drain(1..3);
        Some(path)
    } else {
        None
    };
    let native_invocation = native_path
        .as_ref()
        .and_then(|path| native_inputs::load_invocation(path, &arguments[2..]).ok());
    let fact_invocation = match std::env::var_os(fact_protocol::COMPILER_FACT_INVOCATION_ENV) {
        Some(path) => match load_invocation(Path::new(&path)) {
            Ok(invocation) => Some(invocation),
            Err(error) => {
                eprintln!("cargo-rail fact driver: {error}");
                return ExitCode::FAILURE;
            }
        },
        None if native_path.is_some() => None,
        None => {
            eprintln!("cargo-rail fact driver: missing per-invocation capability");
            return ExitCode::FAILURE;
        }
    };

    // Cargo's workspace-wrapper argv is `[driver, rustc, rustc-args...]`.
    // rustc_driver expects `[rustc, rustc-args...]` and discards argv[0].
    arguments.remove(1);
    let mut callbacks = CompilerCallbacks {
        fact_invocation,
        native_invocation,
        native_observation: None,
        dependency_requests: None,
        codegen: std::sync::Arc::new(std::sync::Mutex::new(
            native_input_protocol::NativeCodegenObservation::NotRun,
        )),
    };
    rustc_driver::catch_with_exit_code(move || {
        rustc_driver::run_compiler(&arguments, &mut callbacks);
        if let (Some(invocation), Some(mut observation)) = (callbacks.native_invocation, callbacks.native_observation)
            && let Ok(codegen) = callbacks.codegen.lock()
        {
            observation.codegen = *codegen;
            let _ = native_inputs::publish(&invocation, &observation);
        }
    })
}

fn nested_llvm_assembly(arguments: &[String]) -> bool {
    matches!(arguments,
        [target_flag, target, crate_type_flag, crate_type, emit_flag, emit, output_flag, output,
         input, style, backend, unstable]
        if target_flag == "--target" && !target.is_empty()
            && crate_type_flag == "--crate-type" && crate_type == "staticlib"
            && emit_flag == "--emit" && emit == "obj"
            && output_flag == "-o" && !output.is_empty()
            && input == "-" && style == "-Abad_asm_style"
            && backend == "-Zcodegen-backend=llvm" && unstable == "-Zunstable-options")
}

fn load_invocation(path: &Path) -> Result<fact_protocol::CompilerFactInvocation, String> {
    const MAX_INVOCATION_BYTES: u64 = 64 * 1024;

    let metadata = fs::symlink_metadata(path).map_err(|error| format!("inspect per-invocation capability: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_INVOCATION_BYTES
    {
        return Err("per-invocation capability is not a bounded real file".to_string());
    }
    let bytes = fs::read(path).map_err(|error| format!("read per-invocation capability: {error}"))?;
    if bytes.len() as u64 != metadata.len() {
        return Err("per-invocation capability changed while it was read".to_string());
    }
    let invocation: fact_protocol::CompilerFactInvocation =
        serde_json::from_slice(&bytes).map_err(|error| format!("decode per-invocation capability: {error}"))?;
    if invocation.version != fact_protocol::COMPILER_FACT_PROTOCOL_VERSION
        || invocation.required_coverage.is_empty()
        || !Path::new(&invocation.observation_directory).is_absolute()
        || !Path::new(&invocation.source_root).is_absolute()
        || invocation.generated_roots.is_empty()
        || invocation.generated_roots.windows(2).any(|pair| pair[0] >= pair[1])
        || invocation
            .generated_roots
            .iter()
            .any(|root| !Path::new(root).is_absolute())
    {
        return Err("per-invocation capability has incompatible authority".to_string());
    }
    let canonical =
        serde_json::to_vec(&invocation).map_err(|error| format!("encode per-invocation capability: {error}"))?;
    if canonical != bytes {
        return Err("per-invocation capability is not canonical JSON".to_string());
    }
    Ok(invocation)
}
