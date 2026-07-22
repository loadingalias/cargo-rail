//! Native rustc-result reuse for one explicitly graduated invocation class.
//!
//! A candidate identity is only an index. Reuse requires revalidating the
//! complete stored observation, deriving its final action identity again, and
//! restoring the locally bound result through the verified CAS.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::compiler::observation::{
  CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, FileObservation, InvocationRecorder,
  NativeOutputPaths, ObservationPath, RawCompilerInvocation,
};
use crate::error::{RailError, RailResult};
use crate::hermetic::cas::{LocalCas, NativeCacheLookup, NativeStoreRequest};
use crate::source::ContentDigest;

pub(crate) const CANDIDATE_KEY_PREFIX: &str = "compiler-candidate-v1-sha256-";
pub(crate) const ACTION_KEY_PREFIX: &str = "compiler-action-v1-sha256-";
pub(crate) const SESSION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION";
pub(crate) const STORE_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_STORE";
pub(crate) const DISPOSITION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION";
const SESSION_FILE: &str = "native-compiler-cache-session-v1.json";
const GRADUATED_RUSTC_RELEASE: &str = "1.97.1";
const GRADUATED_CARGO_RELEASE: &str = "1.97.1";
const MAX_SESSION_BYTES: u64 = 64 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const DEP_INFO_SLOT: &str = "target/outputs/dep-info";
const METADATA_SLOT: &str = "target/outputs/metadata";
const STDOUT_SLOT: &str = "target/streams/stdout";
const STDERR_SLOT: &str = "target/streams/stderr";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerSession {
  version: u32,
  identity: String,
  source_root_identity: String,
  class: NativeCompilerClass,
  toolchain_identity: String,
  compiler_environment_identity: String,
  cargo_configuration_identity: String,
}

/// Exact class, platform, and toolchain boundary that earned native reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerClass {
  name: String,
  platform: String,
  rustc_release: String,
  cargo_release: String,
}

impl NativeCompilerClass {
  fn capture(rustc_verbose_version: &str, cargo_verbose_version: &str) -> Self {
    Self {
      name: "workspace_library_metadata".to_string(),
      platform: format!(
        "{}-{}-{}",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH
      ),
      rustc_release: release_from_verbose(rustc_verbose_version, "rustc"),
      cargo_release: release_from_verbose(cargo_verbose_version, "cargo"),
    }
  }

  fn eligibility_reason(&self) -> Option<&'static str> {
    if self.platform != "unix-macos-aarch64" {
      Some("native_cache_platform_not_graduated")
    } else if self.rustc_release != GRADUATED_RUSTC_RELEASE || self.cargo_release != GRADUATED_CARGO_RELEASE {
      Some("native_cache_toolchain_not_graduated")
    } else {
      None
    }
  }
}

impl NativeCompilerSession {
  pub(crate) fn write(
    directory: &Path,
    source_root: &Path,
    rustc_verbose_version: &str,
    cargo_verbose_version: &str,
    toolchain_identity: &str,
    compiler_environment_identity: &str,
    cargo_configuration_identity: &str,
  ) -> RailResult<PathBuf> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let source_root_identity = path_identity(&source_root)?;
    let class = NativeCompilerClass::capture(rustc_verbose_version, cargo_verbose_version);
    let identity = session_identity(
      &source_root_identity,
      &class,
      toolchain_identity,
      compiler_environment_identity,
      cargo_configuration_identity,
    )?;
    let session = Self {
      version: 1,
      identity,
      source_root_identity,
      class,
      toolchain_identity: toolchain_identity.to_string(),
      compiler_environment_identity: compiler_environment_identity.to_string(),
      cargo_configuration_identity: cargo_configuration_identity.to_string(),
    };
    session.validate_object()?;
    #[cfg(target_os = "macos")]
    if session.class.eligibility_reason().is_none() {
      let cas = LocalCas::open()?;
      crate::hermetic::register_local_cache(&source_root, cas.root())?;
    }
    let session_directory = directory.join("native-cache-session");
    fs::create_dir(&session_directory)?;
    let path = session_directory.join(SESSION_FILE);
    crate::utils::write_file_atomic(&path, &serde_json::to_vec(&session)?)?;
    Ok(path)
  }

  fn load(path: &Path, source_root: &Path) -> RailResult<Self> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_SESSION_BYTES {
      return Err(RailError::message(
        "native compiler cache session is not a bounded regular file",
      ));
    }
    let session: Self = serde_json::from_slice(&fs::read(path)?)?;
    session.validate_object()?;
    if session.source_root_identity != path_identity(source_root)? {
      return Err(RailError::message("native compiler cache session source root changed"));
    }
    Ok(session)
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.version != 1 {
      return Err(RailError::message(
        "native compiler cache session has an incompatible schema",
      ));
    }
    for digest in [
      &self.identity,
      &self.source_root_identity,
      &self.toolchain_identity,
      &self.compiler_environment_identity,
      &self.cargo_configuration_identity,
    ] {
      validate_sha256(digest)?;
    }
    let expected = session_identity(
      &self.source_root_identity,
      &self.class,
      &self.toolchain_identity,
      &self.compiler_environment_identity,
      &self.cargo_configuration_identity,
    )?;
    if self.identity != expected {
      return Err(RailError::message(
        "native compiler cache session identity does not match its inputs",
      ));
    }
    Ok(())
  }
}

/// One output slot bound to rustc's current invocation paths only after CAS verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerOutput {
  role: String,
  slot: String,
  content_digest: String,
  bytes: u64,
}

/// Post-compile evidence retained behind a non-authorizing candidate index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerValidation {
  version: u32,
  candidate_key: String,
  action_key: String,
  session_identity: String,
  source_root_identity: String,
  class: NativeCompilerClass,
  observation: RawCompilerInvocation,
  outputs: Vec<NativeCompilerOutput>,
  stdout_digest: String,
  stderr_digest: String,
}

impl NativeCompilerValidation {
  fn new(
    session: &NativeCompilerSession,
    observation: RawCompilerInvocation,
    outputs: Vec<NativeCompilerOutput>,
    stdout_digest: String,
    stderr_digest: String,
  ) -> RailResult<Self> {
    let candidate_key = candidate_key(
      &session.identity,
      &session.source_root_identity,
      &session.class,
      &observation,
    )?;
    let action_key = action_key(
      &session.identity,
      &session.source_root_identity,
      &session.class,
      &observation,
    )?;
    let validation = Self {
      version: 1,
      candidate_key,
      action_key,
      session_identity: session.identity.clone(),
      source_root_identity: session.source_root_identity.clone(),
      class: session.class.clone(),
      observation,
      outputs,
      stdout_digest,
      stderr_digest,
    };
    validation.validate_object()?;
    Ok(validation)
  }

  pub(crate) fn candidate_key(&self) -> &str {
    &self.candidate_key
  }

  pub(crate) fn action_key(&self) -> &str {
    &self.action_key
  }

  pub(crate) fn result_digest(&self, output_manifest: &str) -> String {
    result_digest(&self.action_key, output_manifest)
  }

  pub(crate) fn validate_object(&self) -> RailResult<()> {
    if self.version != 1 {
      return Err(RailError::message(
        "native compiler observation has an incompatible schema",
      ));
    }
    validate_identity(&self.candidate_key, CANDIDATE_KEY_PREFIX)?;
    validate_identity(&self.action_key, ACTION_KEY_PREFIX)?;
    for digest in [
      &self.session_identity,
      &self.source_root_identity,
      &self.stdout_digest,
      &self.stderr_digest,
    ] {
      validate_sha256(digest)?;
    }
    if self.class.name != "workspace_library_metadata"
      || self.class.platform != "unix-macos-aarch64"
      || self.class.rustc_release != "1.97.1"
      || self.class.cargo_release != "1.97.1"
      || self.observation.version != 4
      || !self.observation.success
      || self.observation.mode != CompilerMode::Rustc
      || self.observation.compiler_arguments.is_empty()
      || invocation_bypass_reason(&self.observation, true).is_some()
      || self.outputs.len() != 2
      || self.outputs[0].role != "dep_info"
      || self.outputs[0].slot != DEP_INFO_SLOT
      || self.outputs[1].role != "metadata"
      || self.outputs[1].slot != METADATA_SLOT
      || self
        .outputs
        .iter()
        .any(|output| validate_sha256(&output.content_digest).is_err())
      || !complete_single_source(&self.observation)
      || !outputs_match_observation(&self.outputs, &self.observation.emitted_outputs)
    {
      return Err(RailError::message(
        "native compiler observation is outside the graduated class",
      ));
    }
    for output in &self.outputs {
      if output.bytes == 0 {
        return Err(RailError::message(
          "native compiler observation contains an empty compiler output",
        ));
      }
    }
    for file in self
      .observation
      .declared_inputs
      .iter()
      .chain(&self.observation.observed_reads)
      .chain(self.observation.dependency_artifacts.iter().map(|(_, file)| file))
      .chain(&self.observation.emitted_outputs)
    {
      validate_file_observation(file)?;
    }
    for environment in &self.observation.environment_reads {
      if environment.name.is_empty()
        || environment.name.as_bytes().contains(&0)
        || environment.secret_capability
        || environment
          .value_digest
          .as_deref()
          .is_some_and(|digest| validate_sha256(digest).is_err())
      {
        return Err(RailError::message(
          "native compiler observation contains an unsupported environment read",
        ));
      }
    }
    if candidate_key(
      &self.session_identity,
      &self.source_root_identity,
      &self.class,
      &self.observation,
    )? != self.candidate_key
      || action_key(
        &self.session_identity,
        &self.source_root_identity,
        &self.class,
        &self.observation,
      )? != self.action_key
    {
      return Err(RailError::message(
        "native compiler observation identity does not match its inputs",
      ));
    }
    Ok(())
  }
}

