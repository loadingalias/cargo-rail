//! Exact-toolchain compiler-fact companion.
//!
//! This crate is deliberately outside cargo-rail's workspace and package. It
//! is built only as a release artifact with crate-scoped bootstrap authority,
//! then authenticated and distributed beside the stable cargo-rail binary.

#![feature(rustc_private)]
#![forbid(unsafe_code)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint_defs;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface;
use rustc_middle::ty::TyCtxt;

#[path = "../../../src/compiler/fact_protocol.rs"]
mod fact_protocol;

mod collection;
mod output;

const PROTOCOL_VERSION_ARGUMENT: &str = "--cargo-rail-fact-protocol-version";

struct FactCallbacks {
  invocation: fact_protocol::CompilerFactInvocation,
}

impl Callbacks for FactCallbacks {
  fn after_analysis<'tcx>(&mut self, _compiler: &interface::Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
    let result =
      collection::collect(tcx, &self.invocation).and_then(|object| output::publish(&self.invocation, object));
    if let Err(error) = result {
      tcx.dcx().fatal(format!(
        "cargo-rail fact driver could not publish complete compiler facts: {error}"
      ));
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
  if arguments.len() < 2 {
    eprintln!("cargo-rail fact driver: this internal executable must be invoked as a rustc wrapper");
    return ExitCode::FAILURE;
  }

  let invocation_path = match std::env::var_os(fact_protocol::COMPILER_FACT_INVOCATION_ENV) {
    Some(path) => PathBuf::from(path),
    None => {
      eprintln!("cargo-rail fact driver: missing per-invocation capability");
      return ExitCode::FAILURE;
    }
  };
  let invocation = match load_invocation(&invocation_path) {
    Ok(invocation) => invocation,
    Err(error) => {
      eprintln!("cargo-rail fact driver: {error}");
      return ExitCode::FAILURE;
    }
  };

  // Cargo's workspace-wrapper argv is `[driver, rustc, rustc-args...]`.
  // rustc_driver expects `[rustc, rustc-args...]` and discards argv[0].
  arguments.remove(1);
  let mut callbacks = FactCallbacks { invocation };
  rustc_driver::catch_with_exit_code(move || rustc_driver::run_compiler(&arguments, &mut callbacks))
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
