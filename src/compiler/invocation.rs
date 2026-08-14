//! Exact pre-Clap compiler invocation boundary.
//!
//! Cargo invokes rustc mode through `RUSTC_WORKSPACE_WRAPPER`, so the unused
//! dependency lint is applied only to workspace members. Rustdoc observation
//! uses Cargo's selected `RUSTDOC` slot and retains that program as the inner
//! executable because Cargo has no rustdoc-wrapper setting.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Process-local control for the outer compiler cache boundary.
pub(crate) const CACHE_CONTROL_ENV: &str = "CARGO_RAIL_CACHE";
const BENCH_COVERAGE_CACHE_CONTROL: &str = "__cargo_rail_benchmark_coverage_v1";

/// Marker set when this executable is the cache-disabled outer compiler wrapper.
pub(crate) const CACHE_WRAPPER_MARKER: &str = "CARGO_RAIL_COMPILER_CACHE_WRAPPER";

/// Marker set by the diagnostics collector when this executable is acting as a
/// rustc workspace wrapper.
pub(crate) const WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTC_WRAPPER";

/// Existing workspace wrapper saved by the collector for transparent chaining.
pub(crate) const INNER_WRAPPER_ENV: &str = "CARGO_RAIL_INNER_WORKSPACE_WRAPPER";

/// Marker set when this executable is transparently proxying rustdoc.
pub(crate) const RUSTDOC_WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTDOC_WRAPPER";

/// Selected rustdoc executable retained behind the cargo-rail observation proxy.
pub(crate) const INNER_RUSTDOC_ENV: &str = "CARGO_RAIL_INNER_RUSTDOC";

/// Private directory where diagnostics wrappers publish immutable invocation evidence.
pub(crate) const OBSERVATION_DIRECTORY_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY";

/// Physical source root used only to normalize and revalidate observation paths.
pub(crate) const OBSERVATION_SOURCE_ROOT_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT";

/// Record invocations without enabling cargo-rail's workspace diagnostic lint.
pub(crate) const OBSERVATION_ONLY_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_ONLY";

/// Result of classifying the process before Clap or workspace acquisition.
#[doc(hidden)]
pub enum PreClapDispatch {
  /// No compiler role was requested; continue with the ordinary CLI.
  Cli,
  /// A compiler role ran and produced this process exit code.
  Exit(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvocationRole {
  LinkAdapter,
  DirectCache,
  MarkedCache,
  RustcObservation,
  RustdocObservation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InvocationSignals {
  link_adapter: bool,
  direct_cache: bool,
  marked_cache: bool,
  rustc_observation: bool,
  rustdoc_observation: bool,
}

impl InvocationSignals {
  fn classify(self) -> Result<Option<InvocationRole>, &'static str> {
    if self.link_adapter {
      if self.marked_cache || self.rustc_observation || self.rustdoc_observation {
        return Err("linker adapter received conflicting compiler role markers");
      }
      return Ok(Some(InvocationRole::LinkAdapter));
    }
    if self.direct_cache {
      if self.marked_cache || self.rustc_observation || self.rustdoc_observation {
        return Err("direct cache wrapper received conflicting compiler role markers");
      }
      return Ok(Some(InvocationRole::DirectCache));
    }
    if self.rustdoc_observation && (self.marked_cache || self.rustc_observation) {
      return Err("rustdoc proxy received conflicting compiler role markers");
    }
    if self.marked_cache {
      // The outer cache marker deliberately coexists with the rustc observation
      // marker. Cargo invokes the cache wrapper first, which removes only its
      // own authority before starting the observation wrapper.
      return Ok(Some(InvocationRole::MarkedCache));
    }
    if self.rustc_observation {
      return Ok(Some(InvocationRole::RustcObservation));
    }
    if self.rustdoc_observation {
      return Ok(Some(InvocationRole::RustdocObservation));
    }
    Ok(None)
  }
}

/// Exact program and argv selected by Cargo for one compiler process.
struct CompilerInvocation {
  program: OsString,
  arguments: Vec<OsString>,
}

impl CompilerInvocation {
  fn from_wrapper_arguments(context: &str) -> Result<Self, i32> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
      eprintln!("{context}: missing compiler executable");
      return Err(1);
    };
    Ok(Self {
      program,
      arguments: arguments.collect(),
    })
  }