pub(crate) fn result_digest(action_key: &str, output_manifest: &str) -> String {
  let mut framed = Vec::from(&b"cargo-rail-native-compiler-result\0"[..]);
  append_frame(&mut framed, b"version", &1_u32.to_le_bytes());
  append_frame(&mut framed, b"action", action_key.as_bytes());
  append_frame(&mut framed, b"outputs", output_manifest.as_bytes());
  crate::instrumentation::record_hash(framed.len());
  format!("compiler-result-v1-sha256-{}", ContentDigest::sha256(&framed))
}

fn session_identity(
  source_root_identity: &str,
  class: &NativeCompilerClass,
  toolchain_identity: &str,
  compiler_environment_identity: &str,
  cargo_configuration_identity: &str,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  Ok(sha256_identity(
    "sha256:",
    b"cargo-rail-native-compiler-session\0",
    &[
      (b"version", &1_u32.to_le_bytes()),
      (b"source-root", source_root_identity.as_bytes()),
      (b"class", &class),
      (b"toolchain", toolchain_identity.as_bytes()),
      (b"compiler-environment", compiler_environment_identity.as_bytes()),
      (b"cargo-configuration", cargo_configuration_identity.as_bytes()),
    ],
  ))
}

fn candidate_key(
  session_identity: &str,
  source_root_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  let pre_execution = serde_json::to_vec(&(
    &observation.mode,
    &observation.crate_name,
    &observation.crate_types,
    &observation.target_argument,
    &observation.cfg,
    &observation.emit_modes,
    observation.test_mode,
    &observation.compiler_arguments,
    &observation.declared_inputs,
    &observation.dependency_artifacts,
  ))?;
  Ok(sha256_identity(
    CANDIDATE_KEY_PREFIX,
    b"cargo-rail-native-compiler-candidate\0",
    &[
      (b"version", &1_u32.to_le_bytes()),
      (b"session", session_identity.as_bytes()),
      (b"source-root", source_root_identity.as_bytes()),
      (b"class", &class),
      (b"pre-execution", &pre_execution),
    ],
  ))
}

fn action_key(
  session_identity: &str,
  source_root_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
) -> RailResult<String> {
  let candidate = candidate_key(session_identity, source_root_identity, class, observation)?;
  let discovered = serde_json::to_vec(&(&observation.observed_reads, &observation.environment_reads))?;
  Ok(sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-action\0",
    &[
      (b"version", &1_u32.to_le_bytes()),
      (b"candidate", candidate.as_bytes()),
      (b"discovered-inputs", &discovered),
    ],
  ))
}

fn sha256_identity(prefix: &str, domain: &[u8], frames: &[(&[u8], &[u8])]) -> String {
  let mut framed = Vec::from(domain);
  for (tag, value) in frames {
    append_frame(&mut framed, tag, value);
  }
  crate::instrumentation::record_hash(framed.len());
  format!("{prefix}{}", ContentDigest::sha256(&framed))
}

fn path_identity(path: &Path) -> RailResult<String> {
  let path = crate::utils::canonicalize_existing(path)?;
  Ok(sha256_identity(
    "sha256:",
    b"cargo-rail-native-compiler-source-root\0",
    &[(b"path", path.as_os_str().as_encoded_bytes())],
  ))
}

fn release_from_verbose(verbose: &str, program: &str) -> String {
  verbose
    .lines()
    .next()
    .and_then(|line| line.strip_prefix(program))
    .map(str::trim)
    .and_then(|rest| rest.split_ascii_whitespace().next())
    .unwrap_or("unknown")
    .to_string()
}

fn validate_file_observation(file: &FileObservation) -> RailResult<()> {
  validate_sha256(&file.content_digest)?;
  if file.symlink_target.is_some() {
    return Err(RailError::message(
      "native compiler observation contains a symlink input or output",
    ));
  }
  match &file.path {
    ObservationPath::Repository(path) => {
      crate::source::RepositoryPath::new(Path::new(path))?;
    }
    ObservationPath::Host(path) => {
      if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
        return Err(RailError::message(
          "native compiler observation contains an invalid host path",
        ));
      }
    }
  }
  Ok(())
}

fn complete_single_source(observation: &RawCompilerInvocation) -> bool {
  let [declared] = observation.declared_inputs.as_slice() else {
    return false;
  };
  let [observed] = observation.observed_reads.as_slice() else {
    return false;
  };
  matches!(&declared.path, ObservationPath::Repository(_))
    && declared == observed
    && observation
      .dependency_artifacts
      .iter()
      .all(|(_, artifact)| matches!(&artifact.path, ObservationPath::Repository(_)))
}

fn outputs_match_observation(outputs: &[NativeCompilerOutput], observed: &[FileObservation]) -> bool {
  let [dep_info, metadata] = outputs else {
    return false;
  };
  let mut dep_info_observation = None;
  let mut metadata_observation = None;
  for output in observed {
    if output.executable || output.symlink_target.is_some() {
      return false;
    }
    match output.path.resolve(Path::new("/")).extension().and_then(OsStr::to_str) {
      Some("d") if dep_info_observation.is_none() => dep_info_observation = Some(output),
      Some("rmeta") if metadata_observation.is_none() => metadata_observation = Some(output),
      _ => return false,
    }
  }
  dep_info_observation.is_some_and(|output| output.content_digest == dep_info.content_digest)
    && metadata_observation.is_some_and(|output| output.content_digest == metadata.content_digest)
}

fn invocation_bypass_reason(observation: &RawCompilerInvocation, complete: bool) -> Option<&'static str> {
  if observation.mode != CompilerMode::Rustc {
    return Some("rustdoc_not_graduated");
  }
  if observation.target_argument.is_some() {
    return Some("cross_target_not_graduated");
  }
  if observation.test_mode {
    return Some("test_compilation_not_graduated");
  }
  if observation.crate_types.contains("proc-macro") {
    return Some("proc_macro_not_graduated");
  }
  if observation
    .crate_types
    .iter()
    .any(|kind| matches!(kind.as_str(), "dylib" | "cdylib" | "staticlib"))
  {
    return Some("linker_producing_crate_type_not_graduated");
  }
  if observation.crate_types.contains("bin") {
    return Some(if observation.crate_name.as_deref() == Some("build_script_build") {
      "build_script_not_graduated"
    } else {
      "binary_not_graduated"
    });
  }
  if observation.crate_types != BTreeSet::from(["lib".to_string()]) {
    return Some("compiler_crate_type_not_graduated");
  }
  if observation.emit_modes != BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]) {
    return Some("compiler_emit_mode_not_graduated");
  }
  if observation.compiler_arguments.iter().any(|argument| argument == "-") {
    return Some("compiler_stdin_not_graduated");
  }
  if observation.compiler_arguments.iter().any(|argument| {
    argument == "-l"
      || argument.starts_with("-l")
      || argument.starts_with("-Lnative")
      || argument.starts_with("-L") && argument.contains("native=")
      || argument.contains("linker=")
      || argument.contains("link-arg=")
      || argument.contains("link-args=")
  }) || observation.compiler_arguments.windows(2).any(|pair| {
    pair[0] == "-L" && pair[1].starts_with("native=")
      || pair[0] == "-C"
        && matches!(
          pair[1].split_once('=').map(|(name, _)| name),
          Some("linker" | "link-arg" | "link-args")
        )
  }) {
    return Some("native_linking_not_graduated");
  }
  if observation
    .compiler_arguments
    .iter()
    .any(|argument| argument.contains("incremental="))
  {
    return Some("incremental_compilation_not_graduated");
  }
  if unsupported_compiler_argument(&observation.compiler_arguments) {
    return Some("compiler_flag_not_graduated");
  }
  if observation
    .dependency_artifacts
    .iter()
    .any(|(_, artifact)| artifact.path.resolve(Path::new("/")).extension() != Some(OsStr::new("rmeta")))
  {
    return Some("dependency_artifact_class_not_graduated");
  }
  if observation
    .environment_reads
    .iter()
    .any(|environment| environment.secret_capability)
  {
    return Some("secret_compiler_environment");
  }
  if !observation.bypasses.is_empty() {
    return Some("compiler_inputs_incomplete");
  }
  if observation.declared_inputs.is_empty() {
    return Some("declared_compiler_inputs_unavailable");
  }
  if complete && (observation.observed_reads.is_empty() || observation.emitted_outputs.len() != 2) {
    return Some("complete_compiler_observation_unavailable");
  }
  None
}

