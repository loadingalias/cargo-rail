//! Canonical native-result descriptors and private local-CAS staging.

use std::convert::TryFrom as _;
use std::fs::File;
use std::path::Path;

use super::{
  CDYLIB_SLOT, DEP_INFO_SLOT, DYLIB_SLOT, EXECUTABLE_SLOT, MAX_SOURCE_ENTRIES, METADATA_SLOT, NativeCompilerOutput,
  NativeCompilerWitness, NativeResultIdentity, PROC_MACRO_SLOT, RESULT_KEY_PREFIX, RLIB_SLOT, STATICLIB_SLOT,
  sha256_identity, validate_action_key, validate_sha256,
};
use crate::error::{RailError, RailResult};

const DESCRIPTOR_MAGIC: &[u8; 8] = b"CRNDESC1";
const DESCRIPTOR_VERSION: u16 = 5;
const IDENTITY_CONTRACT_VERSION: u16 = 10;
const RESULT_CLASS_VERSION: u16 = 3;
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_STRING_BYTES: usize = 4 * 1024;
const FIXED_DESCRIPTOR_PREFIX_BYTES: usize = 8 + 2 + 2 + 2 + 2;
const MAX_RESULT_PAYLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 16 * 1024 * 1024;

const SLOT_DEP_INFO: u8 = 1;
const SLOT_METADATA: u8 = 2;
const SLOT_RLIB: u8 = 3;
const SLOT_STDOUT: u8 = 4;
const SLOT_STDERR: u8 = 5;
const SLOT_EXECUTABLE: u8 = 6;
const SLOT_PROC_MACRO: u8 = 7;
const SLOT_DYLIB: u8 = 8;
const SLOT_CDYLIB: u8 = 9;
const SLOT_STATICLIB: u8 = 10;
const STREAM_SLOT_MODE: u32 = 0o644;

/// Private payload staging protected by the local CAS active-file lease.
pub(crate) struct NativeResultStaging {
  directory: tempfile::TempDir,
  active: Option<File>,
}

impl NativeResultStaging {
  pub(crate) fn guarded(directory: tempfile::TempDir, active: File) -> Self {
    Self {
      directory,
      active: Some(active),
    }
  }

  pub(super) fn path(&self) -> &Path {
    self.directory.path()
  }

  pub(super) const fn requires_durable_handoff(&self) -> bool {
    true
  }

  pub(super) fn into_parts(self) -> (tempfile::TempDir, Option<File>) {
    (self.directory, self.active)
  }
}

/// The semantic descriptor used by local result identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeResultDescriptor {
  pub(super) action_key: String,
  pub(super) witness: NativeCompilerWitness,
  pub(super) outputs: Vec<NativeCompilerOutput>,
  pub(super) stdout_digest: String,
  pub(super) stdout_bytes: u64,
  pub(super) stderr_digest: String,
  pub(super) stderr_bytes: u64,
}

impl NativeResultDescriptor {
  pub(super) fn from_identity(identity: NativeResultIdentity<'_>) -> RailResult<Self> {
    let descriptor = Self {
      action_key: identity.action_key.to_string(),
      witness: identity.witness.clone(),
      outputs: identity.outputs.to_vec(),
      stdout_digest: identity.stdout_digest.to_string(),
      stdout_bytes: identity.stdout_bytes,
      stderr_digest: identity.stderr_digest.to_string(),
      stderr_bytes: identity.stderr_bytes,
    };
    descriptor.validate()?;
    Ok(descriptor)
  }

  pub(super) fn result_key(&self) -> RailResult<String> {
    let descriptor = self.encode()?;
    Ok(sha256_identity(
      RESULT_KEY_PREFIX,
      b"cargo-rail-native-compiler-result\0",
      &[(b"version", &7_u32.to_le_bytes()), (b"descriptor", &descriptor)],
    ))
  }