  fn selected(program: OsString, arguments: Vec<OsString>) -> Self {
    Self { program, arguments }
  }

  fn command(&self) -> Command {
    let mut command = Command::new(&self.program);
    command.args(&self.arguments);
    command
  }

  fn compiler_selection(&self, observation_wrapper: bool) -> Option<(&std::ffi::OsStr, &[OsString])> {
    if observation_wrapper {
      self
        .arguments
        .split_first()
        .map(|(program, arguments)| (program.as_os_str(), arguments))
    } else {
      Some((self.program.as_os_str(), &self.arguments))
    }
  }
}

/// Compose Cargo's stable wrapper order: global wrapper, workspace wrapper, rustc.
pub(crate) fn rustc_command(
  rustc: &std::ffi::OsStr,
  rustc_wrapper: Option<&std::ffi::OsStr>,
  workspace_wrapper: Option<&std::ffi::OsStr>,
) -> Command {
  match (rustc_wrapper, workspace_wrapper) {
    (Some(wrapper), Some(workspace_wrapper)) => {
      let mut command = Command::new(wrapper);
      command.arg(workspace_wrapper).arg(rustc);
      command
    }
    (Some(wrapper), None) => {
      let mut command = Command::new(wrapper);
      command.arg(rustc);
      command
    }
    (None, Some(workspace_wrapper)) => {
      let mut command = Command::new(workspace_wrapper);
      command.arg(rustc);
      command
    }
    (None, None) => Command::new(rustc),
  }
}

/// Classify and run compiler roles before Clap or workspace acquisition.
#[must_use]
pub fn dispatch() -> PreClapDispatch {
  if let Some(exit_code) = crate::remote_cache::run_coordinator_if_requested() {
    return PreClapDispatch::Exit(exit_code);
  }
  let signals = InvocationSignals {
    link_adapter: std::env::var_os(crate::compiler::native_cache::APPLE_LINK_ADAPTER_ENV).is_some()
      || std::env::var_os(crate::compiler::native_cache::ELF_LINK_ADAPTER_ENV).is_some(),
    direct_cache: crate::compiler::native_cache::NativeCacheContext::is_direct_invocation(),
    marked_cache: std::env::var_os(CACHE_WRAPPER_MARKER).is_some(),
    rustc_observation: std::env::var_os(WRAPPER_MARKER).is_some(),
    rustdoc_observation: std::env::var_os(RUSTDOC_WRAPPER_MARKER).is_some(),
  };
  let role = match signals.classify() {
    Ok(role) => role,
    Err(error) => {
      eprintln!("cargo-rail compiler invocation: {error}");
      return PreClapDispatch::Exit(2);
    }
  };
  let Some(role) = role else {
    if is_unmarked_recursive_wrapper_invocation() {
      eprintln!("cargo-rail rustc wrapper: recursive cargo-rail rustc wrapper configuration");
      return PreClapDispatch::Exit(2);
    }
    return PreClapDispatch::Cli;
  };

  let exit_code = match role {
    InvocationRole::LinkAdapter => run_link_adapter(),
    InvocationRole::DirectCache => run_direct_cache(),
    InvocationRole::MarkedCache => run_cache(
      crate::compiler::native_cache::NativeCacheContext::from_environment(),
      signals.rustc_observation,
    ),
    InvocationRole::RustcObservation => run_rustc(),
    InvocationRole::RustdocObservation => run_rustdoc(),
  };
  PreClapDispatch::Exit(exit_code)
}