fn unsupported_compiler_argument(arguments: &[String]) -> bool {
  let mut index = 0usize;
  let mut source_inputs = 0usize;
  while index < arguments.len() {
    let argument = arguments[index].as_str();
    let next = arguments.get(index + 1).map(String::as_str);
    let consumes_next = match argument {
      "--crate-name" | "--crate-type" | "--emit" | "--out-dir" | "--edition" | "--error-format" | "--json"
      | "--cfg" | "--check-cfg" | "--cap-lints" | "--color" | "--diagnostic-width" | "--allow" | "--warn"
      | "--deny" | "--forbid" => next.is_some(),
      "--extern" => next.is_some_and(|value| value.contains('=')),
      "-L" => next.is_some_and(|value| value.starts_with("dependency=")),
      "-C" => next.is_some_and(supported_codegen_option),
      "-A" | "-W" | "-D" | "-F" => next.is_some(),
      _ if argument.starts_with("--crate-name=")
        || argument == "--crate-type=lib"
        || argument.starts_with("--emit=")
        || argument.starts_with("--out-dir=")
        || argument.starts_with("--edition=")
        || argument.starts_with("--error-format=")
        || argument.starts_with("--json=")
        || argument.starts_with("--cfg=")
        || argument.starts_with("--check-cfg=")
        || argument.starts_with("--cap-lints=")
        || argument.starts_with("--color=")
        || argument.starts_with("--diagnostic-width=")
        || argument.starts_with("--allow=")
        || argument.starts_with("--warn=")
        || argument.starts_with("--deny=")
        || argument.starts_with("--forbid=")
        || argument.starts_with("--extern=") && argument.contains('=')
        || argument.starts_with("-Ldependency=")
        || argument.starts_with("-A") && argument.len() > 2
        || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-D") && argument.len() > 2
        || argument.starts_with("-F") && argument.len() > 2 =>
      {
        false
      }
      _ if argument.starts_with("-C") && argument.len() > 2 => {
        if !supported_codegen_option(argument.trim_start_matches("-C")) {
          return true;
        }
        false
      }
      _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
        source_inputs += 1;
        false
      }
      _ => return true,
    };
    if consumes_next && next.is_none() {
      return true;
    }
    index += usize::from(consumes_next) + 1;
  }
  source_inputs != 1
}

fn supported_codegen_option(option: &str) -> bool {
  matches!(
    option.split_once('=').map(|(name, _)| name),
    Some(
      "metadata"
        | "extra-filename"
        | "embed-bitcode"
        | "debuginfo"
        | "split-debuginfo"
        | "opt-level"
        | "debug-assertions"
        | "overflow-checks"
        | "panic"
        | "codegen-units"
        | "strip"
    )
  )
}

pub(crate) fn metadata_from_environment() -> Option<CompilerCacheWrapperMetadata> {
  let encoded = std::env::var_os(DISPOSITION_ENV)?;
  let encoded = encoded.to_str()?;
  (encoded.len() <= MAX_SESSION_BYTES as usize)
    .then(|| serde_json::from_str(encoded).ok())
    .flatten()
}

/// Attempt native reuse and configure the cold child without changing Cargo's wrapper order.
///
/// `arguments` starts with the rustc executable because `program` is Cargo's
/// workspace-wrapper slot. A returned code means verified outputs and streams
/// were already restored; `None` preserves the ordinary child execution.
#[cfg(target_os = "macos")]
pub(crate) fn configure_outer(program: &OsStr, arguments: &[OsString], command: &mut Command) -> Option<i32> {
  command.env_remove(STORE_ENV).env_remove(DISPOSITION_ENV);
  if std::env::var_os(SESSION_ENV).is_none() {
    command.env_remove(SESSION_ENV);
    return None;
  }
  if !is_diagnostic_workspace_wrapper(program) {
    command.env_remove(SESSION_ENV);
    write_cache_event("bypassed", "dependency_crate_not_graduated", None, None, 0, 0);
    return None;
  }

  let Some((rustc, compiler_arguments)) = arguments.split_first() else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_argv_unavailable",
      None,
      0,
      false,
    );
    return None;
  };
  let Some(source_root) = std::env::var_os(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from)
  else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "native_cache_source_root_unavailable",
      None,
      0,
      false,
    );
    return None;
  };
  let Some(observation_directory) =
    std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV).map(PathBuf::from)
  else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "native_cache_observation_directory_unavailable",
      None,
      0,
      false,
    );
    return None;
  };
  let session = std::env::var_os(SESSION_ENV)
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message("native compiler cache session is unavailable"))
    .and_then(|path| NativeCompilerSession::load(&path, &source_root));
  let session = match session {
    Ok(session) => session,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "native_cache_session_unavailable",
        None,
        0,
        false,
      );
      return None;
    }
  };
  if let Some(reason) = session.class.eligibility_reason() {
    configure_cold(command, CompilerCacheWrapperStatus::Bypassed, reason, None, 0, false);
    return None;
  }
  if std::env::var_os("CARGO_INCREMENTAL").as_deref() != Some(OsStr::new("0")) {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "incremental_compilation_not_graduated",
      None,
      0,
      false,
    );
    return None;
  }
  let recorder = match crate::compiler::observation::begin_invocation(
    &observation_directory,
    &source_root,
    rustc,
    compiler_arguments,
  ) {
    Ok(recorder) => recorder,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "compiler_invocation_observation_unavailable",
        None,
        0,
        false,
      );
      return None;
    }
  };
  let observation = recorder.observation();
  if let Some(reason) = invocation_bypass_reason(observation, false) {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      reason,
      None,
      estimated_input_bytes(observation, &source_root),
      false,
    );
    return None;
  }
  let Some(output_paths) = recorder.native_output_paths() else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_paths_unavailable",
      None,
      estimated_input_bytes(observation, &source_root),
      false,
    );
    return None;
  };
  if validated_output_parent(&output_paths, &source_root).is_err() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_root_not_graduated",
      None,
      estimated_input_bytes(observation, &source_root),
      false,
    );
    return None;
  }
  if filesystem_macro_present(observation, &source_root) {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "filesystem_reading_macro_not_graduated",
      None,
      estimated_input_bytes(observation, &source_root),
      false,
    );
    return None;
  }
  let candidate = match candidate_key(
    &session.identity,
    &session.source_root_identity,
    &session.class,
    observation,
  ) {
    Ok(candidate) => candidate,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "candidate_key_unavailable",
        None,
        estimated_input_bytes(observation, &source_root),
        false,
      );
      return None;
    }
  };
  let mut bytes_hashed = estimated_input_bytes(observation, &source_root);
  let cas = match LocalCas::open() {
    Ok(cas) => cas,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_unavailable",
        Some(candidate),
        bytes_hashed,
        false,
      );
      return None;
    }
  };
  let candidates = match cas.native_candidates(&candidate) {
    Ok(candidates) => candidates,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_candidate_corrupt",
        Some(candidate),
        bytes_hashed,
        true,
      );
      return None;
    }
  };
  let mut miss_reason = "candidate_not_found";
  for cached in candidates {
    let _ = (cached.objects_verified, cached.bytes_read);
    let revalidated = revalidate_candidate(&cached.validation, &session, observation, &source_root);
    let action_key = match revalidated {
      Ok((revalidated, hashed)) if revalidated == cached.action_key => {
        bytes_hashed = bytes_hashed.saturating_add(hashed);
        revalidated
      }
      Ok((_, hashed)) => {
        bytes_hashed = bytes_hashed.saturating_add(hashed);
        miss_reason = "candidate_action_binding_mismatch";
        continue;
      }
      Err((reason, hashed)) => {
        bytes_hashed = bytes_hashed.saturating_add(hashed);
        miss_reason = reason;
        continue;
      }
    };
    match restore_and_publish(
      &cas,
      &action_key,
      &cached.validation,
      &output_paths,
      &source_root,
      &observation_directory,
      bytes_hashed,
    ) {
      Ok(()) => return Some(0),
      Err(_) => {
        miss_reason = "verified_result_materialization_failed";
      }
    }
  }
  configure_cold(
    command,
    CompilerCacheWrapperStatus::Miss,
    miss_reason,
    Some(candidate),
    bytes_hashed,
    true,
  );
  None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn configure_outer(_program: &OsStr, _arguments: &[OsString], command: &mut Command) -> Option<i32> {
  command.env_remove(STORE_ENV).env_remove(DISPOSITION_ENV);
  if std::env::var_os(SESSION_ENV).is_some() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "native_cache_platform_not_graduated",
      None,
      0,
      false,
    );
  } else {
    command.env_remove(SESSION_ENV);
  }
  None
}

