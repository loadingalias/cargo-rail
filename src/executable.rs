//! Exact executable-byte identities with explicit runtime limitations.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const EXECUTABLE_IDENTITY_VERSION: u32 = 1;
const MAX_INTERPRETER_DEPTH: usize = 4;

/// Exact bytes selected for one process executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutableIdentity {
  version: u32,
  selection: String,
  resolved_path: String,
  content_digest: String,
  executable: bool,
  interpreter: Option<Box<Self>>,
  limitations: BTreeSet<String>,
}

/// Executables captured once for one immutable workspace snapshot.
#[derive(Clone)]
pub(crate) struct ToolchainExecutableIdentities {
  cargo: ExecutableIdentity,
  rustc: ExecutableIdentity,
  rustdoc: Option<ExecutableIdentity>,
  rustc_wrapper: Option<ExecutableIdentity>,
  rustc_workspace_wrapper: Option<ExecutableIdentity>,
  cargo_implementation: Option<ExecutableIdentity>,
  rustc_implementation: Option<ExecutableIdentity>,
  rustdoc_implementation: Option<ExecutableIdentity>,
  limitations: BTreeSet<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum ToolchainExecutableScope {
  Compilation,
  Documentation,
}

impl ExecutableIdentity {
  /// Resolve and digest the executable that `Command` would select.
  pub(crate) fn capture(selection: &OsStr, current_dir: &Path, source_root: &Path) -> RailResult<Self> {
    capture_at_depth(selection, current_dir, source_root, 0)
  }

  /// Stable bytes suitable for a larger framed identity.
  pub(crate) fn identity_bytes(&self) -> RailResult<Vec<u8>> {
    serde_json::to_vec(self).map_err(Into::into)
  }

  /// Fail-closed limitations that prevent this executable alone from proving a hermetic process.
  pub(crate) fn limitations(&self) -> impl Iterator<Item = &str> {
    self.limitations.iter().map(String::as_str)
  }

  pub(crate) fn content_digest(&self) -> &str {
    &self.content_digest
  }

  pub(crate) fn is_executable(&self) -> bool {
    self.executable
  }

  pub(crate) fn same_resolved_file(&self, other: &Self) -> bool {
    self.resolved_path == other.resolved_path && self.content_digest == other.content_digest
  }
}

impl ToolchainExecutableIdentities {
  pub(crate) fn capture(
    toolchain: &crate::cargo::ToolchainIdentity,
    current_dir: &Path,
    source_root: &Path,
    _scope: ToolchainExecutableScope,
  ) -> RailResult<Self> {
    let mut captured = std::collections::BTreeMap::<PathBuf, ExecutableIdentity>::new();
    let mut capture = |program: &OsStr| -> RailResult<ExecutableIdentity> {
      let resolved = resolve_program(program, current_dir)?.canonicalize().map_err(|error| {
        RailError::message(format!(
          "failed to resolve executable '{}': {error}",
          program.to_string_lossy()
        ))
      })?;
      if let Some(identity) = captured.get(&resolved) {
        return Ok(identity.clone());
      }
      let identity = ExecutableIdentity::capture(program, current_dir, source_root)?;
      captured.insert(resolved, identity.clone());
      Ok(identity)
    };
    let cargo = capture(toolchain.cargo_program())?;
    let rustc = capture(toolchain.rustc_program())?;
    // rustdoc is part of the selected Rust distribution even when the current
    // action compiles rather than documents. Native-cache capability
    // certificates bind the complete Cargo/rustc/rustdoc trio.
    let rustdoc = Some(capture(toolchain.rustdoc_program())?);
    let rustc_wrapper = toolchain.rustc_wrapper_program().map(&mut capture).transpose()?;
    let rustc_workspace_wrapper = toolchain
      .rustc_workspace_wrapper_program()
      .map(&mut capture)
      .transpose()?;
    let mut limitations = BTreeSet::new();
    let mut implementation =
      |name: &'static str| match capture(sysroot_program(toolchain.rustc_sysroot(), name).as_os_str()) {
        Ok(identity) => Some(identity),
        Err(_) => {
          limitations.insert(format!("{name}_implementation_executable_unavailable"));
          None
        }
      };
    let cargo_implementation = implementation("cargo");
    let rustc_implementation = implementation("rustc");
    let rustdoc_implementation = Some(implementation("rustdoc")).flatten();
    for (role, executable) in [
      ("cargo", Some(&cargo)),
      ("rustc", Some(&rustc)),
      ("rustdoc", rustdoc.as_ref()),
      ("rustc_wrapper", rustc_wrapper.as_ref()),
      ("rustc_workspace_wrapper", rustc_workspace_wrapper.as_ref()),
      ("cargo_implementation", cargo_implementation.as_ref()),
      ("rustc_implementation", rustc_implementation.as_ref()),
      ("rustdoc_implementation", rustdoc_implementation.as_ref()),
    ] {
      if let Some(executable) = executable {
        limitations.extend(
          executable
            .limitations()
            .map(|limitation| format!("{role}_{limitation}")),
        );
      }
    }
    Ok(Self {
      cargo,
      rustc,
      rustdoc,
      rustc_wrapper,
      rustc_workspace_wrapper,
      cargo_implementation,
      rustc_implementation,
      rustdoc_implementation,
      limitations,
    })
  }