fn run_link_adapter() -> i32 {
  let elf = std::env::var_os(crate::compiler::native_cache::ELF_LINK_ADAPTER_ENV).is_some();
  let driver = if elf {
    std::env::var_os(crate::compiler::native_cache::ELF_LINK_DRIVER_ENV)
  } else {
    std::env::var_os(crate::compiler::native_cache::APPLE_LINK_DRIVER_ENV)
  };
  let Some(driver) = driver else {
    eprintln!("cargo-rail linker adapter: missing selected linker driver");
    return 1;
  };
  let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
  let mut command = Command::new(driver);
  let instrumented = if elf {
    crate::compiler::native_cache::configure_elf_link_adapter(&mut command, &arguments)
  } else {
    crate::compiler::native_cache::configure_apple_link_adapter(&mut command, &arguments)
  };
  if !instrumented {
    command.args(&arguments);
  }
  crate::compiler::native_cache::remove_private_environment(&mut command);
  if !instrumented {
    return run_transparently(command, "cargo-rail linker adapter");
  }
  match command.status() {
    Ok(status) => {
      if status.success() && !elf {
        let _ = crate::compiler::native_cache::finalize_apple_link_adapter();
      }
      compiler_status_code(status)
    }
    Err(error) => {
      eprintln!("cargo-rail linker adapter: failed to execute compiler: {error}");
      1
    }
  }
}

/// Run the compiler boundary from the dedicated wrapper executable.
#[must_use]
pub fn dispatch_required() -> i32 {
  match dispatch() {
    PreClapDispatch::Exit(exit_code) => exit_code,
    PreClapDispatch::Cli => {
      eprintln!("cargo-rail compiler cache wrapper: missing private invocation context");
      2
    }
  }
}

fn run_direct_cache() -> i32 {
  let cache_control = cache_control();
  if cache_control == CacheControl::Disabled {
    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
      eprintln!("cargo-rail compiler cache wrapper: missing compiler executable");
      return 1;
    };
    let mut command = Command::new(program);
    command.args(arguments);
    return run_transparently(command, "cargo-rail compiler cache wrapper");
  }
  let invocation = match CompilerInvocation::from_wrapper_arguments("cargo-rail compiler cache wrapper") {
    Ok(invocation) => invocation,
    Err(exit_code) => return exit_code,
  };
  if cache_control == CacheControl::BenchmarkCoverage {
    crate::compiler::native_cache::activate_benchmark_coverage();
  }
  if crate::compiler::native_cache::NativeCacheContext::is_direct_wrapper_program(&invocation.program) {
    eprintln!("cargo-rail compiler cache wrapper: recursive transparent wrapper configuration");
    return 2;
  }
  if let Some(reason) = cache_fast_bypass_reason(&invocation, false) {
    crate::compiler::native_cache::record_benchmark_coverage_bypass(&invocation.program, &invocation.arguments, reason);
    let mut command = invocation.command();
    if cache_control == CacheControl::BenchmarkCoverage {
      crate::compiler::native_cache::remove_cache_environment(&mut command);
    }
    return run_transparently(command, "cargo-rail compiler cache wrapper");
  }
  let context = crate::compiler::native_cache::NativeCacheContext::load_direct_invocation(
    &invocation.program,
    &invocation.arguments,
  );
  match context {
    Ok(context) => run_cache_invocation(invocation, Some(context)),
    Err(reason) => {
      crate::compiler::native_cache::record_benchmark_coverage_bypass(
        &invocation.program,
        &invocation.arguments,
        reason,
      );
      let mut command = invocation.command();
      if cache_control == CacheControl::BenchmarkCoverage {
        crate::compiler::native_cache::remove_cache_environment(&mut command);
      }
      run_transparently(command, "cargo-rail compiler cache wrapper")
    }
  }
}

fn run_cache(context: Option<crate::compiler::native_cache::NativeCacheContext>, observation_wrapper: bool) -> i32 {
  let invocation = match CompilerInvocation::from_wrapper_arguments("cargo-rail compiler cache wrapper") {
    Ok(invocation) => invocation,
    Err(exit_code) => return exit_code,
  };
  if cache_control() == CacheControl::Disabled
    || context.is_none()
    || cache_fast_bypass_reason(&invocation, observation_wrapper).is_some()
  {
    return run_cache_bypass(invocation);
  }
  run_cache_invocation(invocation, context)
}