fn configure_cold(
  command: &mut Command,
  status: CompilerCacheWrapperStatus,
  reason: &'static str,
  candidate_key: Option<String>,
  bytes_hashed: u64,
  store: bool,
) {
  let metadata = CompilerCacheWrapperMetadata::native(status, reason, candidate_key.clone(), None, bytes_hashed, 0);
  if let Ok(encoded) = serde_json::to_string(&metadata) {
    command.env(DISPOSITION_ENV, encoded);
  }
  if store {
    command.env(STORE_ENV, "1");
  }
  write_cache_event(
    status_name(status),
    reason,
    candidate_key.as_deref(),
    None,
    bytes_hashed,
    0,
  );
}

fn is_diagnostic_workspace_wrapper(program: &OsStr) -> bool {
  if std::env::var_os(crate::compiler::wrapper::WRAPPER_MARKER).is_none() {
    return false;
  }
  let Ok(current) = std::env::current_exe().and_then(fs::canonicalize) else {
    return false;
  };
  let selected = Path::new(program);
  let selected = if selected.is_absolute() {
    selected.to_path_buf()
  } else {
    match std::env::current_dir() {
      Ok(current_dir) => current_dir.join(selected),
      Err(_) => return false,
    }
  };
  fs::canonicalize(selected).is_ok_and(|selected| selected == current)
}

fn estimated_input_bytes(observation: &RawCompilerInvocation, source_root: &Path) -> u64 {
  observation
    .declared_inputs
    .iter()
    .chain(observation.dependency_artifacts.iter().map(|(_, file)| file))
    .filter_map(|file| fs::metadata(file.path.resolve(source_root)).ok())
    .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn filesystem_macro_present(observation: &RawCompilerInvocation, source_root: &Path) -> bool {
  observation
    .declared_inputs
    .iter()
    .chain(&observation.observed_reads)
    .filter_map(|file| fs::read(file.path.resolve(source_root)).ok())
    .any(|bytes| {
      [
        b"include".as_slice(),
        b"include_str".as_slice(),
        b"include_bytes".as_slice(),
      ]
      .iter()
      .any(|name| macro_invocation_present(&bytes, name))
    })
}

fn macro_invocation_present(bytes: &[u8], name: &[u8]) -> bool {
  if name.len() > bytes.len() {
    return false;
  }
  bytes.windows(name.len()).enumerate().any(|(offset, window)| {
    if window != name
      || offset
        .checked_sub(1)
        .and_then(|index| bytes.get(index))
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
      return false;
    }
    bytes[offset + name.len()..]
      .iter()
      .copied()
      .find(|byte| !byte.is_ascii_whitespace())
      == Some(b'!')
  })
}

fn revalidate_candidate(
  validation: &NativeCompilerValidation,
  session: &NativeCompilerSession,
  current: &RawCompilerInvocation,
  source_root: &Path,
) -> Result<(String, u64), (&'static str, u64)> {
  if validation.validate_object().is_err()
    || validation.session_identity != session.identity
    || validation.source_root_identity != session.source_root_identity
    || validation.class != session.class
  {
    return Err(("candidate_observation_incompatible", 0));
  }
  let current_candidate = candidate_key(
    &session.identity,
    &session.source_root_identity,
    &session.class,
    current,
  )
  .map_err(|_| ("candidate_key_unavailable", 0))?;
  if current_candidate != validation.candidate_key || !same_pre_execution_inputs(current, &validation.observation) {
    return Err(("candidate_pre_execution_inputs_changed", 0));
  }

  let mut bytes_hashed = 0u64;
  let mut revalidated = validation.observation.clone();
  revalidated.declared_inputs = revalidate_files(
    &validation.observation.declared_inputs,
    source_root,
    &mut bytes_hashed,
    "declared_compiler_input_changed",
  )?;
  revalidated.observed_reads = revalidate_files(
    &validation.observation.observed_reads,
    source_root,
    &mut bytes_hashed,
    "observed_compiler_read_changed",
  )?;
  let mut dependencies = Vec::with_capacity(validation.observation.dependency_artifacts.len());
  for (name, file) in &validation.observation.dependency_artifacts {
    let current = revalidate_file(file, source_root, &mut bytes_hashed)
      .map_err(|_| ("dependency_artifact_changed", bytes_hashed))?;
    dependencies.push((name.clone(), current));
  }
  revalidated.dependency_artifacts = dependencies;
  for environment in &validation.observation.environment_reads {
    if environment.secret_capability {
      return Err(("secret_compiler_environment", bytes_hashed));
    }
    let current = std::env::var_os(&environment.name)
      .as_deref()
      .map(OsStr::as_encoded_bytes)
      .map(ContentDigest::sha256)
      .map(|digest| format!("sha256:{digest}"));
    if current != environment.value_digest {
      return Err(("compiler_environment_changed", bytes_hashed));
    }
  }
  let action = action_key(
    &session.identity,
    &session.source_root_identity,
    &session.class,
    &revalidated,
  )
  .map_err(|_| ("compiler_action_key_unavailable", bytes_hashed))?;
  Ok((action, bytes_hashed))
}

fn same_pre_execution_inputs(current: &RawCompilerInvocation, stored: &RawCompilerInvocation) -> bool {
  current.mode == stored.mode
    && current.crate_name == stored.crate_name
    && current.crate_types == stored.crate_types
    && current.target_argument == stored.target_argument
    && current.cfg == stored.cfg
    && current.emit_modes == stored.emit_modes
    && current.test_mode == stored.test_mode
    && current.compiler_arguments == stored.compiler_arguments
    && current.declared_inputs == stored.declared_inputs
    && current.dependency_artifacts == stored.dependency_artifacts
}

fn revalidate_files(
  files: &[FileObservation],
  source_root: &Path,
  bytes_hashed: &mut u64,
  reason: &'static str,
) -> Result<Vec<FileObservation>, (&'static str, u64)> {
  files
    .iter()
    .map(|file| revalidate_file(file, source_root, bytes_hashed).map_err(|_| (reason, *bytes_hashed)))
    .collect()
}

fn revalidate_file(
  expected: &FileObservation,
  source_root: &Path,
  bytes_hashed: &mut u64,
) -> RailResult<FileObservation> {
  let path = expected.path.resolve(source_root);
  let (current, read) = FileObservation::capture_counted(&path, source_root, source_root)?;
  *bytes_hashed = bytes_hashed.saturating_add(read);
  if &current != expected {
    return Err(RailError::message("observed compiler input changed"));
  }
  Ok(current)
}

#[cfg(target_os = "macos")]
fn restore_and_publish(
  cas: &LocalCas,
  action_key: &str,
  validation: &NativeCompilerValidation,
  output_paths: &NativeOutputPaths,
  source_root: &Path,
  observation_directory: &Path,
  bytes_hashed: u64,
) -> RailResult<()> {
  validate_current_output_binding(validation, output_paths, source_root)?;
  let output_parent = validated_output_parent(output_paths, source_root)?;
  let temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-native-cache-")
    .tempdir_in(&output_parent)?;
  let restored = temporary.path().join("verified");
  let hit = match cas.restore_native(action_key, &restored) {
    NativeCacheLookup::Hit(hit) => hit,
    NativeCacheLookup::Miss(miss) => {
      let _ = (miss.objects_verified, miss.bytes_read);
      return Err(RailError::message(format!(
        "native compiler cache restore rejected the result: {}",
        miss.reason
      )));
    }
  };
  validate_restored_tree(&restored, validation)?;

  let stdout = read_bounded(&restored.join(STDOUT_SLOT), MAX_STREAM_BYTES)?;
  let stderr = read_bounded(&restored.join(STDERR_SLOT), MAX_STREAM_BYTES)?;
  if digest(&stdout) != validation.stdout_digest || digest(&stderr) != validation.stderr_digest {
    return Err(RailError::message(
      "native compiler cache stream binding changed after restore",
    ));
  }
  publish_output(
    &restored.join(DEP_INFO_SLOT),
    &output_paths.dep_info,
    &validation.outputs[0],
  )?;
  publish_output(
    &restored.join(METADATA_SLOT),
    &output_paths.metadata,
    &validation.outputs[1],
  )?;
  let mut raw = validation.observation.clone();
  raw.emitted_outputs = vec![
    FileObservation::capture(&output_paths.dep_info, source_root, source_root)?,
    FileObservation::capture(&output_paths.metadata, source_root, source_root)?,
  ];
  raw.emitted_outputs.sort();
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Hit,
    "verified_local_result",
    Some(validation.candidate_key.clone()),
    Some(validation.action_key.clone()),
    bytes_hashed,
    hit.bytes_restored,
  ));
  crate::compiler::observation::publish_raw(observation_directory, &raw)?;
  std::io::stdout().write_all(&stdout)?;
  std::io::stderr().write_all(&stderr)?;
  write_cache_event(
    "hit",
    "verified_local_result",
    Some(&validation.candidate_key),
    Some(&validation.action_key),
    bytes_hashed,
    hit.bytes_restored,
  );
  let _ = (
    hit.action_result,
    hit.result_digest,
    hit.objects_verified,
    hit.bytes_read,
  );
  Ok(())
}