  fn encode(&self) -> RailResult<Vec<u8>> {
    self.validate()?;
    let mut bytes = Vec::with_capacity(self.encoded_len()?);
    bytes.extend_from_slice(DESCRIPTOR_MAGIC);
    bytes.extend_from_slice(&DESCRIPTOR_VERSION.to_le_bytes());
    bytes.extend_from_slice(&IDENTITY_CONTRACT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&RESULT_CLASS_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    push_string(&mut bytes, &self.action_key)?;
    let witness_version = u16::try_from(self.witness.version)
      .map_err(|_| RailError::message("native result descriptor witness version is out of range"))?;
    bytes.extend_from_slice(&witness_version.to_le_bytes());
    bytes.push(u8::from(self.witness.complete));
    push_strings(&mut bytes, &self.witness.source_paths)?;
    push_strings(&mut bytes, &self.witness.generated_paths)?;
    push_strings(&mut bytes, &self.witness.dependency_names)?;
    push_strings(&mut bytes, &self.witness.environment_names)?;
    push_bytes(&mut bytes, &serde_json::to_vec(&self.witness.apple_linker)?)?;
    bytes.push(
      u8::try_from(self.outputs.len() + 2)
        .map_err(|_| RailError::message("native result descriptor slot count is out of range"))?,
    );
    for output in &self.outputs {
      let kind = match output.slot.as_str() {
        DEP_INFO_SLOT => SLOT_DEP_INFO,
        METADATA_SLOT => SLOT_METADATA,
        RLIB_SLOT => SLOT_RLIB,
        EXECUTABLE_SLOT => SLOT_EXECUTABLE,
        PROC_MACRO_SLOT => SLOT_PROC_MACRO,
        DYLIB_SLOT => SLOT_DYLIB,
        CDYLIB_SLOT => SLOT_CDYLIB,
        STATICLIB_SLOT => SLOT_STATICLIB,
        _ => {
          return Err(RailError::message(
            "native result descriptor contains an unknown output slot",
          ));
        }
      };
      push_slot(&mut bytes, kind, output.mode, output.bytes, &output.content_digest)?;
      push_string(&mut bytes, &output.file_name)?;
    }
    push_slot(
      &mut bytes,
      SLOT_STDOUT,
      STREAM_SLOT_MODE,
      self.stdout_bytes,
      &self.stdout_digest,
    )?;
    push_slot(
      &mut bytes,
      SLOT_STDERR,
      STREAM_SLOT_MODE,
      self.stderr_bytes,
      &self.stderr_digest,
    )?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
      return Err(RailError::message("native result descriptor exceeds its byte bound"));
    }
    Ok(bytes)
  }

  fn encoded_len(&self) -> RailResult<usize> {
    let strings = std::iter::once(&self.action_key)
      .chain(self.witness.source_paths.iter())
      .chain(self.witness.generated_paths.iter())
      .chain(self.witness.dependency_names.iter())
      .chain(self.witness.environment_names.iter())
      .chain(self.outputs.iter().map(|output| &output.file_name))
      .try_fold(0usize, |total, value| {
        total
          .checked_add(2)
          .and_then(|total| total.checked_add(value.len()))
          .ok_or_else(|| RailError::message("native result descriptor size overflow"))
      })?;
    let apple_linker = serde_json::to_vec(&self.witness.apple_linker)?;
    FIXED_DESCRIPTOR_PREFIX_BYTES
      .checked_add(strings)
      .and_then(|total| total.checked_add(2 + 1 + 4 * 4 + 1))
      .and_then(|total| total.checked_add(4 + apple_linker.len()))
      .and_then(|total| total.checked_add((self.outputs.len() + 2) * (1 + 2 + 8 + 32)))
      .ok_or_else(|| RailError::message("native result descriptor size overflow"))
  }