fn cache_fast_bypass_reason(invocation: &CompilerInvocation, observation_wrapper: bool) -> Option<&'static str> {
  let Some((program, arguments)) = invocation.compiler_selection(observation_wrapper) else {
    return Some("compiler_argv_unavailable");
  };
  if !observation_wrapper && !direct_rustc_program_shape(program) {
    return Some("compiler_program_not_graduated");
  }
  crate::compiler::native_cache::fast_bypass_reason(program, arguments)
}

fn direct_rustc_program_shape(program: &std::ffi::OsStr) -> bool {
  Path::new(program)
    .file_stem()
    .and_then(std::ffi::OsStr::to_str)
    .is_some_and(|name| {
      name.eq_ignore_ascii_case("rustc") || name.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("rustc"))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheControl {
  Enabled,
  Disabled,
  BenchmarkCoverage,
}

fn cache_control() -> CacheControl {
  match std::env::var_os(CACHE_CONTROL_ENV).as_deref() {
    Some(value) if value == "off" => CacheControl::Disabled,
    Some(value) if value == BENCH_COVERAGE_CACHE_CONTROL => CacheControl::BenchmarkCoverage,
    _ => CacheControl::Enabled,
  }
}

fn run_cache_bypass(invocation: CompilerInvocation) -> i32 {
  let mut command = invocation.command();
  crate::compiler::native_cache::remove_cache_environment(&mut command);
  run_transparently(command, "cargo-rail compiler cache wrapper")
}

fn run_cache_invocation(
  invocation: CompilerInvocation,
  context: Option<crate::compiler::native_cache::NativeCacheContext>,
) -> i32 {
  if let Some(context) = context
    && let Err(error) = context.activate()
  {
    eprintln!("cargo-rail compiler cache wrapper: {error}");
    return 2;
  }
  let mut command = invocation.command();
  command.env_remove(CACHE_WRAPPER_MARKER).env_remove(CACHE_CONTROL_ENV);
  match crate::compiler::native_cache::configure_outer(&invocation.program, &invocation.arguments, &mut command) {
    crate::compiler::native_cache::OuterCacheAction::Hit(exit_code) => exit_code,
    crate::compiler::native_cache::OuterCacheAction::Store {
      recorder,
      capture,
      base_action_key,
      cache_bytes_read,
    } => crate::compiler::native_cache::run_and_store(
      command,
      recorder,
      capture,
      base_action_key,
      cache_bytes_read,
      "cargo-rail compiler cache wrapper",
    ),
    crate::compiler::native_cache::OuterCacheAction::OperationalFailure(error) => {
      crate::compiler::native_cache::record_active_failure();
      eprintln!("cargo-rail compiler cache wrapper: {error}");
      2
    }
    crate::compiler::native_cache::OuterCacheAction::Execute => {
      run_transparently(command, "cargo-rail compiler cache wrapper")
    }
  }
}

#[cfg(unix)]
fn run_transparently(mut command: Command, context: &str) -> i32 {
  use std::os::unix::process::CommandExt as _;

  let error = command.exec();
  eprintln!("{context}: failed to execute compiler: {error}");
  1
}

#[cfg(not(unix))]
fn run_transparently(mut command: Command, context: &str) -> i32 {
  match command.status() {
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      1
    }
  }
}

fn is_unmarked_recursive_wrapper_invocation() -> bool {
  std::env::var_os("CARGO").is_some()
    && std::env::args_os().nth(1).is_some_and(|program| {
      Path::new(&program)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("rustc"))
    })
}