fn validated_output_parent(outputs: &NativeOutputPaths, source_root: &Path) -> RailResult<PathBuf> {
  let dep_parent = outputs
    .dep_info
    .parent()
    .ok_or_else(|| RailError::message("dep-info output has no parent"))?;
  let metadata_parent = outputs
    .metadata
    .parent()
    .ok_or_else(|| RailError::message("metadata output has no parent"))?;
  if dep_parent != metadata_parent {
    return Err(RailError::message(
      "native compiler outputs do not share one publication directory",
    ));
  }
  let metadata = fs::symlink_metadata(dep_parent)?;
  if !metadata.is_dir() || metadata.file_type().is_symlink() {
    return Err(RailError::message(
      "native compiler output parent is not a real directory",
    ));
  }
  let canonical_parent = crate::utils::canonicalize_existing(dep_parent)?;
  let canonical_root = crate::utils::canonicalize_existing(source_root)?;
  if !canonical_parent.starts_with(&canonical_root)
    || outputs.dep_info.extension() != Some(OsStr::new("d"))
    || outputs.metadata.extension() != Some(OsStr::new("rmeta"))
    || outputs.dep_info == outputs.metadata
  {
    return Err(RailError::message(
      "native compiler outputs are outside the graduated publication root",
    ));
  }
  Ok(canonical_parent)
}

fn validate_current_output_binding(
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<()> {
  let current = [
    ObservationPath::capture(&outputs.dep_info, source_root, source_root),
    ObservationPath::capture(&outputs.metadata, source_root, source_root),
  ]
  .into_iter()
  .collect::<BTreeSet<_>>();
  let stored = validation
    .observation
    .emitted_outputs
    .iter()
    .map(|output| output.path.clone())
    .collect::<BTreeSet<_>>();
  if current != stored {
    return Err(RailError::message(
      "native compiler output destinations do not match the verified invocation",
    ));
  }
  Ok(())
}

fn validate_restored_tree(root: &Path, validation: &NativeCompilerValidation) -> RailResult<()> {
  let mut files = Vec::new();
  let mut pending = vec![root.to_path_buf()];
  while let Some(directory) = pending.pop() {
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
      return Err(RailError::message(
        "native compiler cache restored a non-directory tree node",
      ));
    }
    for entry in fs::read_dir(&directory)? {
      let path = entry?.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() && !metadata.file_type().is_symlink() {
        pending.push(path);
      } else if metadata.is_file() && !metadata.file_type().is_symlink() && single_link(&metadata) {
        files.push(crate::utils::path_to_git_format(path.strip_prefix(root)?));
      } else {
        return Err(RailError::message(
          "native compiler cache restored a symlink, hard link, or special file",
        ));
      }
    }
  }
  files.sort();
  if files != [DEP_INFO_SLOT, METADATA_SLOT, STDERR_SLOT, STDOUT_SLOT] {
    return Err(RailError::message(
      "native compiler cache restored an unexpected output tree",
    ));
  }
  for output in &validation.outputs {
    let bytes = read_bounded(
      &root.join(&output.slot),
      usize::try_from(output.bytes).unwrap_or(usize::MAX),
    )?;
    if bytes.len() as u64 != output.bytes || digest(&bytes) != output.content_digest {
      return Err(RailError::message(
        "native compiler cache restored output bytes that do not match the observation",
      ));
    }
  }
  Ok(())
}

fn publish_output(source: &Path, destination: &Path, expected: &NativeCompilerOutput) -> RailResult<()> {
  if let Ok(metadata) = fs::symlink_metadata(destination) {
    if !metadata.is_file() || metadata.file_type().is_symlink() || !single_link(&metadata) {
      return Err(RailError::message(
        "native compiler output destination is prepositioned",
      ));
    }
    let bytes = read_bounded(destination, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
    if bytes.len() as u64 == expected.bytes && digest(&bytes) == expected.content_digest {
      return Ok(());
    }
    return Err(RailError::message(
      "native compiler output destination contains different bytes",
    ));
  }
  let parent = destination
    .parent()
    .ok_or_else(|| RailError::message("native compiler output has no parent"))?;
  let mut input = File::open(source)?;
  let source_metadata = input.metadata()?;
  if !source_metadata.is_file() || !single_link(&source_metadata) {
    return Err(RailError::message(
      "verified native compiler output is not a single-link regular file",
    ));
  }
  let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
  let copied = std::io::copy(&mut input, &mut temporary)?;
  if copied != expected.bytes {
    return Err(RailError::message(
      "verified native compiler output changed during publication",
    ));
  }
  temporary.as_file().sync_all()?;
  let staged = read_bounded(temporary.path(), usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
  if staged.len() as u64 != expected.bytes || digest(&staged) != expected.content_digest {
    return Err(RailError::message(
      "verified native compiler output changed during publication",
    ));
  }
  fs::set_permissions(temporary.path(), source_metadata.permissions())?;
  match temporary.persist_noclobber(destination) {
    Ok(_) => sync_directory(parent),
    Err(error) if destination.is_file() => {
      let bytes = read_bounded(destination, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
      if bytes.len() as u64 == expected.bytes && digest(&bytes) == expected.content_digest {
        Ok(())
      } else {
        Err(RailError::message(format!(
          "concurrent native compiler output publication disagreed: {}",
          error.error
        )))
      }
    }
    Err(error) => Err(RailError::message(format!(
      "failed to publish native compiler output '{}': {}",
      destination.display(),
      error.error
    ))),
  }
}

fn read_bounded(path: &Path, limit: usize) -> RailResult<Vec<u8>> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file()
    || metadata.file_type().is_symlink()
    || !single_link(&metadata)
    || metadata.len() > limit as u64
  {
    return Err(RailError::message(format!(
      "native compiler cache file '{}' is not a bounded regular file",
      path.display()
    )));
  }
  fs::read(path).map_err(Into::into)
}

#[cfg(unix)]
fn single_link(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::MetadataExt as _;
  metadata.nlink() == 1
}

#[cfg(not(unix))]
fn single_link(_metadata: &fs::Metadata) -> bool {
  true
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> RailResult<()> {
  File::open(path)?.sync_all()?;
  Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> RailResult<()> {
  Ok(())
}

fn digest(bytes: &[u8]) -> String {
  format!("sha256:{}", ContentDigest::sha256(bytes))
}

pub(crate) fn store_requested() -> bool {
  std::env::var_os(STORE_ENV).is_some()
}

pub(crate) fn remove_private_environment(command: &mut Command) {
  command
    .env_remove(SESSION_ENV)
    .env_remove(STORE_ENV)
    .env_remove(DISPOSITION_ENV);
}

/// Execute one eligible cold invocation, replay its exact streams, and publish
/// only a complete successful observation.
pub(crate) fn run_and_store(mut command: Command, recorder: InvocationRecorder, context: &str) -> i32 {
  let output_paths = recorder.native_output_paths();
  let stdout_file = match tempfile::NamedTempFile::new() {
    Ok(file) => file,
    Err(_) => return run_without_store(command, recorder, context, "native_cache_stream_capture_unavailable"),
  };
  let stderr_file = match tempfile::NamedTempFile::new() {
    Ok(file) => file,
    Err(_) => return run_without_store(command, recorder, context, "native_cache_stream_capture_unavailable"),
  };
  let stdout_writer = match stdout_file.reopen() {
    Ok(file) => file,
    Err(_) => return run_without_store(command, recorder, context, "native_cache_stream_capture_unavailable"),
  };
  let stderr_writer = match stderr_file.reopen() {
    Ok(file) => file,
    Err(_) => return run_without_store(command, recorder, context, "native_cache_stream_capture_unavailable"),
  };
  let status = command.stdout(stdout_writer).stderr(stderr_writer).status();
  let status = match status {
    Ok(status) => status,
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      return 1;
    }
  };
  let stdout_len = stdout_file.as_file().metadata().map(|metadata| metadata.len()).ok();
  let stderr_len = stderr_file.as_file().metadata().map(|metadata| metadata.len()).ok();
  if let Ok(mut stdout) = File::open(stdout_file.path()) {
    let _ = std::io::copy(&mut stdout, &mut std::io::stdout());
  }
  if let Ok(mut stderr) = File::open(stderr_file.path()) {
    let _ = std::io::copy(&mut stderr, &mut std::io::stderr());
  }

  let mut raw = match recorder.complete(status.success()) {
    Ok(raw) => raw,
    Err(_) => return status.code().unwrap_or(1),
  };
  if !status.success() {
    let _ = publish_cold_observation(&mut raw, "compiler_execution_failed", None, 0, 0);
    return status.code().unwrap_or(1);
  }
  let Some(output_paths) = output_paths else {
    let _ = publish_cold_observation(&mut raw, "compiler_output_paths_unavailable", None, 0, 0);
    return status.code().unwrap_or(1);
  };
  if stdout_len.is_none_or(|bytes| bytes > MAX_STREAM_BYTES as u64)
    || stderr_len.is_none_or(|bytes| bytes > MAX_STREAM_BYTES as u64)
  {
    let _ = publish_cold_observation(&mut raw, "compiler_stream_limit_exceeded", None, 0, 0);
    return status.code().unwrap_or(1);
  }
  let source_root = match std::env::var_os(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from) {
    Some(root) => root,
    None => {
      let _ = publish_cold_observation(&mut raw, "native_cache_source_root_unavailable", None, 0, 0);
      return status.code().unwrap_or(1);
    }
  };
  let session = std::env::var_os(SESSION_ENV)
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message("native compiler cache session is unavailable"))
    .and_then(|path| NativeCompilerSession::load(&path, &source_root));
  let session = match session {
    Ok(session) => session,
    Err(_) => {
      let _ = publish_cold_observation(&mut raw, "native_cache_session_unavailable", None, 0, 0);
      return status.code().unwrap_or(1);
    }
  };
  if let Some(reason) = post_execution_bypass_reason(&raw, &source_root) {
    let bytes_hashed = estimated_input_bytes(&raw, &source_root);
    let _ = publish_cold_observation(&mut raw, reason, None, bytes_hashed, 0);
    return status.code().unwrap_or(1);
  }
  let stdout = match read_bounded(stdout_file.path(), MAX_STREAM_BYTES) {
    Ok(bytes) => bytes,
    Err(_) => {
      let _ = publish_cold_observation(&mut raw, "compiler_stdout_unavailable", None, 0, 0);
      return status.code().unwrap_or(1);
    }
  };
  let stderr = match read_bounded(stderr_file.path(), MAX_STREAM_BYTES) {
    Ok(bytes) => bytes,
    Err(_) => {
      let _ = publish_cold_observation(&mut raw, "compiler_stderr_unavailable", None, 0, 0);
      return status.code().unwrap_or(1);
    }
  };
  let publication = publish_cold_result(&session, &raw, &output_paths, &stdout, &stderr, &source_root);
  match publication {
    Ok((validation, written)) => {
      let initial = metadata_from_environment();
      let reason = initial
        .as_ref()
        .map(CompilerCacheWrapperMetadata::reason)
        .unwrap_or("candidate_not_found");
      let bytes_hashed = initial
        .as_ref()
        .map(CompilerCacheWrapperMetadata::bytes_hashed)
        .unwrap_or_default()
        .saturating_add(estimated_complete_input_bytes(&raw, &source_root));
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Miss,
        format!("{reason};stored_verified_result"),
        Some(validation.candidate_key.clone()),
        Some(validation.action_key.clone()),
        bytes_hashed,
        0,
      ));
      write_cache_event(
        "miss",
        "stored_verified_result",
        Some(&validation.candidate_key),
        Some(&validation.action_key),
        bytes_hashed,
        0,
      );
      let _ = written;
    }
    Err(_) => {
      let initial = metadata_from_environment();
      let reason = initial.as_ref().map(CompilerCacheWrapperMetadata::reason).map_or_else(
        || "local_cache_store_failed".to_string(),
        |reason| format!("{reason};local_cache_store_failed"),
      );
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Bypassed,
        &reason,
        initial
          .as_ref()
          .and_then(CompilerCacheWrapperMetadata::candidate_key)
          .map(str::to_string),
        None,
        estimated_complete_input_bytes(&raw, &source_root),
        0,
      ));
      write_cache_event(
        "bypassed",
        &reason,
        initial.as_ref().and_then(CompilerCacheWrapperMetadata::candidate_key),
        None,
        estimated_complete_input_bytes(&raw, &source_root),
        0,
      );
    }
  }
  if let Some(directory) = std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV).map(PathBuf::from) {
    let _ = crate::compiler::observation::publish_raw(&directory, &raw);
  }
  status.code().unwrap_or(1)
}