  fn validate(&self) -> RailResult<()> {
    validate_action_key(&self.action_key)?;
    if self.witness.version != 3
      || !self.witness.complete
      || self.witness.source_paths.is_empty()
      || self.witness.source_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.generated_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.dependency_names.len() > MAX_SOURCE_ENTRIES
      || self.witness.environment_names.len() > MAX_SOURCE_ENTRIES
      || !strictly_sorted(&self.witness.source_paths)
      || !strictly_sorted(&self.witness.generated_paths)
      || !strictly_sorted(&self.witness.dependency_names)
      || !strictly_sorted(&self.witness.environment_names)
      || self
        .witness
        .apple_linker
        .as_ref()
        .is_some_and(|witness| super::validate_apple_linker_witness(witness).is_err())
    {
      return Err(RailError::message("native result descriptor has an invalid witness"));
    }
    let contract = self.outputs.as_slice();
    let valid_contract = matches!(contract, [dep_info, metadata]
        if dep_info.role == "dep_info" && dep_info.slot == DEP_INFO_SLOT
          && metadata.role == "metadata" && metadata.slot == METADATA_SLOT)
      || matches!(contract, [dep_info, metadata, rlib]
        if dep_info.role == "dep_info" && dep_info.slot == DEP_INFO_SLOT
          && metadata.role == "metadata" && metadata.slot == METADATA_SLOT
          && rlib.role == "rlib" && rlib.slot == RLIB_SLOT)
      || matches!(contract, [dep_info, linked]
      if dep_info.role == "dep_info" && dep_info.slot == DEP_INFO_SLOT
        && matches!(
          (linked.role.as_str(), linked.slot.as_str()),
          ("executable", EXECUTABLE_SLOT)
            | ("proc_macro", PROC_MACRO_SLOT)
            | ("dylib", DYLIB_SLOT)
            | ("cdylib", CDYLIB_SLOT)
            | ("staticlib", STATICLIB_SLOT)
        ));
    if !valid_contract
      || self.outputs.iter().any(|output| {
        output.bytes == 0
          || output.file_name.is_empty()
          || output.file_name.len() > MAX_DESCRIPTOR_STRING_BYTES
          || output.file_name.as_bytes().contains(&0)
          || !super::valid_native_output_mode(&output.role, output.mode)
      })
    {
      return Err(RailError::message(
        "native result descriptor has an invalid output contract",
      ));
    }
    for digest in self
      .outputs
      .iter()
      .map(|output| output.content_digest.as_str())
      .chain([self.stdout_digest.as_str(), self.stderr_digest.as_str()])
    {
      validate_sha256(digest)?;
    }
    let payload = self
      .outputs
      .iter()
      .map(|output| output.bytes)
      .chain([self.stdout_bytes, self.stderr_bytes])
      .try_fold(0_u64, |total, bytes| total.checked_add(bytes))
      .ok_or_else(|| RailError::message("native result descriptor payload size overflow"))?;
    if payload > MAX_RESULT_PAYLOAD_BYTES
      || self.stdout_bytes > MAX_STREAM_BYTES
      || self.stderr_bytes > MAX_STREAM_BYTES
    {
      return Err(RailError::message(
        "native result descriptor payload exceeds its byte bound",
      ));
    }
    for value in self
      .witness
      .source_paths
      .iter()
      .chain(&self.witness.generated_paths)
      .chain(&self.witness.dependency_names)
      .chain(&self.witness.environment_names)
    {
      if value.is_empty() || value.len() > MAX_DESCRIPTOR_STRING_BYTES || value.as_bytes().contains(&0) {
        return Err(RailError::message(
          "native result descriptor contains an invalid witness string",
        ));
      }
    }
    for path in self.witness.source_paths.iter().chain(&self.witness.generated_paths) {
      if !super::native_relative_path(Path::new(path)).is_ok_and(|canonical| canonical == *path) {
        return Err(RailError::message(
          "native result descriptor contains an invalid source capability",
        ));
      }
    }
    if self.encoded_len()? > MAX_DESCRIPTOR_BYTES {
      return Err(RailError::message("native result descriptor exceeds its byte bound"));
    }
    Ok(())
  }
}

fn strictly_sorted(values: &[String]) -> bool {
  values.windows(2).all(|pair| pair[0] < pair[1])
}

fn push_strings(output: &mut Vec<u8>, values: &[String]) -> RailResult<()> {
  let count = u32::try_from(values.len())
    .map_err(|_| RailError::message("native result descriptor witness count is out of range"))?;
  output.extend_from_slice(&count.to_le_bytes());
  for value in values {
    push_string(output, value)?;
  }
  Ok(())
}

fn push_string(output: &mut Vec<u8>, value: &str) -> RailResult<()> {
  if value.len() > MAX_DESCRIPTOR_STRING_BYTES {
    return Err(RailError::message(
      "native result descriptor string exceeds its byte bound",
    ));
  }
  let length = u16::try_from(value.len())
    .map_err(|_| RailError::message("native result descriptor string length is out of range"))?;
  output.extend_from_slice(&length.to_le_bytes());
  output.extend_from_slice(value.as_bytes());
  Ok(())
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) -> RailResult<()> {
  let length = u32::try_from(value.len())
    .map_err(|_| RailError::message("native result descriptor byte field is out of range"))?;
  output.extend_from_slice(&length.to_le_bytes());
  output.extend_from_slice(value);
  Ok(())
}

fn push_slot(output: &mut Vec<u8>, kind: u8, mode: u32, length: u64, digest: &str) -> RailResult<()> {
  let mode = u16::try_from(mode).map_err(|_| RailError::message("native result slot mode is out of range"))?;
  output.push(kind);
  output.extend_from_slice(&mode.to_le_bytes());
  output.extend_from_slice(&length.to_le_bytes());
  output.extend_from_slice(&decode_digest(digest)?);
  Ok(())
}

fn decode_digest(value: &str) -> RailResult<[u8; 32]> {
  validate_sha256(value)?;
  let value = value
    .strip_prefix("sha256:")
    .ok_or_else(|| RailError::message("native result descriptor digest has an invalid domain"))?;
  let mut digest = [0_u8; 32];
  for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
    digest[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
  }
  Ok(digest)
}

fn hex_nibble(value: u8) -> RailResult<u8> {
  match value {
    b'0'..=b'9' => Ok(value - b'0'),
    b'a'..=b'f' => Ok(value - b'a' + 10),
    _ => Err(RailError::message(
      "native result descriptor digest is not lowercase hexadecimal",
    )),
  }
}