fn run_rustc() -> i32 {
  let invocation = match CompilerInvocation::from_wrapper_arguments("cargo-rail rustc wrapper") {
    Ok(invocation) => invocation,
    Err(exit_code) => return exit_code,
  };
  let inner_wrapper = std::env::var_os(INNER_WRAPPER_ENV);
  if is_rustc_information_request(&invocation.arguments) {
    return run_rustc_bypass(invocation, inner_wrapper.as_deref());
  }
  let fact_session = match fact_session() {
    Ok(Some(session)) => session,
    Ok(None) => return run_rustc_bypass(invocation, inner_wrapper.as_deref()),
    Err(error) => {
      eprintln!("cargo-rail rustc wrapper: {error}");
      return 2;
    }
  };
  let recorder = match crate::compiler::observation::begin_invocation(
    fact_session.observation_directory(),
    fact_session.source_root(),
    &invocation.program,
    &invocation.arguments,
  ) {
    Ok(recorder) => recorder,
    Err(error) => {
      eprintln!("cargo-rail rustc wrapper: failed to begin compiler fact collection: {error}");
      return 2;
    }
  };
  let mut command = rustc_command(&invocation.program, None, inner_wrapper.as_deref());
  command.args(&invocation.arguments);
  if std::env::var_os(OBSERVATION_ONLY_ENV).is_none() {
    command.arg("--warn=unused-crate-dependencies");
  }
  command
    .env_remove(WRAPPER_MARKER)
    .env_remove(INNER_WRAPPER_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .env_remove(OBSERVATION_ONLY_ENV);
  crate::compiler::native_cache::remove_private_environment(&mut command);

  let status = command.status();
  let fact_result = recorder.finish(status.as_ref().is_ok_and(std::process::ExitStatus::success));

  match (status, fact_result) {
    (Ok(status), Ok(())) => compiler_status_code(status),
    (Ok(status), Err(_)) if !status.success() => compiler_status_code(status),
    (Ok(_), Err(error)) => {
      eprintln!("cargo-rail rustc wrapper: failed to publish compiler facts: {error}");
      2
    }
    (Err(error), _) => {
      eprintln!("cargo-rail rustc wrapper: failed to execute compiler: {error}");
      1
    }
  }
}

fn run_rustc_bypass(invocation: CompilerInvocation, inner_wrapper: Option<&std::ffi::OsStr>) -> i32 {
  let mut command = rustc_command(&invocation.program, None, inner_wrapper);
  command.args(&invocation.arguments);
  crate::compiler::native_cache::remove_private_environment(&mut command);
  run_transparently(command, "cargo-rail rustc wrapper")
}

fn is_rustc_information_request(arguments: &[OsString]) -> bool {
  arguments.iter().any(|argument| {
    matches!(
      argument.to_str(),
      Some("-h" | "--help" | "-V" | "--version" | "-vV" | "--print")
    ) || argument
      .to_str()
      .is_some_and(|argument| argument.starts_with("--print="))
  })
}

fn run_rustdoc() -> i32 {
  let Some(rustdoc) = std::env::var_os(INNER_RUSTDOC_ENV) else {
    eprintln!("cargo-rail rustdoc proxy: missing selected rustdoc executable");
    return 1;
  };
  let original_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
  if is_rustdoc_information_request(&original_arguments) {
    return run_rustdoc_bypass(CompilerInvocation::selected(rustdoc, original_arguments));
  }
  let fact_session = match fact_session() {
    Ok(Some(session)) => session,
    Ok(None) => return run_rustdoc_bypass(CompilerInvocation::selected(rustdoc, original_arguments)),
    Err(error) => {
      eprintln!("cargo-rail rustdoc proxy: {error}");
      return 2;
    }
  };
  let arguments = rustdoc_observation_arguments(&rustdoc, original_arguments);
  let invocation = CompilerInvocation::selected(rustdoc, arguments);
  let recorder = match crate::compiler::observation::begin_rustdoc_invocation(
    fact_session.observation_directory(),
    fact_session.source_root(),
    &invocation.arguments,
  ) {
    Ok(recorder) => recorder,
    Err(error) => {
      eprintln!("cargo-rail rustdoc proxy: failed to begin compiler fact collection: {error}");
      return 2;
    }
  };
  let mut command = invocation.command();
  command
    .env("RUSTDOC", &invocation.program)
    .env_remove(RUSTDOC_WRAPPER_MARKER)
    .env_remove(INNER_RUSTDOC_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV);
  crate::compiler::native_cache::remove_private_environment(&mut command);
  let status = command.status();
  let fact_result = recorder.finish(status.as_ref().is_ok_and(std::process::ExitStatus::success));

  match (status, fact_result) {
    (Ok(status), Ok(())) => compiler_status_code(status),
    (Ok(status), Err(_)) if !status.success() => compiler_status_code(status),
    (Ok(_), Err(error)) => {
      eprintln!("cargo-rail rustdoc proxy: failed to publish compiler facts: {error}");
      2
    }
    (Err(error), _) => {
      eprintln!("cargo-rail rustdoc proxy: failed to execute rustdoc: {error}");
      1
    }
  }
}

fn run_rustdoc_bypass(invocation: CompilerInvocation) -> i32 {
  let mut command = invocation.command();
  command.env("RUSTDOC", &invocation.program);
  crate::compiler::native_cache::remove_private_environment(&mut command);
  run_transparently(command, "cargo-rail rustdoc proxy")
}

#[cfg(unix)]
fn compiler_status_code(status: std::process::ExitStatus) -> i32 {
  use std::os::unix::process::ExitStatusExt as _;

  let Some(raw_signal) = status.signal() else {
    return status.code().unwrap_or(1);
  };
  if let Some(signal) = rustix::process::Signal::from_named_raw(raw_signal) {
    let _ = rustix::process::kill_process(rustix::process::getpid(), signal);
  }
  128 + raw_signal
}

#[cfg(not(unix))]
fn compiler_status_code(status: std::process::ExitStatus) -> i32 {
  status.code().unwrap_or(1)
}

fn fact_session() -> crate::error::RailResult<Option<crate::compiler::session::CompilerFactSession>> {
  let capability = std::env::var_os(crate::compiler::session::FACT_SESSION_ENV).map(PathBuf::from);
  let observation_directory = std::env::var_os(OBSERVATION_DIRECTORY_ENV).map(PathBuf::from);
  let source_root = std::env::var_os(OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from);
  match (capability, observation_directory, source_root) {
    (None, None, None) => Ok(None),
    (Some(capability), Some(observation_directory), Some(source_root)) => {
      crate::compiler::session::CompilerFactSession::load(&capability, &observation_directory, &source_root).map(Some)
    }
    _ => Err(crate::error::RailError::message(
      "compiler fact capability is incomplete",
    )),
  }
}

fn rustdoc_observation_arguments(rustdoc: &std::ffi::OsStr, mut arguments: Vec<OsString>) -> Vec<OsString> {
  if is_rustdoc_information_request(&arguments)
    || arguments
      .iter()
      .any(|argument| argument.as_encoded_bytes().starts_with(b"@"))
    || arguments
      .iter()
      .any(|argument| matches!(argument.to_str(), Some("--test" | "--check")))
    || uses_non_html_output_format(&arguments)
    || !is_cargo_rustdoc_crate_invocation(&arguments)
  {
    return arguments;
  }

  let mut index = 0usize;
  while index < arguments.len() {
    if arguments[index] == "--emit" {
      if let Some(value) = arguments.get_mut(index + 1)
        && let Some(value) = value.to_str()
        && let Some(extended) = rustdoc_emit_with_dep_info(rustdoc, value)
      {
        arguments[index + 1] = extended.into();
      }
      return arguments;
    }
    if let Some(value) = arguments[index]
      .to_str()
      .and_then(|argument| argument.strip_prefix("--emit="))
    {
      if let Some(extended) = rustdoc_emit_with_dep_info(rustdoc, value) {
        arguments[index] = format!("--emit={extended}").into();
      }
      return arguments;
    }
    index += 1;
  }

  if let Some(modes) = rustdoc_default_emit_modes(rustdoc) {
    arguments.push(format!("--emit={modes}").into());
  }
  arguments
}

fn is_cargo_rustdoc_crate_invocation(arguments: &[OsString]) -> bool {
  has_option_value(arguments, "--crate-name")
    && (has_option_value(arguments, "-o")
      || has_option_value(arguments, "--out-dir")
      || has_option_value(arguments, "--output"))
    && arguments.iter().any(|argument| {
      argument
        .to_str()
        .is_some_and(|argument| !argument.starts_with('-') && argument.ends_with(".rs"))
    })
}

fn has_option_value(arguments: &[OsString], option: &str) -> bool {
  arguments.iter().enumerate().any(|(index, argument)| {
    (argument == option && arguments.get(index + 1).is_some_and(|value| !value.is_empty()))
      || argument
        .to_str()
        .and_then(|argument| argument.strip_prefix(option))
        .is_some_and(|value| value.starts_with('=') && value.len() > 1)
  })
}

fn is_rustdoc_information_request(arguments: &[OsString]) -> bool {
  arguments
    .iter()
    .any(|argument| matches!(argument.to_str(), Some("-h" | "--help" | "-V" | "--version" | "-vV")))
}

fn uses_non_html_output_format(arguments: &[OsString]) -> bool {
  arguments.iter().enumerate().any(|(index, argument)| {
    let Some(argument) = argument.to_str() else {
      return true;
    };
    match argument {
      "-w" | "--output-format" => arguments
        .get(index + 1)
        .and_then(|value| value.to_str())
        .is_none_or(|value| value != "html"),
      _ => argument
        .strip_prefix("--output-format=")
        .is_some_and(|value| value != "html"),
    }
  })
}

fn rustdoc_emit_with_dep_info(rustdoc: &std::ffi::OsStr, value: &str) -> Option<String> {
  if emit_contains_dep_info(value) {
    return extend_rustdoc_emit(value, "");
  }
  extend_rustdoc_emit(value, &rustdoc_default_emit_modes(rustdoc)?)
}

fn emit_contains_dep_info(value: &str) -> bool {
  value
    .split(',')
    .map(str::trim)
    .any(|mode| mode.split_once('=').map_or(mode, |(name, _)| name) == "dep-info")
}

fn extend_rustdoc_emit(value: &str, supported: &str) -> Option<String> {
  let mut modes = value.split(',').map(str::trim).collect::<Vec<_>>();
  if modes.is_empty() || modes.iter().any(|mode| mode.is_empty()) {
    return None;
  }
  if emit_contains_dep_info(value) {
    return Some(modes.join(","));
  }
  let supported = supported.split(',').collect::<Vec<_>>();
  if !modes.iter().all(|mode| {
    let name = mode.split_once('=').map_or(*mode, |(name, _)| name);
    supported.contains(&name)
  }) {
    return None;
  }
  modes.push("dep-info");
  Some(modes.join(","))
}

fn rustdoc_default_emit_modes(rustdoc: &std::ffi::OsStr) -> Option<String> {
  let output = Command::new(rustdoc)
    // Older stable rustdoc releases listed `--emit` in help while still
    // rejecting it without `-Z unstable-options`. Probe the exact stable
    // invocation cargo-rail would add instead of trusting advertised syntax.
    .args(["--emit=dep-info", "--help"])
    .env("RUSTDOC", rustdoc)
    .env_remove(RUSTDOC_WRAPPER_MARKER)
    .env_remove(INNER_RUSTDOC_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  rustdoc_emit_modes_from_help(&String::from_utf8(output.stdout).ok()?)
}

fn rustdoc_emit_modes_from_help(help: &str) -> Option<String> {
  let modes = help.lines().find_map(|line| {
    let (_, remainder) = line.split_once("--emit [")?;
    remainder.split_once(']').map(|(modes, _)| modes)
  })?;
  let modes = modes.split(',').map(str::trim).collect::<Vec<_>>();
  matches!(
    modes.as_slice(),
    ["toolchain-shared-resources", "invocation-specific", "dep-info"]
      | ["html-static-files", "html-non-static-files", "dep-info"]
  )
  .then(|| modes.join(","))
}

#[cfg(test)]
mod tests {
  use std::ffi::OsStr;

  use super::*;

  #[test]
  fn compiler_roles_reject_ambiguity_but_allow_cache_then_observation() {
    assert_eq!(
      InvocationSignals {
        marked_cache: true,
        rustc_observation: true,
        ..InvocationSignals::default()
      }
      .classify(),
      Ok(Some(InvocationRole::MarkedCache))
    );

    for signals in [
      InvocationSignals {
        direct_cache: true,
        marked_cache: true,
        ..InvocationSignals::default()
      },
      InvocationSignals {
        rustc_observation: true,
        rustdoc_observation: true,
        ..InvocationSignals::default()
      },
      InvocationSignals {
        marked_cache: true,
        rustdoc_observation: true,
        ..InvocationSignals::default()
      },
    ] {
      assert!(signals.classify().is_err(), "ambiguous role was accepted: {signals:?}");
    }
  }

  #[cfg(unix)]
  #[test]
  fn compiler_invocation_preserves_non_utf8_program_and_arguments() {
    use std::os::unix::ffi::OsStringExt as _;

    let program = OsString::from_vec(vec![b'r', b'u', b's', b't', b'c', 0x80]);
    let arguments = vec![OsString::from_vec(vec![b'-', b'-', b'c', b'f', b'g', 0xff])];
    let invocation = CompilerInvocation::selected(program.clone(), arguments.clone());

    assert_eq!(invocation.program, program);
    assert_eq!(invocation.arguments, arguments);
  }

  #[test]
  fn rustc_information_requests_do_not_become_compilation_units() {
    for arguments in [
      vec![OsString::from("-vV")],
      vec![OsString::from("--print"), OsString::from("cfg")],
      vec![OsString::from("--print=file-names")],
    ] {
      assert!(is_rustc_information_request(&arguments));
    }
    assert!(!is_rustc_information_request(&[
      OsString::from("--crate-name"),
      OsString::from("unit"),
      OsString::from("src/lib.rs"),
    ]));
  }

  #[test]
  fn rustdoc_emit_discovery_accepts_both_msrv_and_current_names() {
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [toolchain-shared-resources,invocation-specific,dep-info]\n"),
      Some("toolchain-shared-resources,invocation-specific,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [html-static-files,html-non-static-files,dep-info]\n"),
      Some("html-static-files,html-non-static-files,dep-info".to_string())
    );
    assert_eq!(rustdoc_emit_modes_from_help("        --emit [html]\n"), None);
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [html-static-files,html-non-static-files,json,dep-info]\n"),
      None
    );
  }

  #[test]
  fn existing_rustdoc_emit_modes_gain_dep_info_without_losing_outputs() {
    assert_eq!(
      extend_rustdoc_emit(
        "html-static-files,html-non-static-files",
        "html-static-files,html-non-static-files,dep-info"
      ),
      Some("html-static-files,html-non-static-files,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_with_dep_info(OsStr::new("missing-rustdoc"), "html-static-files,dep-info"),
      Some("html-static-files,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_with_dep_info(OsStr::new("missing-rustdoc"), "dep-info=unit.d"),
      Some("dep-info=unit.d".to_string())
    );
    assert_eq!(
      extend_rustdoc_emit("json", "html-static-files,html-non-static-files,dep-info"),
      None
    );
    assert_eq!(extend_rustdoc_emit("", "html-static-files,dep-info"), None);
  }

  #[test]
  fn non_html_and_non_rendering_rustdoc_invocations_remain_unchanged() {
    let original = vec!["--crate-name".into(), "unit".into(), "--output-format=json".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["--crate-name".into(), "unit".into(), "--test".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["@arguments".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["README.md".into(), "-o".into(), "doc".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );
  }
}