fn run_without_store(mut command: Command, recorder: InvocationRecorder, context: &str, reason: &'static str) -> i32 {
  match command.status() {
    Ok(status) => {
      if let Ok(mut raw) = recorder.complete(status.success()) {
        let _ = publish_cold_observation(&mut raw, reason, None, 0, 0);
        write_cache_event("bypassed", reason, None, None, 0, 0);
      }
      status.code().unwrap_or(1)
    }
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      1
    }
  }
}

fn publish_cold_observation(
  raw: &mut RawCompilerInvocation,
  reason: &'static str,
  action_key: Option<String>,
  bytes_hashed: u64,
  bytes_restored: u64,
) -> RailResult<()> {
  let initial = metadata_from_environment();
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    initial
      .as_ref()
      .and_then(CompilerCacheWrapperMetadata::candidate_key)
      .map(str::to_string),
    action_key,
    bytes_hashed,
    bytes_restored,
  ));
  let directory = std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV)
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message("compiler observation directory is unavailable"))?;
  crate::compiler::observation::publish_raw(&directory, raw)
}

fn post_execution_bypass_reason(observation: &RawCompilerInvocation, source_root: &Path) -> Option<&'static str> {
  if let Some(reason) = invocation_bypass_reason(observation, true) {
    return Some(reason);
  }
  if filesystem_macro_present(observation, source_root) {
    return Some("filesystem_reading_macro_not_graduated");
  }
  let declared = observation
    .declared_inputs
    .iter()
    .map(|file| &file.path)
    .collect::<BTreeSet<_>>();
  let additional = observation
    .observed_reads
    .iter()
    .filter(|file| !declared.contains(&file.path))
    .collect::<Vec<_>>();
  if additional.iter().any(|file| {
    file
      .path
      .resolve(source_root)
      .extension()
      .is_none_or(|extension| extension != "rs")
  }) {
    return Some("filesystem_reading_macro_not_graduated");
  }
  if !additional.is_empty() {
    return Some("multi_source_library_not_graduated");
  }
  None
}

fn publish_cold_result(
  session: &NativeCompilerSession,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  stdout: &[u8],
  stderr: &[u8],
  source_root: &Path,
) -> RailResult<(NativeCompilerValidation, u64)> {
  validated_output_parent(output_paths, source_root)?;
  let dep_info = observed_output(observation, &output_paths.dep_info, source_root)?;
  let metadata = observed_output(observation, &output_paths.metadata, source_root)?;
  let dep_info_bytes = fs::metadata(&output_paths.dep_info)?.len();
  let metadata_bytes = fs::metadata(&output_paths.metadata)?.len();
  let outputs = vec![
    NativeCompilerOutput {
      role: "dep_info".to_string(),
      slot: DEP_INFO_SLOT.to_string(),
      content_digest: dep_info.content_digest.clone(),
      bytes: dep_info_bytes,
    },
    NativeCompilerOutput {
      role: "metadata".to_string(),
      slot: METADATA_SLOT.to_string(),
      content_digest: metadata.content_digest.clone(),
      bytes: metadata_bytes,
    },
  ];
  let validation =
    NativeCompilerValidation::new(session, observation.clone(), outputs, digest(stdout), digest(stderr))?;
  if metadata_from_environment()
    .as_ref()
    .and_then(CompilerCacheWrapperMetadata::candidate_key)
    .is_some_and(|candidate| candidate != validation.candidate_key)
  {
    return Err(RailError::message(
      "cold compiler observation does not match the candidate selected by the outer wrapper",
    ));
  }

  let staging = tempfile::tempdir()?;
  let dep_slot = staging.path().join(DEP_INFO_SLOT);
  let metadata_slot = staging.path().join(METADATA_SLOT);
  let stdout_slot = staging.path().join(STDOUT_SLOT);
  let stderr_slot = staging.path().join(STDERR_SLOT);
  for directory in [dep_slot.parent(), stdout_slot.parent()].into_iter().flatten() {
    fs::create_dir_all(directory)?;
  }
  copy_regular_file(&output_paths.dep_info, &dep_slot, dep_info_bytes)?;
  copy_regular_file(&output_paths.metadata, &metadata_slot, metadata_bytes)?;
  validate_staged_output(&dep_slot, dep_info, dep_info_bytes)?;
  validate_staged_output(&metadata_slot, metadata, metadata_bytes)?;
  write_new_file(&stdout_slot, stdout)?;
  write_new_file(&stderr_slot, stderr)?;
  let manifest = crate::hermetic::capture_native_compiler_outputs(
    staging.path(),
    &[dep_slot, metadata_slot, stdout_slot, stderr_slot],
  )?;
  let result = validation.result_digest(manifest.digest());
  let cas = LocalCas::open()?;
  let stats = cas.store_native(NativeStoreRequest {
    action_key: validation.action_key(),
    candidate_key: validation.candidate_key(),
    result_digest: &result,
    manifest: &manifest,
    validation: &validation,
    source_root: staging.path(),
  })?;
  Ok((validation, stats.bytes_written))
}