  pub(crate) fn cargo(&self) -> &ExecutableIdentity {
    &self.cargo
  }

  pub(crate) fn rustc(&self) -> &ExecutableIdentity {
    &self.rustc
  }

  pub(crate) fn rustdoc(&self) -> Option<&ExecutableIdentity> {
    self.rustdoc.as_ref()
  }

  pub(crate) fn rustc_wrapper(&self) -> Option<&ExecutableIdentity> {
    self.rustc_wrapper.as_ref()
  }

  pub(crate) fn rustc_workspace_wrapper(&self) -> Option<&ExecutableIdentity> {
    self.rustc_workspace_wrapper.as_ref()
  }

  pub(crate) fn cargo_implementation(&self) -> Option<&ExecutableIdentity> {
    self.cargo_implementation.as_ref()
  }

  pub(crate) fn rustc_implementation(&self) -> Option<&ExecutableIdentity> {
    self.rustc_implementation.as_ref()
  }

  pub(crate) fn rustdoc_implementation(&self) -> Option<&ExecutableIdentity> {
    self.rustdoc_implementation.as_ref()
  }

  pub(crate) fn limitations(&self) -> impl Iterator<Item = &str> {
    self.limitations.iter().map(String::as_str)
  }

  pub(crate) fn identity_bytes(&self) -> RailResult<Vec<u8>> {
    serde_json::to_vec(&(
      &self.cargo,
      &self.rustc,
      &self.rustdoc,
      &self.rustc_wrapper,
      &self.rustc_workspace_wrapper,
      &self.cargo_implementation,
      &self.rustc_implementation,
      &self.rustdoc_implementation,
      &self.limitations,
    ))
    .map_err(Into::into)
  }
}

fn sysroot_program(sysroot: &Path, name: &str) -> PathBuf {
  #[cfg(windows)]
  let name = format!("{name}.exe");
  sysroot.join("bin").join(name)
}

