//! Canonical native-result descriptor and fixed result-pack framing.

use std::convert::TryFrom as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use super::{
  DEP_INFO_SLOT, MAX_SOURCE_ENTRIES, METADATA_SLOT, NativeCompilerOutput, NativeCompilerWitness, NativeResultIdentity,
  RESULT_KEY_PREFIX, RLIB_SLOT, STDERR_SLOT, STDOUT_SLOT, sha256_identity, validate_action_key, validate_result_key,
  validate_sha256,
};
use crate::error::{RailError, RailResult};
use sha2::{Digest as _, Sha256};

const DESCRIPTOR_MAGIC: &[u8; 8] = b"CRNDESC1";
const DESCRIPTOR_VERSION: u16 = 2;
const IDENTITY_CONTRACT_VERSION: u16 = 7;
const RESULT_CLASS_VERSION: u16 = 2;
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_STRING_BYTES: usize = 4 * 1024;
const FIXED_DESCRIPTOR_PREFIX_BYTES: usize = 8 + 2 + 2 + 2 + 2;
const PACK_MAGIC: &[u8; 8] = b"CRNPACK1";
const PACK_VERSION: u16 = 2;
const PACK_PRELUDE_BYTES: u64 = 8 + 2 + 4;
const PACK_FRAME_HEADER_BYTES: u64 = 1 + 8;
const MAX_PACK_PAYLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 16 * 1024 * 1024;
const IO_BUFFER_BYTES: usize = 64 * 1024;

/// Absolute native-pack object bound: maximum descriptor, frame headers, and payload.
pub(crate) const MAX_PACK_BYTES: u64 =
  PACK_PRELUDE_BYTES + MAX_DESCRIPTOR_BYTES as u64 + 5 * PACK_FRAME_HEADER_BYTES + MAX_PACK_PAYLOAD_BYTES;

const SLOT_DEP_INFO: u8 = 1;
const SLOT_METADATA: u8 = 2;
const SLOT_RLIB: u8 = 3;
const SLOT_STDOUT: u8 = 4;
const SLOT_STDERR: u8 = 5;
const STREAM_SLOT_MODE: u32 = 0o644;

/// One fixed payload slot derived from the canonical descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePackSlot<'a> {
  kind: u8,
  pub(crate) path: &'static str,
  pub(crate) bytes: u64,
  pub(crate) digest: &'a str,
  pub(crate) mode: u32,
}

/// Verified pack metadata and private slot staging produced before CAS admission.
pub(crate) struct DecodedNativePack {
  pub(super) staging: NativeResultStaging,
  pub(super) descriptor: NativeResultDescriptor,
  pub(crate) bytes_read: u64,
}

/// Private payload staging, optionally protected by the local CAS active-file
/// lease so GC cannot race result preparation or import.
pub(crate) struct NativeResultStaging {
  directory: tempfile::TempDir,
  active: Option<File>,
  command_scoped: bool,
}

impl NativeResultStaging {
  pub(crate) fn guarded(directory: tempfile::TempDir, active: File) -> Self {
    Self {
      directory,
      active: Some(active),
      command_scoped: false,
    }
  }

  pub(crate) fn command_scoped(directory: tempfile::TempDir) -> Self {
    Self {
      directory,
      active: None,
      command_scoped: true,
    }
  }

  pub(super) fn temporary() -> RailResult<Self> {
    Ok(Self {
      directory: tempfile::Builder::new().prefix("native-pack-").tempdir()?,
      active: None,
      command_scoped: false,
    })
  }

  pub(super) fn path(&self) -> &Path {
    self.directory.path()
  }

  pub(super) fn requires_durable_handoff(&self) -> bool {
    !self.command_scoped
  }

  pub(super) fn into_parts(self) -> (tempfile::TempDir, Option<File>, bool) {
    (self.directory, self.active, self.command_scoped)
  }
}

/// Exact pack length and bytes emitted by one streaming export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePackExport {
  pub(crate) content_length: u64,
  pub(crate) bytes_written: u64,
}

/// Canonical association body shared byte-for-byte with the pack descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeAssociation {
  bytes: Vec<u8>,
  action_key: String,
  result_key: String,
  pack_length: u64,
}

impl NativeAssociation {
  #[cfg(test)]
  pub(crate) fn bytes(&self) -> &[u8] {
    &self.bytes
  }

  pub(crate) fn action_key(&self) -> &str {
    &self.action_key
  }

  pub(crate) fn result_key(&self) -> &str {
    &self.result_key
  }

  pub(crate) const fn pack_length(&self) -> u64 {
    self.pack_length
  }
}

/// The one semantic descriptor used by local result identity, associations,
/// and the fixed pack header. It contains no local CAS or producer telemetry.
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

pub(crate) fn association(validation: &super::NativeCompilerValidation) -> RailResult<NativeAssociation> {
  validation.validate_object()?;
  let descriptor = NativeResultDescriptor::from_identity(NativeResultIdentity {
    action_key: validation.action_key(),
    witness: &validation.witness,
    outputs: &validation.outputs,
    stdout_digest: &validation.stdout_digest,
    stdout_bytes: validation.stdout_bytes,
    stderr_digest: &validation.stderr_digest,
    stderr_bytes: validation.stderr_bytes,
  })?;
  let bytes = descriptor.encode()?;
  let result_key = descriptor.result_key()?;
  if result_key != validation.result_key() {
    return Err(RailError::message(
      "native association does not match its verified result",
    ));
  }
  let pack_length = descriptor.pack_length()?;
  Ok(NativeAssociation {
    bytes,
    action_key: descriptor.action_key,
    result_key,
    pack_length,
  })
}