fn observed_output<'a>(
  observation: &'a RawCompilerInvocation,
  path: &Path,
  source_root: &Path,
) -> RailResult<&'a FileObservation> {
  let expected = ObservationPath::capture(path, source_root, source_root);
  observation
    .emitted_outputs
    .iter()
    .find(|output| output.path == expected)
    .ok_or_else(|| RailError::message(format!("compiler output '{}' was not observed", path.display())))
}

fn copy_regular_file(source: &Path, destination: &Path, expected_bytes: u64) -> RailResult<()> {
  let before = fs::symlink_metadata(source)?;
  if !before.is_file() || before.file_type().is_symlink() || !single_link(&before) {
    return Err(RailError::message(format!(
      "compiler output '{}' is not a single-link regular file",
      source.display()
    )));
  }
  let mut input = File::open(source)?;
  let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
  let copied = std::io::copy(&mut input, &mut output)?;
  output.sync_all()?;
  let after = fs::symlink_metadata(source)?;
  if copied != expected_bytes || before.len() != after.len() || before.modified()? != after.modified()? {
    return Err(RailError::message(format!(
      "compiler output '{}' changed during cache staging",
      source.display()
    )));
  }
  Ok(())
}

fn validate_staged_output(path: &Path, expected: &FileObservation, expected_bytes: u64) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  let staged = FileObservation::capture(path, path.parent().unwrap_or(Path::new("/")), Path::new("/"))?;
  if metadata.len() != expected_bytes
    || staged.content_digest != expected.content_digest
    || staged.executable != expected.executable
    || staged.symlink_target.is_some()
  {
    return Err(RailError::message(
      "staged compiler output does not match the post-compile digest",
    ));
  }
  Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  Ok(())
}

fn estimated_complete_input_bytes(observation: &RawCompilerInvocation, source_root: &Path) -> u64 {
  observation
    .declared_inputs
    .iter()
    .chain(&observation.observed_reads)
    .chain(observation.dependency_artifacts.iter().map(|(_, file)| file))
    .filter_map(|file| fs::metadata(file.path.resolve(source_root)).ok())
    .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn status_name(status: CompilerCacheWrapperStatus) -> &'static str {
  match status {
    CompilerCacheWrapperStatus::Hit => "hit",
    CompilerCacheWrapperStatus::Miss => "miss",
    CompilerCacheWrapperStatus::Disabled => "disabled",
    CompilerCacheWrapperStatus::Bypassed => "bypassed",
  }
}

#[derive(Serialize)]
struct NativeCacheEvent<'a> {
  version: u32,
  status: &'a str,
  reason: &'a str,
  candidate_key: Option<&'a str>,
  action_key: Option<&'a str>,
  bytes_hashed: u64,
  bytes_restored: u64,
}

fn write_cache_event(
  status: &str,
  reason: &str,
  candidate_key: Option<&str>,
  action_key: Option<&str>,
  bytes_hashed: u64,
  bytes_restored: u64,
) {
  let Some(directory) = std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV).map(PathBuf::from) else {
    return;
  };
  let directory = directory.join("native-cache-events");
  if fs::create_dir_all(&directory).is_err() {
    return;
  }
  let event = NativeCacheEvent {
    version: 1,
    status,
    reason,
    candidate_key,
    action_key,
    bytes_hashed,
    bytes_restored,
  };
  let Ok(bytes) = serde_json::to_vec(&event) else {
    return;
  };
  let reason_slug = reason
    .bytes()
    .map(|byte| {
      if byte.is_ascii_alphanumeric() || byte == b'_' {
        byte as char
      } else {
        '_'
      }
    })
    .collect::<String>();
  let path = directory.join(format!("event-{}-{status}-{reason_slug}.json", std::process::id()));
  let _ = crate::utils::write_file_atomic(&path, &bytes);
}

pub(crate) fn validate_candidate_key(value: &str) -> RailResult<()> {
  validate_identity(value, CANDIDATE_KEY_PREFIX).map(|_| ())
}

pub(crate) fn validate_action_key(value: &str) -> RailResult<()> {
  validate_identity(value, ACTION_KEY_PREFIX).map(|_| ())
}

fn validate_sha256(value: &str) -> RailResult<()> {
  validate_identity(value, "sha256:").map(|_| ())
}

fn validate_identity<'a>(value: &'a str, prefix: &str) -> RailResult<&'a str> {
  let hex = value
    .strip_prefix(prefix)
    .ok_or_else(|| RailError::message("native compiler identity has the wrong domain or version"))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(RailError::message("native compiler identity is not canonical SHA-256"));
  }
  Ok(hex)
}