fn capture_at_depth(
  selection: &OsStr,
  current_dir: &Path,
  source_root: &Path,
  depth: usize,
) -> RailResult<ExecutableIdentity> {
  if depth > MAX_INTERPRETER_DEPTH {
    return Err(RailError::message(format!(
      "executable interpreter chain exceeds {MAX_INTERPRETER_DEPTH} entries"
    )));
  }
  let selection_text = selection
    .to_str()
    .ok_or_else(|| RailError::message("executable selection is not valid UTF-8"))?;
  if selection_text.is_empty() {
    return Err(RailError::message("executable selection is empty"));
  }
  let resolved = resolve_program(selection, current_dir)?;
  let canonical = resolved.canonicalize().map_err(|error| {
    RailError::message(format!(
      "failed to resolve executable '{}': {error}",
      resolved.display()
    ))
  })?;
  let metadata = fs::metadata(&canonical).map_err(|error| {
    RailError::message(format!(
      "failed to inspect executable '{}': {error}",
      canonical.display()
    ))
  })?;
  if !metadata.is_file() {
    return Err(RailError::message(format!(
      "selected executable '{}' is not a regular file",
      canonical.display()
    )));
  }
  let (content_digest, shebang) = digest_executable(&canonical)?;
  let executable = is_executable(&metadata);
  let mut limitations = BTreeSet::new();
  let interpreter = if let Some(shebang) = shebang {
    limitations.insert("script_runtime_inputs_unavailable".to_string());
    match shebang_interpreter(&shebang)? {
      Some(interpreter) if is_env_interpreter(&interpreter) => {
        limitations.insert("env_shebang_resolution_unavailable".to_string());
        None
      }
      Some(interpreter) => Some(Box::new(capture_at_depth(
        OsStr::new(&interpreter),
        canonical.parent().unwrap_or(current_dir),
        source_root,
        depth + 1,
      )?)),
      None => {
        limitations.insert("script_interpreter_identity_unavailable".to_string());
        None
      }
    }
  } else {
    limitations.insert("dynamic_executable_inputs_unavailable".to_string());
    None
  };
  if let Some(interpreter) = &interpreter {
    limitations.extend(interpreter.limitations.iter().cloned());
  }

  Ok(ExecutableIdentity {
    version: EXECUTABLE_IDENTITY_VERSION,
    selection: portable_selection(selection_text, current_dir, source_root),
    resolved_path: portable_path(&canonical, source_root),
    content_digest: format!("sha256:{content_digest}"),
    executable,
    interpreter,
    limitations,
  })
}

fn digest_executable(path: &Path) -> RailResult<(ContentDigest, Option<Vec<u8>>)> {
  let mut file = File::open(path)
    .map_err(|error| RailError::message(format!("failed to read executable '{}': {error}", path.display())))?;
  let mut hasher = Sha256::new();
  let mut prefix = [0_u8; 2];
  let mut prefix_len = 0;
  while prefix_len < prefix.len() {
    let read = read_executable_chunk(&mut file, &mut prefix[prefix_len..], path)?;
    if read == 0 {
      break;
    }
    prefix_len += read;
  }
  hasher.update(&prefix[..prefix_len]);
  let mut bytes_read = prefix_len;
  let mut shebang = (prefix_len == prefix.len() && prefix == *b"#!").then(Vec::new);
  let mut buffer = [0_u8; 64 * 1024];
  loop {
    let read = read_executable_chunk(&mut file, &mut buffer, path)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    bytes_read = bytes_read.saturating_add(read);
    if let Some(line) = &mut shebang
      && !line.ends_with(b"\n")
    {
      let end = buffer[..read]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(read, |newline| newline + 1);
      line.extend_from_slice(&buffer[..end]);
    }
  }
  crate::instrumentation::record_hash(bytes_read);
  crate::instrumentation::record_hashed_file_bytes_read(bytes_read);
  Ok((ContentDigest::from_sha256_bytes(hasher.finalize().into()), shebang))
}

fn read_executable_chunk(file: &mut File, buffer: &mut [u8], path: &Path) -> RailResult<usize> {
  loop {
    match file.read(buffer) {
      Ok(read) => return Ok(read),
      Err(error) if error.kind() == ErrorKind::Interrupted => {}
      Err(error) => {
        return Err(RailError::message(format!(
          "failed to read executable '{}': {error}",
          path.display()
        )));
      }
    }
  }
}

pub(crate) fn resolve_program(selection: &OsStr, current_dir: &Path) -> RailResult<PathBuf> {
  let selected = Path::new(selection);
  if selected.is_absolute() || selected.components().count() > 1 {
    return Ok(if selected.is_absolute() {
      selected.to_path_buf()
    } else {
      current_dir.join(selected)
    });
  }

  let path = std::env::var_os("PATH").ok_or_else(|| {
    RailError::message(format!(
      "cannot resolve executable '{}' because PATH is absent",
      selected.display()
    ))
  })?;
  for directory in std::env::split_paths(&path) {
    for candidate in program_candidates(&directory, selection) {
      if candidate.is_file() {
        return Ok(candidate);
      }
    }
  }
  Err(RailError::message(format!(
    "executable '{}' was not found in PATH",
    selected.display()
  )))
}