#[cfg(test)]
pub(crate) fn decode_association(
  bytes: &[u8],
  expected_action: &str,
  expected_result: &str,
) -> RailResult<NativeAssociation> {
  validate_action_key(expected_action)?;
  validate_result_key(expected_result)?;
  let descriptor = NativeResultDescriptor::decode(bytes)?;
  let result_key = descriptor.result_key()?;
  if descriptor.action_key != expected_action || result_key != expected_result {
    return Err(RailError::message(
      "native association descriptor does not match its action/result key",
    ));
  }
  let pack_length = descriptor.pack_length()?;
  Ok(NativeAssociation {
    bytes: bytes.to_vec(),
    action_key: descriptor.action_key,
    result_key,
    pack_length,
  })
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

  pub(super) fn encode(&self) -> RailResult<Vec<u8>> {
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
    push_strings(&mut bytes, &self.witness.dependency_names)?;
    push_strings(&mut bytes, &self.witness.environment_names)?;
    bytes.push(
      u8::try_from(self.outputs.len() + 2)
        .map_err(|_| RailError::message("native result descriptor slot count is out of range"))?,
    );
    for output in &self.outputs {
      let kind = match output.slot.as_str() {
        DEP_INFO_SLOT => SLOT_DEP_INFO,
        METADATA_SLOT => SLOT_METADATA,
        RLIB_SLOT => SLOT_RLIB,
        _ => {
          return Err(RailError::message(
            "native result descriptor contains an unknown output slot",
          ));
        }
      };
      push_slot(&mut bytes, kind, output.mode, output.bytes, &output.content_digest)?;
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

  pub(super) fn result_key(&self) -> RailResult<String> {
    let descriptor = self.encode()?;
    Ok(sha256_identity(
      RESULT_KEY_PREFIX,
      b"cargo-rail-native-compiler-result\0",
      &[(b"version", &4_u32.to_le_bytes()), (b"descriptor", &descriptor)],
    ))
  }

  pub(crate) fn slots(&self) -> Vec<NativePackSlot<'_>> {
    self
      .outputs
      .iter()
      .map(|output| NativePackSlot {
        kind: match output.slot.as_str() {
          DEP_INFO_SLOT => SLOT_DEP_INFO,
          METADATA_SLOT => SLOT_METADATA,
          RLIB_SLOT => SLOT_RLIB,
          _ => 0,
        },
        path: match output.slot.as_str() {
          DEP_INFO_SLOT => DEP_INFO_SLOT,
          METADATA_SLOT => METADATA_SLOT,
          RLIB_SLOT => RLIB_SLOT,
          _ => "",
        },
        bytes: output.bytes,
        digest: &output.content_digest,
        mode: output.mode,
      })
      .chain([
        NativePackSlot {
          kind: SLOT_STDOUT,
          path: STDOUT_SLOT,
          bytes: self.stdout_bytes,
          digest: &self.stdout_digest,
          mode: STREAM_SLOT_MODE,
        },
        NativePackSlot {
          kind: SLOT_STDERR,
          path: STDERR_SLOT,
          bytes: self.stderr_bytes,
          digest: &self.stderr_digest,
          mode: STREAM_SLOT_MODE,
        },
      ])
      .collect()
  }

  pub(crate) fn pack_length(&self) -> RailResult<u64> {
    let descriptor = u64::try_from(self.encoded_len()?)
      .map_err(|_| RailError::message("native result descriptor size is out of range"))?;
    let slots = self.slots();
    let payload = slots.iter().try_fold(0_u64, |total, slot| {
      total
        .checked_add(slot.bytes)
        .ok_or_else(|| RailError::message("native result pack payload size overflow"))
    })?;
    if payload > MAX_PACK_PAYLOAD_BYTES {
      return Err(RailError::message("native result pack exceeds its payload bound"));
    }
    PACK_PRELUDE_BYTES
      .checked_add(descriptor)
      .and_then(|total| total.checked_add(PACK_FRAME_HEADER_BYTES * slots.len() as u64))
      .and_then(|total| total.checked_add(payload))
      .ok_or_else(|| RailError::message("native result pack size overflow"))
  }

  pub(super) fn decode(bytes: &[u8]) -> RailResult<Self> {
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
      return Err(RailError::message("native result descriptor exceeds its byte bound"));
    }
    let mut input = Decoder::new(bytes);
    input.expect(DESCRIPTOR_MAGIC)?;
    if input.u16()? != DESCRIPTOR_VERSION
      || input.u16()? != IDENTITY_CONTRACT_VERSION
      || input.u16()? != RESULT_CLASS_VERSION
      || input.u16()? != 0
    {
      return Err(RailError::message(
        "native result descriptor has an incompatible prelude",
      ));
    }
    let action_key = input.string()?;
    let witness_version = u32::from(input.u16()?);
    let complete = match input.u8()? {
      0 => false,
      1 => true,
      _ => {
        return Err(RailError::message(
          "native result descriptor has an invalid witness flag",
        ));
      }
    };
    let witness = NativeCompilerWitness {
      version: witness_version,
      complete,
      source_paths: input.strings()?,
      dependency_names: input.strings()?,
      environment_names: input.strings()?,
    };
    let slot_count = usize::from(input.u8()?);
    if !matches!(slot_count, 4 | 5) {
      return Err(RailError::message("native result descriptor has an invalid slot count"));
    }
    let mut outputs = Vec::with_capacity(slot_count - 2);
    let mut stdout = None;
    let mut stderr = None;
    for index in 0..slot_count {
      let kind = input.u8()?;
      let mode = u32::from(input.u16()?);
      if (matches!(kind, SLOT_STDOUT | SLOT_STDERR) && mode != STREAM_SLOT_MODE)
        || (!matches!(kind, SLOT_STDOUT | SLOT_STDERR) && !super::valid_native_output_mode(mode))
      {
        return Err(RailError::message(
          "native result descriptor has an unsupported slot mode",
        ));
      }
      let length = input.u64()?;
      let digest = format!("sha256:{}", hex_digest(input.take(32)?));
      let expected_kind = if slot_count == 5 {
        [SLOT_DEP_INFO, SLOT_METADATA, SLOT_RLIB, SLOT_STDOUT, SLOT_STDERR][index]
      } else {
        [SLOT_DEP_INFO, SLOT_METADATA, SLOT_STDOUT, SLOT_STDERR][index]
      };
      if kind != expected_kind {
        return Err(RailError::message(
          "native result descriptor slots are unknown, omitted, duplicated, or reordered",
        ));
      }
      match kind {
        SLOT_DEP_INFO => outputs.push(output("dep_info", DEP_INFO_SLOT, digest, length, mode)),
        SLOT_METADATA => outputs.push(output("metadata", METADATA_SLOT, digest, length, mode)),
        SLOT_RLIB => outputs.push(output("rlib", RLIB_SLOT, digest, length, mode)),
        SLOT_STDOUT => stdout = Some((digest, length)),
        SLOT_STDERR => stderr = Some((digest, length)),
        _ => return Err(RailError::message("native result descriptor contains an unknown slot")),
      }
    }
    input.finish()?;
    let (stdout_digest, stdout_bytes) =
      stdout.ok_or_else(|| RailError::message("native result descriptor omits stdout"))?;
    let (stderr_digest, stderr_bytes) =
      stderr.ok_or_else(|| RailError::message("native result descriptor omits stderr"))?;
    let descriptor = Self {
      action_key,
      witness,
      outputs,
      stdout_digest,
      stdout_bytes,
      stderr_digest,
      stderr_bytes,
    };
    descriptor.validate()?;
    if descriptor.encode()? != bytes {
      return Err(RailError::message(
        "native result descriptor is not canonically encoded",
      ));
    }
    Ok(descriptor)
  }

  fn encoded_len(&self) -> RailResult<usize> {
    let strings = std::iter::once(&self.action_key)
      .chain(self.witness.source_paths.iter())
      .chain(self.witness.dependency_names.iter())
      .chain(self.witness.environment_names.iter())
      .try_fold(0usize, |total, value| {
        total
          .checked_add(2)
          .and_then(|total| total.checked_add(value.len()))
          .ok_or_else(|| RailError::message("native result descriptor size overflow"))
      })?;
    FIXED_DESCRIPTOR_PREFIX_BYTES
      .checked_add(strings)
      .and_then(|total| total.checked_add(2 + 1 + 3 * 4 + 1))
      .and_then(|total| total.checked_add((self.outputs.len() + 2) * (1 + 2 + 8 + 32)))
      .ok_or_else(|| RailError::message("native result descriptor size overflow"))
  }

  fn validate(&self) -> RailResult<()> {
    validate_action_key(&self.action_key)?;
    if self.witness.version != 1
      || !self.witness.complete
      || self.witness.source_paths.is_empty()
      || self.witness.source_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.dependency_names.len() > MAX_SOURCE_ENTRIES
      || self.witness.environment_names.len() > MAX_SOURCE_ENTRIES
      || !strictly_sorted(&self.witness.source_paths)
      || !strictly_sorted(&self.witness.dependency_names)
      || !strictly_sorted(&self.witness.environment_names)
    {
      return Err(RailError::message("native result descriptor has an invalid witness"));
    }
    let expected = if self.outputs.len() == 3 {
      &[
        ("dep_info", DEP_INFO_SLOT),
        ("metadata", METADATA_SLOT),
        ("rlib", RLIB_SLOT),
      ][..]
    } else {
      &[("dep_info", DEP_INFO_SLOT), ("metadata", METADATA_SLOT)][..]
    };
    if self.outputs.len() != expected.len()
      || self.outputs.iter().zip(expected).any(|(output, (role, slot))| {
        output.role != *role
          || output.slot != *slot
          || output.bytes == 0
          || !super::valid_native_output_mode(output.mode)
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
    if payload > MAX_PACK_PAYLOAD_BYTES || self.stdout_bytes > MAX_STREAM_BYTES || self.stderr_bytes > MAX_STREAM_BYTES
    {
      return Err(RailError::message(
        "native result descriptor payload exceeds its byte bound",
      ));
    }
    for value in self
      .witness
      .source_paths
      .iter()
      .chain(&self.witness.dependency_names)
      .chain(&self.witness.environment_names)
    {
      if value.is_empty() || value.len() > MAX_DESCRIPTOR_STRING_BYTES || value.as_bytes().contains(&0) {
        return Err(RailError::message(
          "native result descriptor contains an invalid witness string",
        ));
      }
    }
    for path in &self.witness.source_paths {
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

/// Stream one canonical pack from immutable, caller-opened slot files.
pub(crate) fn export<W, F>(
  validation: &super::NativeCompilerValidation,
  mut writer: W,
  mut open_slot: F,
) -> RailResult<NativePackExport>
where
  W: Write,
  F: for<'a> FnMut(NativePackSlot<'a>) -> RailResult<File>,
{
  validation.validate_object()?;
  let descriptor = NativeResultDescriptor::from_identity(NativeResultIdentity {
    action_key: validation.action_key(),
    witness: &validation.witness,
    outputs: &validation.outputs,
    stdout_digest: &validation.stdout_digest,
    stdout_bytes: validation.stdout_bytes,
    stderr_digest: &validation.stderr_digest,
    stderr_bytes: validation.stderr_bytes,
  })?;
  if descriptor.result_key()? != validation.result_key() {
    return Err(RailError::message(
      "native result descriptor identity changed before export",
    ));
  }
  let descriptor_bytes = descriptor.encode()?;
  let content_length = descriptor.pack_length()?;
  writer.write_all(PACK_MAGIC)?;
  writer.write_all(&PACK_VERSION.to_le_bytes())?;
  let header_length = u32::try_from(descriptor_bytes.len())
    .map_err(|_| RailError::message("native result pack header length is out of range"))?;
  writer.write_all(&header_length.to_le_bytes())?;
  writer.write_all(&descriptor_bytes)?;
  let mut bytes_written = PACK_PRELUDE_BYTES + u64::from(header_length);
  for slot in descriptor.slots() {
    writer.write_all(&[slot.kind])?;
    writer.write_all(&slot.bytes.to_le_bytes())?;
    bytes_written = bytes_written.saturating_add(PACK_FRAME_HEADER_BYTES);
    let mut reader = open_slot(slot)?;
    let payload_written = copy_exact_digest(&mut reader, &mut writer, slot.bytes, slot.digest)?;
    if payload_written != slot.bytes {
      return Err(RailError::message(
        "native result pack encoder wrote an inconsistent payload length",
      ));
    }
    bytes_written = bytes_written.saturating_add(slot.bytes);
  }
  if bytes_written != content_length {
    return Err(RailError::message(
      "native result pack encoder produced an inconsistent length",
    ));
  }
  Ok(NativePackExport {
    content_length,
    bytes_written,
  })
}

/// Decode a canonical pack into private staging without granting local authority.
#[cfg(test)]
pub(crate) fn decode<R: Read>(
  reader: R,
  expected_action: &str,
  expected_result: &str,
  declared_length: Option<u64>,
  staging: Option<NativeResultStaging>,
) -> RailResult<DecodedNativePack> {
  decode_counted_inner(reader, expected_action, Some(expected_result), declared_length, staging)
    .map(|(decoded, _, _)| decoded)
}

/// Decode one complete canonical pack bound to `expected_action` and derive
/// its result identity from the verified descriptor and payload.
///
/// This is the only admissible way for remote storage to inspect an orphan
/// result whose action state has not yet named a result identity.
pub(crate) fn decode_for_action<R: Read>(
  reader: R,
  expected_action: &str,
  declared_length: Option<u64>,
  staging: Option<NativeResultStaging>,
) -> RailResult<(DecodedNativePack, NativeAssociation)> {
  decode_counted_inner(reader, expected_action, None, declared_length, staging)
    .map(|(decoded, association, _)| (decoded, association))
}

#[cfg(test)]
fn decode_counted<R: Read>(
  reader: R,
  expected_action: &str,
  expected_result: &str,
  declared_length: Option<u64>,
  staging: Option<NativeResultStaging>,
) -> RailResult<(DecodedNativePack, u64)> {
  decode_counted_inner(reader, expected_action, Some(expected_result), declared_length, staging)
    .map(|(decoded, _, bytes)| (decoded, bytes))
}

fn decode_counted_inner<R: Read>(
  mut reader: R,
  expected_action: &str,
  expected_result: Option<&str>,
  declared_length: Option<u64>,
  staging: Option<NativeResultStaging>,
) -> RailResult<(DecodedNativePack, NativeAssociation, u64)> {
  validate_action_key(expected_action)?;
  if let Some(expected_result) = expected_result {
    validate_result_key(expected_result)?;
  }
  let mut prelude = [0_u8; PACK_PRELUDE_BYTES as usize];
  reader
    .read_exact(&mut prelude)
    .map_err(|error| RailError::message(format!("native result pack prelude is truncated: {error}")))?;
  if &prelude[..8] != PACK_MAGIC || u16::from_le_bytes([prelude[8], prelude[9]]) != PACK_VERSION {
    return Err(RailError::message("native result pack has an incompatible prelude"));
  }
  let header_length = u32::from_le_bytes(
    prelude[10..14]
      .try_into()
      .map_err(|_| RailError::message("native result pack has a malformed header length"))?,
  ) as usize;
  if header_length > MAX_DESCRIPTOR_BYTES {
    return Err(RailError::message(
      "native result pack descriptor exceeds its byte bound",
    ));
  }
  let mut descriptor_bytes = vec![0_u8; header_length];
  reader
    .read_exact(&mut descriptor_bytes)
    .map_err(|error| RailError::message(format!("native result pack descriptor is truncated: {error}")))?;
  let descriptor = NativeResultDescriptor::decode(&descriptor_bytes)?;
  if descriptor.action_key != expected_action {
    return Err(RailError::message(
      "native result pack descriptor does not match the live action",
    ));
  }
  let result_key = descriptor.result_key()?;
  if expected_result.is_some_and(|expected| result_key != expected) {
    return Err(RailError::message(
      "native result pack descriptor does not match its result object key",
    ));
  }
  let exact_length = descriptor.pack_length()?;
  if declared_length.is_some_and(|declared| declared != exact_length) {
    return Err(RailError::message(
      "native result pack declared length does not match its descriptor",
    ));
  }
  let staging = match staging {
    Some(staging) => staging,
    None => NativeResultStaging::temporary()?,
  };
  let mut bytes_read = PACK_PRELUDE_BYTES + header_length as u64;
  let mut bytes_staged = 0_u64;
  for slot in descriptor.slots() {
    let mut frame = [0_u8; PACK_FRAME_HEADER_BYTES as usize];
    reader
      .read_exact(&mut frame)
      .map_err(|error| RailError::message(format!("native result pack frame is truncated: {error}")))?;
    let length = u64::from_le_bytes(
      frame[1..]
        .try_into()
        .map_err(|_| RailError::message("native result pack has a malformed frame length"))?,
    );
    if frame[0] != slot.kind || length != slot.bytes {
      return Err(RailError::message(
        "native result pack frames are unknown, omitted, duplicated, reordered, or inconsistent",
      ));
    }
    bytes_read = bytes_read.saturating_add(PACK_FRAME_HEADER_BYTES);
    let path = staging.path().join(slot.path);
    let parent = path
      .parent()
      .ok_or_else(|| RailError::message("native result pack slot has no staging parent"))?;
    fs::create_dir_all(parent)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(&path)?;
    let staged = copy_exact_digest(&mut reader, &mut output, slot.bytes, slot.digest)?;
    bytes_staged = bytes_staged
      .checked_add(staged)
      .ok_or_else(|| RailError::message("native result pack staged-byte count overflow"))?;
    output.sync_all()?;
    set_private_output_mode(&path, slot.mode)?;
    bytes_read = bytes_read.saturating_add(slot.bytes);
  }
  let mut trailing = [0_u8; 1];
  if reader.read(&mut trailing)? != 0 {
    return Err(RailError::message("native result pack contains trailing bytes"));
  }
  if bytes_read != exact_length {
    return Err(RailError::message(
      "native result pack decoder consumed an inconsistent length",
    ));
  }
  let association = NativeAssociation {
    bytes: descriptor_bytes,
    action_key: descriptor.action_key.clone(),
    result_key,
    pack_length: exact_length,
  };
  Ok((
    DecodedNativePack {
      staging,
      descriptor,
      bytes_read,
    },
    association,
    bytes_staged,
  ))
}

fn copy_exact_digest<R: Read, W: Write>(
  reader: &mut R,
  writer: &mut W,
  expected_bytes: u64,
  expected_digest: &str,
) -> RailResult<u64> {
  let mut hasher = Sha256::new();
  let mut copied = 0_u64;
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  while copied < expected_bytes {
    let remaining = expected_bytes - copied;
    let limit = usize::try_from(remaining.min(buffer.len() as u64))
      .map_err(|_| RailError::message("native result pack read bound is out of range"))?;
    let read = reader.read(&mut buffer[..limit])?;
    if read == 0 {
      return Err(RailError::message("native result pack payload is truncated"));
    }
    writer.write_all(&buffer[..read])?;
    hasher.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  let actual = format!("sha256:{}", hex_digest(&hasher.finalize()));
  if actual != expected_digest {
    return Err(RailError::message(
      "native result pack payload digest does not match its descriptor",
    ));
  }
  Ok(copied)
}

#[cfg(unix)]
fn set_private_output_mode(path: &Path, mode: u32) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
  Ok(())
}

#[cfg(not(unix))]
fn set_private_output_mode(path: &Path, mode: u32) -> RailResult<()> {
  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_readonly(mode & 0o200 == 0);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

fn output(role: &str, slot: &str, content_digest: String, bytes: u64, mode: u32) -> NativeCompilerOutput {
  NativeCompilerOutput {
    role: role.to_string(),
    slot: slot.to_string(),
    content_digest,
    bytes,
    mode,
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

fn hex_digest(value: &[u8]) -> String {
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let mut output = String::with_capacity(value.len() * 2);
  for byte in value {
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
  }
  output
}

struct Decoder<'a> {
  bytes: &'a [u8],
  offset: usize,
}

impl<'a> Decoder<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self { bytes, offset: 0 }
  }

  fn take(&mut self, length: usize) -> RailResult<&'a [u8]> {
    let end = self
      .offset
      .checked_add(length)
      .filter(|end| *end <= self.bytes.len())
      .ok_or_else(|| RailError::message("native result descriptor is truncated"))?;
    let value = &self.bytes[self.offset..end];
    self.offset = end;
    Ok(value)
  }

  fn expect(&mut self, expected: &[u8]) -> RailResult<()> {
    if self.take(expected.len())? == expected {
      Ok(())
    } else {
      Err(RailError::message("native result descriptor has invalid magic"))
    }
  }

  fn u8(&mut self) -> RailResult<u8> {
    Ok(self.take(1)?[0])
  }

  fn u16(&mut self) -> RailResult<u16> {
    let bytes: [u8; 2] = self
      .take(2)?
      .try_into()
      .map_err(|_| RailError::message("native result descriptor has a malformed integer"))?;
    Ok(u16::from_le_bytes(bytes))
  }

  fn u32(&mut self) -> RailResult<u32> {
    let bytes: [u8; 4] = self
      .take(4)?
      .try_into()
      .map_err(|_| RailError::message("native result descriptor has a malformed integer"))?;
    Ok(u32::from_le_bytes(bytes))
  }

  fn u64(&mut self) -> RailResult<u64> {
    let bytes: [u8; 8] = self
      .take(8)?
      .try_into()
      .map_err(|_| RailError::message("native result descriptor has a malformed integer"))?;
    Ok(u64::from_le_bytes(bytes))
  }

  fn string(&mut self) -> RailResult<String> {
    let length = usize::from(self.u16()?);
    if length > MAX_DESCRIPTOR_STRING_BYTES {
      return Err(RailError::message(
        "native result descriptor string exceeds its byte bound",
      ));
    }
    let value = std::str::from_utf8(self.take(length)?)
      .map_err(|_| RailError::message("native result descriptor string is not UTF-8"))?;
    Ok(value.to_string())
  }

  fn strings(&mut self) -> RailResult<Vec<String>> {
    let count = usize::try_from(self.u32()?)
      .map_err(|_| RailError::message("native result descriptor witness count is out of range"))?;
    if count > MAX_SOURCE_ENTRIES {
      return Err(RailError::message(
        "native result descriptor witness exceeds its entry bound",
      ));
    }
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
      values.push(self.string()?);
    }
    Ok(values)
  }

  fn finish(&self) -> RailResult<()> {
    if self.offset == self.bytes.len() {
      Ok(())
    } else {
      Err(RailError::message("native result descriptor contains trailing bytes"))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::io::{Cursor, Seek as _};

  use super::*;

  const CANONICAL_ACTION_KEY: &str =
    "compiler-action-v8-sha256-6043c4575263d08ee556606c6e9b02923e50cefacef05cf4463822ca7304bd41";
  const CANONICAL_RESULT_KEY: &str =
    "compiler-result-v6-sha256-ed907fa74ec90d5e7180d25e7998d80177c5ea8048730d2c900492da6599d52e";
  const CANONICAL_DESCRIPTOR_SHA256: &str = "996cf00730052557dd42c107fb460f769c5a39e7e623a8473f21ea3463c8c8d7";
  const CANONICAL_PACK_SHA256: &str = "4ac64622c422268110ad10e9c1cc663a0ac0afb17db60e2ceb34306a9520144e";
  const CANONICAL_DESCRIPTOR_HEX: &str = concat!(
    "43524e444553433102000700020000005a00",
    "636f6d70696c65722d616374696f6e2d76382d7368613235362d36303433633435373532363364303865653535363630366336653962303239323365353063656661636566303563663434363338323263613733303462643431",
    "0100010100000006006c69622e727300000000000000000401",
    "a4010800000000000000d708da5a10078e1c0ec92d1f93c4592e152f1639a0cdf3f545e02fd7e632588302",
    "a401080000000000000045447b7afbd5e544f7d0f1df0fccd26014d9850130abd3f020b89ff96b82079f04",
    "a4010f0000000000000050c560dce53c1b4f84780f00d3177742272b2d6cef2e80347fe8227320ff7c7c05",
    "a4010000000000000000e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
  const CANONICAL_PACK_PRELUDE_HEX: &str = "43524e5041434b31020030010000";
  const CANONICAL_PACK_FRAMES_HEX: &str = concat!(
    "0108000000000000006465702d696e666f",
    "0208000000000000006d65746164617461",
    "040f00000000000000706f727461626c65207374646f7574",
    "050000000000000000",
  );

  const ACTION_OFFSET: usize = 18;
  const ACTION_BYTES: usize = 90;
  const WITNESS_VERSION_OFFSET: usize = ACTION_OFFSET + ACTION_BYTES;
  const WITNESS_FLAG_OFFSET: usize = WITNESS_VERSION_OFFSET + 2;
  const SOURCE_COUNT_OFFSET: usize = WITNESS_FLAG_OFFSET + 1;
  const SOURCE_LENGTH_OFFSET: usize = SOURCE_COUNT_OFFSET + 4;
  const SOURCE_OFFSET: usize = SOURCE_LENGTH_OFFSET + 2;
  const DEPENDENCY_COUNT_OFFSET: usize = SOURCE_OFFSET + 6;
  const ENVIRONMENT_COUNT_OFFSET: usize = DEPENDENCY_COUNT_OFFSET + 4;
  const SLOT_COUNT_OFFSET: usize = ENVIRONMENT_COUNT_OFFSET + 4;
  const DESCRIPTOR_SLOT_OFFSETS: [usize; 4] = [132, 175, 218, 261];
  const PACK_DESCRIPTOR_OFFSET: usize = PACK_PRELUDE_BYTES as usize;
  const PACK_FRAME_OFFSETS: [usize; 4] = [318, 335, 352, 376];
  const PACK_END: usize = 385;

  fn literal_bytes(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "hex fixture must contain whole bytes");
    value
      .as_bytes()
      .chunks_exact(2)
      .map(|pair| {
        let pair = std::str::from_utf8(pair).expect("hex fixture must be UTF-8");
        u8::from_str_radix(pair, 16).expect("hex fixture must be lowercase hexadecimal")
      })
      .collect()
  }

  fn canonical_pack_bytes() -> Vec<u8> {
    let mut pack = literal_bytes(CANONICAL_PACK_PRELUDE_HEX);
    pack.extend_from_slice(&literal_bytes(CANONICAL_DESCRIPTOR_HEX));
    pack.extend_from_slice(&literal_bytes(CANONICAL_PACK_FRAMES_HEX));
    pack
  }

  fn digest(value: &[u8]) -> String {
    format!("sha256:{}", hex_digest(&Sha256::digest(value)))
  }

  fn canonical_descriptor() -> NativeResultDescriptor {
    NativeResultDescriptor {
      action_key: CANONICAL_ACTION_KEY.to_string(),
      witness: NativeCompilerWitness {
        version: 1,
        complete: true,
        source_paths: vec!["lib.rs".to_string()],
        dependency_names: Vec::new(),
        environment_names: Vec::new(),
      },
      outputs: vec![
        output(
          "dep_info",
          DEP_INFO_SLOT,
          "sha256:d708da5a10078e1c0ec92d1f93c4592e152f1639a0cdf3f545e02fd7e6325883".to_string(),
          8,
          0o644,
        ),
        output(
          "metadata",
          METADATA_SLOT,
          "sha256:45447b7afbd5e544f7d0f1df0fccd26014d9850130abd3f020b89ff96b82079f".to_string(),
          8,
          0o644,
        ),
      ],
      stdout_digest: "sha256:50c560dce53c1b4f84780f00d3177742272b2d6cef2e80347fe8227320ff7c7c".to_string(),
      stdout_bytes: 15,
      stderr_digest: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
      stderr_bytes: 0,
    }
  }

  fn canonical_slot_payloads() -> [(&'static str, &'static [u8]); 4] {
    [
      (DEP_INFO_SLOT, b"dep-info"),
      (METADATA_SLOT, b"metadata"),
      (STDOUT_SLOT, b"portable stdout"),
      (STDERR_SLOT, b""),
    ]
  }

  fn write_slot_payloads(root: &Path, payloads: &[(&str, &[u8])]) {
    for (slot, payload) in payloads {
      let path = root.join(slot);
      fs::create_dir_all(path.parent().expect("slot parent")).expect("slot directory");
      fs::write(path, payload).expect("slot payload");
    }
  }

  fn assert_association_rejected(name: &str, bytes: &[u8]) {
    assert!(
      decode_association(bytes, CANONICAL_ACTION_KEY, CANONICAL_RESULT_KEY).is_err(),
      "descriptor mutation '{name}' retained association authority"
    );
  }

  fn assert_pack_rejected(
    name: &str,
    bytes: &[u8],
    expected_action: &str,
    expected_result: &str,
    declared_length: Option<u64>,
  ) {
    let directory = tempfile::tempdir().expect("rejected pack staging");
    let staging_path = directory.path().to_path_buf();
    let staging = NativeResultStaging::command_scoped(directory);
    let error = match decode(
      Cursor::new(bytes),
      expected_action,
      expected_result,
      declared_length,
      Some(staging),
    ) {
      Ok(_) => panic!("pack mutation '{name}' retained staging authority"),
      Err(error) => error,
    };
    assert!(!error.to_string().is_empty(), "mutation '{name}' lost its diagnostic");
    assert!(
      !staging_path.exists(),
      "pack mutation '{name}' left private staging behind"
    );
  }

  #[test]
  fn independently_assembled_canonical_descriptor_and_pack_match_the_v8_v3_contract() {
    let expected_descriptor = literal_bytes(CANONICAL_DESCRIPTOR_HEX);
    let descriptor = canonical_descriptor();
    assert_eq!(descriptor.encode().expect("encode descriptor"), expected_descriptor);
    assert_eq!(
      NativeResultDescriptor::decode(&expected_descriptor).expect("decode descriptor"),
      descriptor
    );
    assert_eq!(descriptor.result_key().expect("result key"), CANONICAL_RESULT_KEY);
    assert_eq!(
      hex_digest(&Sha256::digest(&expected_descriptor)),
      CANONICAL_DESCRIPTOR_SHA256
    );

    let validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(b"hello");
    assert_ne!(
      validation.result_key(),
      CANONICAL_RESULT_KEY,
      "stream bytes must enter identity"
    );
    let validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(b"portable stdout");
    assert_eq!(validation.action_key(), CANONICAL_ACTION_KEY);
    assert_eq!(validation.result_key(), CANONICAL_RESULT_KEY);
    let association = association(&validation).expect("canonical association");
    assert_eq!(association.bytes(), expected_descriptor);
    assert_eq!(association.pack_length(), PACK_END as u64);

    let root = tempfile::tempdir().expect("export slot root");
    write_slot_payloads(root.path(), &canonical_slot_payloads());
    let mut opens = BTreeMap::<&str, usize>::new();
    let mut positions = BTreeMap::<&str, (File, u64)>::new();
    let mut encoded_pack = Vec::new();
    let export = export(&validation, &mut encoded_pack, |slot| {
      *opens.entry(slot.path).or_default() += 1;
      let file = File::open(root.path().join(slot.path))?;
      positions.insert(slot.path, (file.try_clone()?, slot.bytes));
      Ok(file)
    })
    .expect("canonical pack export");
    let expected_pack = canonical_pack_bytes();
    assert_eq!(encoded_pack, expected_pack);
    assert_eq!(export.content_length, PACK_END as u64);
    assert_eq!(export.bytes_written, PACK_END as u64);
    assert_eq!(hex_digest(&Sha256::digest(&encoded_pack)), CANONICAL_PACK_SHA256);
    assert_eq!(
      opens,
      BTreeMap::from([
        (DEP_INFO_SLOT, 1),
        (METADATA_SLOT, 1),
        (STDOUT_SLOT, 1),
        (STDERR_SLOT, 1),
      ]),
      "streaming export must open and read each immutable slot exactly once"
    );
    for (slot, (mut file, expected)) in positions {
      assert_eq!(
        file.stream_position().expect("export source position"),
        expected,
        "export did not consume slot '{slot}' exactly once"
      );
    }

    let decoded = decode(
      Cursor::new(&encoded_pack),
      CANONICAL_ACTION_KEY,
      CANONICAL_RESULT_KEY,
      Some(PACK_END as u64),
      None,
    )
    .expect("canonical pack decode");
    assert_eq!(decoded.bytes_read, PACK_END as u64);
    assert_eq!(decoded.descriptor, descriptor);
    let staging_path = decoded.staging.path().to_path_buf();
    for (slot, payload) in canonical_slot_payloads() {
      let path = staging_path.join(slot);
      assert_eq!(fs::read(&path).expect("decoded slot"), payload);
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
          fs::metadata(&path).expect("decoded mode").permissions().mode() & 0o777,
          0o644
        );
      }
      #[cfg(not(unix))]
      assert!(!fs::metadata(&path).expect("decoded mode").permissions().readonly());
    }
    drop(decoded);
    assert!(!staging_path.exists(), "private decoded staging must follow its owner");
  }

  #[test]
  fn descriptor_rejects_every_field_mutation_and_noncanonical_shape() {
    let canonical = literal_bytes(CANONICAL_DESCRIPTOR_HEX);
    assert_eq!(
      &canonical[ACTION_OFFSET..ACTION_OFFSET + ACTION_BYTES],
      CANONICAL_ACTION_KEY.as_bytes()
    );
    assert_eq!(canonical[SLOT_COUNT_OFFSET], 4);

    let mut mutations = Vec::<(String, Vec<u8>)>::new();
    for (name, offset) in [
      ("magic", 0),
      ("descriptor_version", 8),
      ("identity_contract", 10),
      ("result_class", 12),
      ("reserved", 14),
      ("action_length", 16),
      ("action", ACTION_OFFSET),
      ("witness_version", WITNESS_VERSION_OFFSET),
      ("witness_flag", WITNESS_FLAG_OFFSET),
      ("source_count", SOURCE_COUNT_OFFSET),
      ("source_length", SOURCE_LENGTH_OFFSET),
      ("source_path", SOURCE_OFFSET),
      ("dependency_count", DEPENDENCY_COUNT_OFFSET),
      ("environment_count", ENVIRONMENT_COUNT_OFFSET),
      ("slot_count", SLOT_COUNT_OFFSET),
    ] {
      let mut bytes = canonical.clone();
      bytes[offset] ^= if bytes[offset] == 0 { 1 } else { 0xff };
      mutations.push((name.to_string(), bytes));
    }
    for (index, slot) in DESCRIPTOR_SLOT_OFFSETS.into_iter().enumerate() {
      for (field, offset) in [
        ("kind", slot),
        ("mode", slot + 1),
        ("length", slot + 3),
        ("digest", slot + 11),
      ] {
        let mut bytes = canonical.clone();
        bytes[offset] ^= 0xff;
        mutations.push((format!("slot_{index}_{field}"), bytes));
      }
    }

    let mut reordered = canonical.clone();
    let first = reordered[DESCRIPTOR_SLOT_OFFSETS[0]..DESCRIPTOR_SLOT_OFFSETS[1]].to_vec();
    let second = reordered[DESCRIPTOR_SLOT_OFFSETS[1]..DESCRIPTOR_SLOT_OFFSETS[2]].to_vec();
    reordered[DESCRIPTOR_SLOT_OFFSETS[0]..DESCRIPTOR_SLOT_OFFSETS[1]].copy_from_slice(&second);
    reordered[DESCRIPTOR_SLOT_OFFSETS[1]..DESCRIPTOR_SLOT_OFFSETS[2]].copy_from_slice(&first);
    mutations.push(("slot_reorder".to_string(), reordered));

    let mut omitted = canonical.clone();
    omitted.drain(DESCRIPTOR_SLOT_OFFSETS[1]..DESCRIPTOR_SLOT_OFFSETS[2]);
    omitted[SLOT_COUNT_OFFSET] = 3;
    mutations.push(("slot_omission".to_string(), omitted));

    let mut duplicated = canonical.clone();
    let first = duplicated[DESCRIPTOR_SLOT_OFFSETS[0]..DESCRIPTOR_SLOT_OFFSETS[1]].to_vec();
    duplicated[DESCRIPTOR_SLOT_OFFSETS[1]..DESCRIPTOR_SLOT_OFFSETS[2]].copy_from_slice(&first);
    mutations.push(("slot_duplicate".to_string(), duplicated));

    let mut trailing = canonical.clone();
    trailing.push(0);
    mutations.push(("trailing_byte".to_string(), trailing));

    let mut same_size = canonical.clone();
    same_size[SOURCE_OFFSET] = b'b';
    assert!(NativeResultDescriptor::decode(&same_size).is_ok());
    mutations.push(("same_size_semantic_corruption".to_string(), same_size));

    let mut forged_action = canonical.clone();
    let forged = format!("{}{}", crate::compiler::native_cache::ACTION_KEY_PREFIX, "0".repeat(64));
    forged_action[ACTION_OFFSET..ACTION_OFFSET + ACTION_BYTES].copy_from_slice(forged.as_bytes());
    assert!(NativeResultDescriptor::decode(&forged_action).is_ok());
    mutations.push(("forged_action".to_string(), forged_action));

    for (name, bytes) in mutations {
      assert_association_rejected(&name, &bytes);
    }
    for length in 0..canonical.len() {
      assert_association_rejected(&format!("truncated_at_{length}"), &canonical[..length]);
    }
    let forged_action = format!("{}{}", crate::compiler::native_cache::ACTION_KEY_PREFIX, "0".repeat(64));
    let forged_result = format!("{}{}", crate::compiler::native_cache::RESULT_KEY_PREFIX, "0".repeat(64));
    assert!(decode_association(&canonical, &forged_action, CANONICAL_RESULT_KEY).is_err());
    assert!(decode_association(&canonical, CANONICAL_ACTION_KEY, &forged_result).is_err());
  }

  #[test]
  fn pack_rejects_every_frame_mutation_without_leaving_staging() {
    let canonical = canonical_pack_bytes();
    assert_eq!(canonical.len(), PACK_END);
    let mut mutations = Vec::<(String, Vec<u8>)>::new();
    for (name, offset) in [("magic", 0), ("version", 8), ("descriptor_length", 10)] {
      let mut bytes = canonical.clone();
      bytes[offset] ^= 0xff;
      mutations.push((name.to_string(), bytes));
    }
    for (index, frame) in PACK_FRAME_OFFSETS.into_iter().enumerate() {
      for (field, offset) in [("kind", frame), ("length", frame + 1)] {
        let mut bytes = canonical.clone();
        bytes[offset] ^= 0xff;
        mutations.push((format!("frame_{index}_{field}"), bytes));
      }
    }
    for (name, offset) in [
      ("dep_info_payload", 327),
      ("metadata_payload", 344),
      ("stdout_payload", 361),
    ] {
      let mut bytes = canonical.clone();
      bytes[offset] ^= 0xff;
      mutations.push((name.to_string(), bytes));
    }
    let mut descriptor_digest = canonical.clone();
    descriptor_digest[PACK_DESCRIPTOR_OFFSET + DESCRIPTOR_SLOT_OFFSETS[3] + 11] ^= 0xff;
    mutations.push(("zero_length_stderr_digest".to_string(), descriptor_digest));

    let mut reordered = canonical[..PACK_FRAME_OFFSETS[0]].to_vec();
    reordered.extend_from_slice(&canonical[PACK_FRAME_OFFSETS[1]..PACK_FRAME_OFFSETS[2]]);
    reordered.extend_from_slice(&canonical[PACK_FRAME_OFFSETS[0]..PACK_FRAME_OFFSETS[1]]);
    reordered.extend_from_slice(&canonical[PACK_FRAME_OFFSETS[2]..]);
    mutations.push(("frame_reorder".to_string(), reordered));

    let mut omitted = canonical.clone();
    omitted.drain(PACK_FRAME_OFFSETS[1]..PACK_FRAME_OFFSETS[2]);
    mutations.push(("frame_omission".to_string(), omitted));

    let mut duplicated = canonical.clone();
    let first = duplicated[PACK_FRAME_OFFSETS[0]..PACK_FRAME_OFFSETS[1]].to_vec();
    duplicated[PACK_FRAME_OFFSETS[1]..PACK_FRAME_OFFSETS[2]].copy_from_slice(&first);
    mutations.push(("frame_duplicate".to_string(), duplicated));

    let mut trailing = canonical.clone();
    trailing.push(0);
    mutations.push(("trailing_byte".to_string(), trailing));

    for (name, bytes) in mutations {
      assert_pack_rejected(&name, &bytes, CANONICAL_ACTION_KEY, CANONICAL_RESULT_KEY, None);
    }
    for length in 0..canonical.len() {
      assert_pack_rejected(
        &format!("truncated_at_{length}"),
        &canonical[..length],
        CANONICAL_ACTION_KEY,
        CANONICAL_RESULT_KEY,
        None,
      );
    }
    assert_pack_rejected(
      "declared_length",
      &canonical,
      CANONICAL_ACTION_KEY,
      CANONICAL_RESULT_KEY,
      Some(PACK_END as u64 - 1),
    );
    let forged_action = format!("{}{}", crate::compiler::native_cache::ACTION_KEY_PREFIX, "0".repeat(64));
    let forged_result = format!("{}{}", crate::compiler::native_cache::RESULT_KEY_PREFIX, "0".repeat(64));
    assert_pack_rejected(
      "forged_expected_action",
      &canonical,
      &forged_action,
      CANONICAL_RESULT_KEY,
      None,
    );
    assert_pack_rejected(
      "forged_expected_result",
      &canonical,
      CANONICAL_ACTION_KEY,
      &forged_result,
      None,
    );
  }

  #[test]
  fn output_mode_is_exact_result_identity() {
    let descriptor = canonical_descriptor();
    let encoded = descriptor.encode().expect("canonical descriptor");
    let mut restrictive = descriptor.clone();
    restrictive.outputs[0].mode = 0o600;
    let restrictive_bytes = restrictive.encode().expect("restrictive mode descriptor");
    assert_eq!(
      NativeResultDescriptor::decode(&restrictive_bytes).expect("restrictive mode decode"),
      restrictive
    );
    assert_eq!(
      NativeResultDescriptor::decode(&encoded)
        .expect("canonical descriptor")
        .result_key()
        .expect("canonical result key"),
      CANONICAL_RESULT_KEY
    );
    assert_ne!(
      NativeResultDescriptor::decode(&restrictive_bytes)
        .expect("restrictive descriptor")
        .result_key()
        .expect("restrictive result key"),
      CANONICAL_RESULT_KEY,
      "output mode must enter result identity"
    );

    let mut executable = restrictive_bytes;
    executable[DESCRIPTOR_SLOT_OFFSETS[0] + 1..DESCRIPTOR_SLOT_OFFSETS[0] + 3]
      .copy_from_slice(&0o755_u16.to_le_bytes());
    assert!(NativeResultDescriptor::decode(&executable).is_err());
  }

  #[test]
  fn streaming_pack_io_stays_within_wire_overhead_without_export_rereads_or_import_overstaging() {
    const SLOT_BYTES: usize = 256 * 1024;
    let dep_info = vec![b'd'; SLOT_BYTES];
    let metadata = vec![b'm'; SLOT_BYTES];
    let stdout = vec![b'o'; SLOT_BYTES];
    let stderr = vec![b'e'; SLOT_BYTES];
    let payloads = [
      (DEP_INFO_SLOT, dep_info.as_slice()),
      (METADATA_SLOT, metadata.as_slice()),
      (STDOUT_SLOT, stdout.as_slice()),
      (STDERR_SLOT, stderr.as_slice()),
    ];
    let logical_payload = payloads.iter().map(|(_, payload)| payload.len() as u64).sum::<u64>();

    let mut validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(b"portable stdout");
    for ((output, observed), (_, payload)) in validation
      .outputs
      .iter_mut()
      .zip(validation.observation.emitted_outputs.iter_mut())
      .zip(payloads.iter())
    {
      output.content_digest = digest(payload);
      output.bytes = payload.len() as u64;
      observed.content_digest = output.content_digest.clone();
    }
    validation.stdout_digest = digest(&stdout);
    validation.stdout_bytes = stdout.len() as u64;
    validation.stderr_digest = digest(&stderr);
    validation.stderr_bytes = stderr.len() as u64;
    validation.result_key = NativeResultDescriptor::from_identity(NativeResultIdentity {
      action_key: validation.action_key(),
      witness: &validation.witness,
      outputs: &validation.outputs,
      stdout_digest: &validation.stdout_digest,
      stdout_bytes: validation.stdout_bytes,
      stderr_digest: &validation.stderr_digest,
      stderr_bytes: validation.stderr_bytes,
    })
    .expect("large descriptor")
    .result_key()
    .expect("large result key");
    validation.validate_object().expect("large validation");

    let root = tempfile::tempdir().expect("slot root");
    write_slot_payloads(root.path(), &payloads);
    let mut opens = BTreeMap::<&str, usize>::new();
    let mut positions = BTreeMap::<&str, (File, u64)>::new();
    let mut wire = Vec::new();
    let exported = export(&validation, &mut wire, |slot| {
      *opens.entry(slot.path).or_default() += 1;
      let file = File::open(root.path().join(slot.path))?;
      positions.insert(slot.path, (file.try_clone()?, slot.bytes));
      Ok(file)
    })
    .expect("streaming export");
    assert_eq!(exported.bytes_written, wire.len() as u64);
    assert_eq!(exported.content_length, wire.len() as u64);
    assert!(
      exported.bytes_written * 10 <= logical_payload * 11,
      "wire bytes {} exceed 1.1x logical payload {logical_payload}",
      exported.bytes_written
    );
    assert!(
      opens.values().all(|count| *count == 1),
      "export reopened a payload slot"
    );
    assert_eq!(opens.len(), payloads.len());
    for (slot, (mut file, expected)) in positions {
      assert_eq!(
        file.stream_position().expect("export source position"),
        expected,
        "export did not consume slot '{slot}' exactly once"
      );
    }

    let directory = tempfile::tempdir().expect("import staging");
    let staging_path = directory.path().to_path_buf();
    let (decoded, bytes_staged) = decode_counted(
      Cursor::new(&wire),
      validation.action_key(),
      validation.result_key(),
      Some(wire.len() as u64),
      Some(NativeResultStaging::command_scoped(directory)),
    )
    .expect("streaming import");
    assert_eq!(decoded.bytes_read, wire.len() as u64);
    assert_eq!(bytes_staged, logical_payload);
    assert!(
      decoded.bytes_read * 10 <= logical_payload * 11,
      "read bytes {} exceed 1.1x logical payload {logical_payload}",
      decoded.bytes_read
    );
    assert!(
      bytes_staged * 10 <= logical_payload * 11,
      "staged writes {bytes_staged} exceed 1.1x logical payload {logical_payload}"
    );
    let staged_bytes = payloads
      .iter()
      .map(|(slot, _)| {
        fs::metadata(staging_path.join(slot))
          .expect("staged slot metadata")
          .len()
      })
      .sum::<u64>();
    assert_eq!(staged_bytes, logical_payload, "import retained duplicate payload bytes");
    for (slot, payload) in payloads {
      assert_eq!(fs::read(staging_path.join(slot)).expect("staged slot"), payload);
    }
  }
}

#[cfg(test)]
mod process_evidence;