fn append_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use crate::compiler::observation::EnvironmentObservation;

  fn observed_file(path: &str, bytes: &[u8]) -> FileObservation {
    FileObservation {
      path: ObservationPath::Repository(path.to_string()),
      content_digest: digest(bytes),
      executable: false,
      symlink_target: None,
    }
  }

  fn graduated_observation() -> RawCompilerInvocation {
    let source = observed_file("src/lib.rs", b"pub fn value() -> u8 { 1 }\n");
    RawCompilerInvocation {
      version: 4,
      mode: CompilerMode::Rustc,
      crate_name: Some("fixture".to_string()),
      crate_types: BTreeSet::from(["lib".to_string()]),
      target_argument: None,
      cfg: BTreeSet::new(),
      emit_modes: BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]),
      test_mode: false,
      compiler_arguments: [
        "--crate-name",
        "fixture",
        "--edition=2024",
        "src/lib.rs",
        "--crate-type",
        "lib",
        "--emit=dep-info,metadata",
        "-C",
        "metadata=0123456789abcdef",
        "-Cextra-filename=-0123456789abcdef",
        "--out-dir",
        "target/debug/deps",
      ]
      .into_iter()
      .map(str::to_string)
      .collect(),
      declared_inputs: vec![source.clone()],
      observed_reads: vec![source],
      dependency_artifacts: Vec::new(),
      emitted_outputs: vec![
        observed_file("target/debug/deps/fixture-0123456789abcdef.d", b"dep-info"),
        observed_file("target/debug/deps/libfixture-0123456789abcdef.rmeta", b"metadata"),
      ],
      environment_reads: BTreeSet::new(),
      compiler: None,
      wrappers: Vec::new(),
      cache_wrapper: None,
      success: true,
      bypasses: BTreeSet::new(),
    }
  }

  fn graduated_session(source_root_identity: String) -> NativeCompilerSession {
    let class = NativeCompilerClass {
      name: "workspace_library_metadata".to_string(),
      platform: "unix-macos-aarch64".to_string(),
      rustc_release: GRADUATED_RUSTC_RELEASE.to_string(),
      cargo_release: GRADUATED_CARGO_RELEASE.to_string(),
    };
    let toolchain_identity = digest(b"toolchain");
    let compiler_environment_identity = digest(b"compiler-environment");
    let cargo_configuration_identity = digest(b"cargo-configuration");
    let identity = session_identity(
      &source_root_identity,
      &class,
      &toolchain_identity,
      &compiler_environment_identity,
      &cargo_configuration_identity,
    )
    .expect("session identity");
    NativeCompilerSession {
      version: 1,
      identity,
      source_root_identity,
      class,
      toolchain_identity,
      compiler_environment_identity,
      cargo_configuration_identity,
    }
  }

  fn graduated_validation(observation: RawCompilerInvocation) -> NativeCompilerValidation {
    let session = graduated_session(digest(b"source-root"));
    let outputs = vec![
      NativeCompilerOutput {
        role: "dep_info".to_string(),
        slot: DEP_INFO_SLOT.to_string(),
        content_digest: observation.emitted_outputs[0].content_digest.clone(),
        bytes: 8,
      },
      NativeCompilerOutput {
        role: "metadata".to_string(),
        slot: METADATA_SLOT.to_string(),
        content_digest: observation.emitted_outputs[1].content_digest.clone(),
        bytes: 8,
      },
    ];
    NativeCompilerValidation::new(&session, observation, outputs, digest(b""), digest(b""))
      .expect("graduated validation")
  }

  pub(crate) fn cas_validation() -> NativeCompilerValidation {
    graduated_validation(graduated_observation())
  }

  #[test]
  fn candidate_never_contains_discovered_authority() {
    let session = graduated_session(digest(b"source-root"));
    let base = graduated_observation();
    let mut environment_changed = base.clone();
    environment_changed.environment_reads.insert(EnvironmentObservation {
      name: "P73_VALUE".to_string(),
      value_digest: Some(digest(b"one")),
      secret_capability: false,
    });
    let mut observed_changed = base.clone();
    observed_changed.observed_reads[0].content_digest = digest(b"different observed bytes");

    let candidate =
      candidate_key(&session.identity, &session.source_root_identity, &session.class, &base).expect("candidate");
    assert_eq!(
      candidate,
      candidate_key(
        &session.identity,
        &session.source_root_identity,
        &session.class,
        &environment_changed,
      )
      .expect("environment candidate")
    );
    assert_eq!(
      candidate,
      candidate_key(
        &session.identity,
        &session.source_root_identity,
        &session.class,
        &observed_changed,
      )
      .expect("observed candidate")
    );
    assert_ne!(
      action_key(&session.identity, &session.source_root_identity, &session.class, &base).expect("action"),
      action_key(
        &session.identity,
        &session.source_root_identity,
        &session.class,
        &environment_changed,
      )
      .expect("environment action")
    );
    assert_ne!(
      action_key(&session.identity, &session.source_root_identity, &session.class, &base).expect("action"),
      action_key(
        &session.identity,
        &session.source_root_identity,
        &session.class,
        &observed_changed,
      )
      .expect("observed action")
    );
  }

  #[test]
  fn revalidation_hashes_bytes_instead_of_trusting_size() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const VALUE: u8 = 1;\n").expect("source");
    let captured = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![captured.clone()];
    observation.observed_reads = vec![captured];
    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let validation = NativeCompilerValidation::new(
      &session,
      observation.clone(),
      graduated_validation(observation.clone()).outputs,
      digest(b""),
      digest(b""),
    )
    .expect("validation");
    let (action, bytes_hashed) =
      revalidate_candidate(&validation, &session, &observation, root.path()).expect("unchanged candidate");
    assert_eq!(action, validation.action_key);
    assert_eq!(bytes_hashed, fs::metadata(&source).expect("source metadata").len() * 2);

    fs::write(&source, b"pub const VALUE: u8 = 2;\n").expect("same-size mutation");
    let error = revalidate_candidate(&validation, &session, &observation, root.path())
      .expect_err("same-size content mutation must miss");
    assert_eq!(error.0, "declared_compiler_input_changed");
  }

  #[test]
  fn only_the_exact_compiler_class_is_graduated() {
    let baseline = graduated_observation();
    assert_eq!(invocation_bypass_reason(&baseline, true), None);

    let assert_bypass = |expected, mutate: fn(&mut RawCompilerInvocation)| {
      let mut observation = baseline.clone();
      mutate(&mut observation);
      assert_eq!(
        invocation_bypass_reason(&observation, true),
        Some(expected),
        "{expected}"
      );
    };
    assert_bypass("rustdoc_not_graduated", |value| value.mode = CompilerMode::Rustdoc);
    assert_bypass("cross_target_not_graduated", |value| {
      value.target_argument = Some("x86_64-unknown-linux-gnu".to_string());
    });
    assert_bypass("test_compilation_not_graduated", |value| value.test_mode = true);
    assert_bypass("proc_macro_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["proc-macro".to_string()]);
    });
    assert_bypass("linker_producing_crate_type_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["cdylib".to_string()]);
    });
    assert_bypass("binary_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["bin".to_string()]);
    });
    assert_bypass("build_script_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["bin".to_string()]);
      value.crate_name = Some("build_script_build".to_string());
    });
    assert_bypass("compiler_emit_mode_not_graduated", |value| {
      value.emit_modes.insert("link".to_string());
    });
    assert_bypass("compiler_stdin_not_graduated", |value| {
      value.compiler_arguments.push("-".to_string());
    });
    assert_bypass("native_linking_not_graduated", |value| {
      value
        .compiler_arguments
        .extend(["-L".to_string(), "native=/tmp".to_string()]);
    });
    assert_bypass("incremental_compilation_not_graduated", |value| {
      value
        .compiler_arguments
        .extend(["-C".to_string(), "incremental=target/incremental".to_string()]);
    });
    assert_bypass("compiler_flag_not_graduated", |value| {
      value.compiler_arguments.push("-Zunproven".to_string());
    });
    assert_bypass("dependency_artifact_class_not_graduated", |value| {
      value.dependency_artifacts.push((
        "dep".to_string(),
        observed_file("target/debug/deps/libdep.rlib", b"rlib"),
      ));
    });
    assert_bypass("secret_compiler_environment", |value| {
      value.environment_reads.insert(EnvironmentObservation {
        name: "TOKEN".to_string(),
        value_digest: Some(digest(b"redacted")),
        secret_capability: true,
      });
    });
    assert_bypass("compiler_inputs_incomplete", |value| {
      value.bypasses.insert("unknown_input".to_string());
    });
    assert_bypass("declared_compiler_inputs_unavailable", |value| {
      value.declared_inputs.clear();
    });
    assert_bypass("complete_compiler_observation_unavailable", |value| {
      value.observed_reads.clear();
    });
  }

  #[test]
  fn validation_rejects_forged_output_bindings() {
    let validation = graduated_validation(graduated_observation());
    validation.validate_object().expect("baseline validation");

    let mut duplicate_slot = validation.clone();
    duplicate_slot.outputs[1].slot = DEP_INFO_SLOT.to_string();
    assert!(duplicate_slot.validate_object().is_err());

    let mut forged_digest = validation.clone();
    forged_digest.outputs[0].content_digest = digest(b"forged");
    assert!(forged_digest.validate_object().is_err());

    let mut forged_action = validation;
    forged_action.action_key = format!("{ACTION_KEY_PREFIX}{}", "0".repeat(64));
    assert!(forged_action.validate_object().is_err());
  }

  #[test]
  fn publication_root_must_remain_inside_the_source_root() {
    let source = tempfile::tempdir().expect("source root");
    let external = tempfile::tempdir().expect("external root");
    let internal = source.path().join("target/debug/deps");
    fs::create_dir_all(&internal).expect("internal target");
    let valid = NativeOutputPaths {
      dep_info: internal.join("fixture.d"),
      metadata: internal.join("libfixture.rmeta"),
    };
    assert!(validated_output_parent(&valid, source.path()).is_ok());

    let escaped = NativeOutputPaths {
      dep_info: external.path().join("fixture.d"),
      metadata: external.path().join("libfixture.rmeta"),
    };
    assert!(validated_output_parent(&escaped, source.path()).is_err());
  }

  #[test]
  fn publication_rehashes_staged_bytes_before_exposure() {
    let directory = tempfile::tempdir().expect("publication directory");
    let source = directory.path().join("restored.d");
    let destination = directory.path().join("published.d");
    fs::write(&source, b"forged!").expect("forged restored output");
    let expected = NativeCompilerOutput {
      role: "dep_info".to_string(),
      slot: DEP_INFO_SLOT.to_string(),
      content_digest: digest(b"correct"),
      bytes: 7,
    };

    publish_output(&source, &destination, &expected).expect_err("same-size forged bytes must fail closed");

    assert!(!destination.exists());
  }

  #[test]
  fn filesystem_reading_macros_are_detected_conservatively() {
    assert!(macro_invocation_present(
      b"const X: &str = include_str!(\"x\");",
      b"include_str"
    ));
    assert!(macro_invocation_present(b"include_bytes ! (\"x\")", b"include_bytes"));
    assert!(macro_invocation_present(b"include!(\"x.rs\")", b"include"));
    assert!(!macro_invocation_present(b"fn include_str_value() {}", b"include_str"));
    assert!(!macro_invocation_present(b"// included text", b"include"));
  }

  #[test]
  fn eligibility_is_scoped_to_one_platform_and_toolchain() {
    let session = graduated_session(digest(b"source-root"));
    assert_eq!(session.class.eligibility_reason(), None);

    let mut platform = session.class.clone();
    platform.platform = "unix-linux-x86_64".to_string();
    assert_eq!(
      platform.eligibility_reason(),
      Some("native_cache_platform_not_graduated")
    );

    let mut toolchain = session.class;
    toolchain.rustc_release = "1.97.2".to_string();
    assert_eq!(
      toolchain.eligibility_reason(),
      Some("native_cache_toolchain_not_graduated")
    );
  }

  #[test]
  fn session_identity_changes_with_exact_toolchain_identity() {
    let session = graduated_session(digest(b"source-root"));
    let changed = session_identity(
      &session.source_root_identity,
      &session.class,
      &digest(b"changed-toolchain"),
      &session.compiler_environment_identity,
      &session.cargo_configuration_identity,
    )
    .expect("changed session identity");

    assert_ne!(changed, session.identity);
  }
}