#[cfg(not(windows))]
fn program_candidates(directory: &Path, selection: &OsStr) -> Vec<PathBuf> {
  vec![directory.join(selection)]
}

#[cfg(windows)]
fn program_candidates(directory: &Path, selection: &OsStr) -> Vec<PathBuf> {
  let selected = Path::new(selection);
  if selected.extension().is_some() {
    return vec![directory.join(selected)];
  }
  let extensions = std::env::var_os("PATHEXT").unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
  extensions
    .to_string_lossy()
    .split(';')
    .filter(|extension| !extension.is_empty())
    .map(|extension| directory.join(format!("{}{}", selected.to_string_lossy(), extension)))
    .collect()
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
  true
}

fn shebang_interpreter(shebang: &[u8]) -> RailResult<Option<String>> {
  let line = shebang.split(|byte| *byte == b'\n').next().unwrap_or_default();
  let line = std::str::from_utf8(line)
    .map_err(|_| RailError::message("script shebang is not valid UTF-8"))?
    .trim();
  Ok(line.split_ascii_whitespace().next().map(str::to_string))
}

fn is_env_interpreter(interpreter: &str) -> bool {
  Path::new(interpreter).file_name().is_some_and(|name| name == "env")
}

fn portable_selection(selection: &str, current_dir: &Path, source_root: &Path) -> String {
  let path = Path::new(selection);
  if path.is_absolute() {
    portable_path(path, source_root)
  } else if path.components().count() > 1 {
    portable_path(&current_dir.join(path), source_root)
  } else {
    format!("path:{selection}")
  }
}

fn portable_path(path: &Path, source_root: &Path) -> String {
  let repository_relative = path.strip_prefix(source_root).ok().or_else(|| {
    let canonical_root = source_root.canonicalize().ok()?;
    path.strip_prefix(canonical_root).ok()
  });
  repository_relative
    .map(|relative| format!("repository:{}", crate::utils::path_to_git_format(relative)))
    .unwrap_or_else(|| format!("host:{}", crate::utils::path_to_git_format(path)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn executable_identity_uses_logical_repository_paths() {
    let directory = tempfile::tempdir().expect("tempdir");
    let program = directory.path().join("tool");
    fs::write(&program, b"native-tool").expect("write tool");
    let identity = ExecutableIdentity::capture(&program.into_os_string(), directory.path(), directory.path())
      .expect("capture executable");
    let serialized = String::from_utf8(identity.identity_bytes().expect("identity bytes")).expect("utf8 JSON");

    assert!(serialized.contains("repository:tool"));
    assert!(!serialized.contains(&directory.path().to_string_lossy().to_string()));
  }

  #[test]
  fn executable_identity_changes_with_exact_bytes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let program = directory.path().join("tool");
    fs::write(&program, b"first").expect("write tool");
    let first =
      ExecutableIdentity::capture(program.as_os_str(), directory.path(), directory.path()).expect("first identity");
    fs::write(&program, b"second").expect("mutate tool");
    let second =
      ExecutableIdentity::capture(program.as_os_str(), directory.path(), directory.path()).expect("second identity");

    assert_ne!(first, second);
  }

  #[test]
  fn executable_digest_streams_exact_bytes_and_retains_only_the_shebang() {
    let directory = tempfile::tempdir().expect("tempdir");
    let program = directory.path().join("tool");
    for (bytes, expected_shebang) in [
      (b"#".to_vec(), None),
      (b"#!/bin/sh\npayload\xff".to_vec(), Some(b"/bin/sh\n".to_vec())),
      (vec![0x5a; 256 * 1024], None),
    ] {
      fs::write(&program, &bytes).expect("write tool");
      let (digest, shebang) = digest_executable(&program).expect("digest executable");
      assert_eq!(digest, ContentDigest::sha256(&bytes));
      assert_eq!(shebang, expected_shebang);
    }
  }
}
