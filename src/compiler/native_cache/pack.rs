//! Canonical native-result descriptors and private local-CAS staging.

use std::collections::BTreeMap;
use std::convert::TryFrom as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{
  CDYLIB_SLOT, DEP_INFO_SLOT, DYLIB_SLOT, EXECUTABLE_SLOT, MAX_SOURCE_ENTRIES, METADATA_SLOT, NativeCompilerOutput,
  NativeCompilerWitness, NativeResultIdentity, PROC_MACRO_SLOT, RESULT_KEY_PREFIX, RLIB_SLOT, STATICLIB_SLOT,
  STDERR_SLOT, STDOUT_SLOT, sha256_identity, validate_action_key, validate_result_key, validate_sha256,
};
use crate::error::{RailError, RailResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const DESCRIPTOR_MAGIC: &[u8; 8] = b"CRNDESC1";
const DESCRIPTOR_VERSION: u16 = 7;
const IDENTITY_CONTRACT_VERSION: u16 = 12;
const RESULT_CLASS_VERSION: u16 = 5;
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_STRING_BYTES: usize = 4 * 1024;
const FIXED_DESCRIPTOR_PREFIX_BYTES: usize = 8 + 2 + 2 + 2 + 2;
const MAX_RESULT_PAYLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 16 * 1024 * 1024;
const PACK_MAGIC: &[u8; 8] = b"CRNPACK2";
const PACK_VERSION: u16 = 1;
const PACK_PRELUDE_BYTES: u64 = 8 + 2 + 4;
const IO_BUFFER_BYTES: usize = 64 * 1024;

/// Absolute bound for one canonical remote result pack.
pub(crate) const MAX_PACK_BYTES: u64 = PACK_PRELUDE_BYTES + MAX_DESCRIPTOR_BYTES as u64 + MAX_RESULT_PAYLOAD_BYTES;

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
  verified_generations: BTreeMap<PathBuf, Vec<u8>>,
  durable_generations: BTreeMap<PathBuf, Vec<u8>>,
}

impl NativeResultStaging {
  pub(crate) fn guarded(directory: tempfile::TempDir, active: File) -> Self {
    Self {
      directory,
      active: Some(active),
      verified_generations: BTreeMap::new(),
      durable_generations: BTreeMap::new(),
    }
  }

  pub(crate) fn path(&self) -> &Path {
    self.directory.path()
  }

  pub(crate) fn requires_durable_handoff(&self) -> bool {
    self.active.is_some()
  }

  pub(super) fn into_parts(self) -> (tempfile::TempDir, Option<File>, BTreeMap<PathBuf, Vec<u8>>) {
    (self.directory, self.active, self.durable_generations)
  }

  pub(crate) fn temporary() -> RailResult<Self> {
    Ok(Self {
      directory: tempfile::Builder::new().prefix("native-pack-").tempdir()?,
      active: None,
      verified_generations: BTreeMap::new(),
      durable_generations: BTreeMap::new(),
    })
  }

  pub(crate) fn temporary_in(parent: &Path) -> RailResult<Self> {
    Ok(Self {
      directory: tempfile::Builder::new().prefix("native-pack-").tempdir_in(parent)?,
      active: None,
      verified_generations: BTreeMap::new(),
      durable_generations: BTreeMap::new(),
    })
  }

  pub(super) fn into_output_handoff(self) -> NativePackHandoff {
    NativePackHandoff {
      directory: self.directory,
      verified_generations: self.verified_generations,
    }
  }
}

/// The semantic descriptor used by local result identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
      &[(b"version", &8_u32.to_le_bytes()), (b"descriptor", &descriptor)],
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
    push_bytes(&mut bytes, &serde_json::to_vec(&self.witness.linker)?)?;
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
    let linker = serde_json::to_vec(&self.witness.linker)?;
    FIXED_DESCRIPTOR_PREFIX_BYTES
      .checked_add(strings)
      .and_then(|total| total.checked_add(2 + 1 + 4 * 4 + 1))
      .and_then(|total| total.checked_add(4 + linker.len()))
      .and_then(|total| total.checked_add((self.outputs.len() + 2) * (1 + 2 + 8 + 32)))
      .ok_or_else(|| RailError::message("native result descriptor size overflow"))
  }

  fn validate(&self) -> RailResult<()> {
    validate_action_key(&self.action_key)?;
    if self.witness.version != 5
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
        .linker
        .as_ref()
        .is_some_and(|witness| super::validate_linker_witness(witness).is_err())
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
        output.bytes == 0 && output.role != "metadata"
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

/// One fixed payload slot derived from the canonical descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePackSlot<'a> {
  pub(crate) path: &'a str,
  pub(crate) bytes: u64,
  pub(crate) digest: &'a str,
  pub(crate) mode: u32,
}

/// Exact pack length and bytes emitted by one streaming export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePackExport {
  pub(crate) content_length: u64,
  pub(crate) bytes_written: u64,
}

/// Canonical action/result binding derived from one pack descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeAssociation {
  action_key: String,
  result_key: String,
  pack_length: u64,
}

impl NativeAssociation {
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

/// Verified descriptor and private payload staging produced before L1 admission.
pub(crate) struct DecodedNativePack {
  pub(super) staging: NativeResultStaging,
  pub(super) descriptor: NativeResultDescriptor,
  pub(crate) bytes_read: u64,
}

/// One authenticated decode staged privately on the output filesystem.
pub(crate) struct NativePackHandoff {
  directory: tempfile::TempDir,
  verified_generations: BTreeMap<PathBuf, Vec<u8>>,
}

impl NativePackHandoff {
  pub(crate) fn materialize(
    self,
    validation: &super::NativeCompilerValidation,
    destination: &Path,
  ) -> RailResult<Option<u64>> {
    let descriptor = descriptor_from_validation(validation)?;
    let slots = descriptor_slots(&descriptor);
    if self.verified_generations.len() != slots.len() {
      return Ok(None);
    }
    let mut bytes = 0_u64;
    for slot in slots {
      let path = self.directory.path().join(slot.path);
      let Some(expected_generation) = self.verified_generations.get(&path) else {
        return Ok(None);
      };
      let metadata = fs::symlink_metadata(&path)?;
      let opened = File::open(&path)?;
      if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() != slot.bytes
        || super::native_output_mode(&metadata) != slot.mode
        || !crate::utils::private_file_matches_path(&opened, &path, slot.bytes)?
        || crate::utils::stable_file_generation(&path).as_ref() != Some(expected_generation)
      {
        return Ok(None);
      }
      bytes = bytes.saturating_add(slot.bytes);
    }
    fs::create_dir(destination)?;
    let source = self.directory.path().join("target");
    let target = destination.join("target");
    if let Err(error) = fs::rename(&source, &target) {
      let cleanup = fs::remove_dir(destination);
      return Err(match cleanup {
        Ok(()) => error.into(),
        Err(cleanup) => RailError::message(format!(
          "authenticated native pack handoff failed: {error}; cleanup failed: {cleanup}"
        )),
      });
    }
    Ok(Some(bytes))
  }
}

fn descriptor_from_validation(validation: &super::NativeCompilerValidation) -> RailResult<NativeResultDescriptor> {
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
      "native result descriptor identity changed before pack export",
    ));
  }
  Ok(descriptor)
}

fn descriptor_bytes(descriptor: &NativeResultDescriptor) -> RailResult<Vec<u8>> {
  descriptor.validate()?;
  let bytes = serde_json::to_vec(descriptor)?;
  if bytes.len() > MAX_DESCRIPTOR_BYTES {
    return Err(RailError::message(
      "native result pack descriptor exceeds its byte bound",
    ));
  }
  Ok(bytes)
}

fn descriptor_slots(descriptor: &NativeResultDescriptor) -> Vec<NativePackSlot<'_>> {
  descriptor
    .outputs
    .iter()
    .map(|output| NativePackSlot {
      path: output.slot.as_str(),
      bytes: output.bytes,
      digest: output.content_digest.as_str(),
      mode: output.mode,
    })
    .chain([
      NativePackSlot {
        path: STDOUT_SLOT,
        bytes: descriptor.stdout_bytes,
        digest: descriptor.stdout_digest.as_str(),
        mode: STREAM_SLOT_MODE,
      },
      NativePackSlot {
        path: STDERR_SLOT,
        bytes: descriptor.stderr_bytes,
        digest: descriptor.stderr_digest.as_str(),
        mode: STREAM_SLOT_MODE,
      },
    ])
    .collect()
}

fn pack_length(descriptor: &NativeResultDescriptor, encoded_descriptor_bytes: usize) -> RailResult<u64> {
  let descriptor_bytes = u64::try_from(encoded_descriptor_bytes)
    .map_err(|_| RailError::message("native result pack descriptor size is out of range"))?;
  let payload = descriptor_slots(descriptor)
    .into_iter()
    .try_fold(0_u64, |total, slot| {
      total
        .checked_add(slot.bytes)
        .ok_or_else(|| RailError::message("native result pack payload size overflow"))
    })?;
  if payload > MAX_RESULT_PAYLOAD_BYTES {
    return Err(RailError::message("native result pack exceeds its payload bound"));
  }
  PACK_PRELUDE_BYTES
    .checked_add(descriptor_bytes)
    .and_then(|total| total.checked_add(payload))
    .filter(|total| *total <= MAX_PACK_BYTES)
    .ok_or_else(|| RailError::message("native result pack size overflow"))
}

pub(crate) fn association(validation: &super::NativeCompilerValidation) -> RailResult<NativeAssociation> {
  let descriptor = descriptor_from_validation(validation)?;
  let encoded = descriptor_bytes(&descriptor)?;
  Ok(NativeAssociation {
    action_key: descriptor.action_key.clone(),
    result_key: descriptor.result_key()?,
    pack_length: pack_length(&descriptor, encoded.len())?,
  })
}

/// Stream one canonical pack from immutable caller-opened slot files.
pub(crate) fn export<W, F>(
  validation: &super::NativeCompilerValidation,
  mut writer: W,
  mut open_slot: F,
) -> RailResult<NativePackExport>
where
  W: Write,
  F: for<'a> FnMut(NativePackSlot<'a>) -> RailResult<File>,
{
  let descriptor = descriptor_from_validation(validation)?;
  let encoded = descriptor_bytes(&descriptor)?;
  let content_length = pack_length(&descriptor, encoded.len())?;
  writer.write_all(PACK_MAGIC)?;
  writer.write_all(&PACK_VERSION.to_le_bytes())?;
  let header_length = u32::try_from(encoded.len())
    .map_err(|_| RailError::message("native result pack descriptor length is out of range"))?;
  writer.write_all(&header_length.to_le_bytes())?;
  writer.write_all(&encoded)?;
  let mut bytes_written = PACK_PRELUDE_BYTES.saturating_add(u64::from(header_length));
  for slot in descriptor_slots(&descriptor) {
    let mut reader = open_slot(slot)?;
    bytes_written = bytes_written.saturating_add(copy_exact_digest(&mut reader, &mut writer, slot.bytes, slot.digest)?);
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

/// Decode a complete pack bound to one live action without granting L1 authority.
pub(crate) fn decode_for_action<R: Read>(
  mut reader: R,
  expected_action: &str,
  declared_length: Option<u64>,
  staging: Option<NativeResultStaging>,
) -> RailResult<(DecodedNativePack, NativeAssociation)> {
  validate_action_key(expected_action)?;
  let mut prelude = [0_u8; PACK_PRELUDE_BYTES as usize];
  reader
    .read_exact(&mut prelude)
    .map_err(|_| RailError::message("native result pack prelude is truncated"))?;
  if &prelude[..8] != PACK_MAGIC || u16::from_le_bytes([prelude[8], prelude[9]]) != PACK_VERSION {
    return Err(RailError::message("native result pack has an incompatible prelude"));
  }
  let header_length = u32::from_le_bytes(
    prelude[10..14]
      .try_into()
      .map_err(|_| RailError::message("native result pack descriptor length is malformed"))?,
  ) as usize;
  if header_length > MAX_DESCRIPTOR_BYTES {
    return Err(RailError::message(
      "native result pack descriptor exceeds its byte bound",
    ));
  }
  let mut encoded = vec![0_u8; header_length];
  reader
    .read_exact(&mut encoded)
    .map_err(|_| RailError::message("native result pack descriptor is truncated"))?;
  let descriptor = serde_json::from_slice::<NativeResultDescriptor>(&encoded)
    .map_err(|_| RailError::message("native result pack descriptor is malformed"))?;
  descriptor.validate()?;
  if descriptor_bytes(&descriptor)? != encoded {
    return Err(RailError::message(
      "native result pack descriptor is not canonically encoded",
    ));
  }
  if descriptor.action_key != expected_action {
    return Err(RailError::message(
      "native result pack descriptor does not match the requested action",
    ));
  }
  let result_key = descriptor.result_key()?;
  validate_result_key(&result_key)?;
  let exact_length = pack_length(&descriptor, encoded.len())?;
  if declared_length.is_some_and(|declared| declared != exact_length) {
    return Err(RailError::message(
      "native result pack declared length does not match its descriptor",
    ));
  }
  let mut staging = staging.map_or_else(NativeResultStaging::temporary, Ok)?;
  let durable_handoff = staging.requires_durable_handoff();
  let mut bytes_read = PACK_PRELUDE_BYTES.saturating_add(encoded.len() as u64);
  for slot in descriptor_slots(&descriptor) {
    let path = staging.path().join(slot.path);
    let parent = path
      .parent()
      .ok_or_else(|| RailError::message("native result pack slot has no staging parent"))?;
    create_private_slot_parent(staging.path(), parent)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(&path)?;
    bytes_read = bytes_read.saturating_add(copy_exact_digest(&mut reader, &mut output, slot.bytes, slot.digest)?);
    set_private_output_mode(&path, slot.mode)?;
    if durable_handoff {
      let _durability = super::native_durability_phase(super::NativeDurabilityPhase::L1FileSync);
      output.sync_all()?;
      if let Some(generation) = crate::utils::stable_file_generation(&path) {
        staging.verified_generations.insert(path.clone(), generation.clone());
        staging.durable_generations.insert(path, generation);
      }
    } else if let Some(generation) = crate::utils::stable_file_generation(&path) {
      staging.verified_generations.insert(path, generation);
    }
  }
  let mut trailing = [0_u8; 1];
  if reader.read(&mut trailing)? != 0 || bytes_read != exact_length {
    return Err(RailError::message(
      "native result pack contains trailing bytes or has an inconsistent length",
    ));
  }
  let association = NativeAssociation {
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
  ))
}

/// Decode one exact zstd frame while retaining its compressed authority separately.
pub(crate) fn decode_zstd_for_action<R: Read>(
  source: R,
  compressed_bytes: u64,
  expected_action: &str,
  pack_bytes: u64,
  staging: NativeResultStaging,
) -> RailResult<(DecodedNativePack, NativeAssociation)> {
  let mut decoder = zstd::stream::read::Decoder::new(source.take(compressed_bytes))
    .map_err(|_| RailError::message("compressed native pack is malformed"))?;
  let decoded = decode_for_action(&mut decoder, expected_action, Some(pack_bytes), Some(staging))?;
  let source = decoder.finish();
  if !source.buffer().is_empty() || source.get_ref().limit() != 0 {
    return Err(RailError::message(
      "compressed native pack has an inconsistent encoded length",
    ));
  }
  Ok(decoded)
}

fn create_private_slot_parent(root: &Path, parent: &Path) -> RailResult<()> {
  if !parent.starts_with(root) {
    return Err(RailError::message("native result pack slot escaped its staging root"));
  }
  fs::create_dir_all(parent)?;
  let mut current = parent;
  while current != root {
    set_private_output_mode(current, 0o755)?;
    current = current
      .parent()
      .ok_or_else(|| RailError::message("native result pack slot escaped its staging root"))?;
  }
  Ok(())
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
    let remaining = expected_bytes.saturating_sub(copied);
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
  let actual = format!(
    "sha256:{}",
    crate::source::ContentDigest::from_sha256_bytes(hasher.finalize().into())
  );
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

#[cfg(test)]
mod tests {
  use std::fs;
  use std::io::Cursor;

  use super::*;

  fn exported_fixture(stdout: &[u8]) -> (super::super::NativeCompilerValidation, Vec<u8>) {
    let validation = super::super::tests::cas_validation_with_stdout(stdout);
    let bytes = export_fixture(&validation, stdout, b"metadata");
    (validation, bytes)
  }

  fn export_fixture(validation: &super::super::NativeCompilerValidation, stdout: &[u8], metadata: &[u8]) -> Vec<u8> {
    let source = tempfile::tempdir().expect("pack source");
    for (path, bytes) in [
      (DEP_INFO_SLOT, b"dep-info".as_slice()),
      (METADATA_SLOT, metadata),
      (STDOUT_SLOT, stdout),
      (STDERR_SLOT, b"".as_slice()),
    ] {
      let path = source.path().join(path);
      fs::create_dir_all(path.parent().expect("slot parent")).expect("slot parent");
      fs::write(path, bytes).expect("slot payload");
    }

    let mut bytes = Vec::new();
    let exported = export(validation, &mut bytes, |slot| {
      Ok(File::open(source.path().join(slot.path))?)
    })
    .expect("canonical export");
    assert_eq!(exported.content_length, bytes.len() as u64);
    assert_eq!(exported.bytes_written, bytes.len() as u64);
    bytes
  }

  #[test]
  fn canonical_pack_round_trips_exact_payloads_and_identity() {
    let stdout = b"compiler output\n";
    let (validation, bytes) = exported_fixture(stdout);
    let (decoded, association) = decode_for_action(
      Cursor::new(&bytes),
      validation.action_key(),
      Some(bytes.len() as u64),
      None,
    )
    .expect("verified decode");

    assert_eq!(association.action_key(), validation.action_key());
    assert_eq!(association.result_key(), validation.result_key());
    assert_eq!(association.pack_length(), bytes.len() as u64);
    assert_eq!(decoded.bytes_read, bytes.len() as u64);
    assert_eq!(
      fs::read(decoded.staging.path().join(DEP_INFO_SLOT)).expect("decoded dep-info"),
      b"dep-info"
    );
    assert_eq!(
      fs::read(decoded.staging.path().join(METADATA_SLOT)).expect("decoded metadata"),
      b"metadata"
    );
    assert_eq!(
      fs::read(decoded.staging.path().join(STDOUT_SLOT)).expect("decoded stdout"),
      stdout
    );
    assert_eq!(
      fs::read(decoded.staging.path().join(STDERR_SLOT)).expect("decoded stderr"),
      b""
    );
  }

  #[test]
  fn canonical_pack_round_trips_zero_byte_metadata() {
    let mut validation = super::super::tests::cas_validation_with_stdout(b"");
    let empty_digest = super::super::digest(b"");
    validation.outputs[1].bytes = 0;
    validation.outputs[1].content_digest = empty_digest.clone();
    validation.observation.emitted_outputs[1].content_digest = empty_digest;
    validation.result_key = super::super::result_key(
      &validation.action_key,
      &validation.witness,
      &validation.outputs,
      &validation.stdout_digest,
      validation.stdout_bytes,
      &validation.stderr_digest,
      validation.stderr_bytes,
    )
    .expect("zero-byte metadata result identity");

    let bytes = export_fixture(&validation, b"", b"");
    let (decoded, association) = decode_for_action(
      Cursor::new(&bytes),
      validation.action_key(),
      Some(bytes.len() as u64),
      None,
    )
    .expect("zero-byte metadata decode");
    assert_eq!(association.result_key(), validation.result_key());
    assert_eq!(
      fs::read(decoded.staging.path().join(METADATA_SLOT)).expect("decoded metadata"),
      b""
    );
  }

  #[test]
  fn pack_decode_rejects_corruption_length_drift_and_trailing_data() {
    let (validation, bytes) = exported_fixture(b"compiler output\n");

    let mut corrupt = bytes.clone();
    let last = corrupt.last_mut().expect("non-empty pack");
    *last ^= 0xff;
    assert!(
      decode_for_action(
        Cursor::new(corrupt),
        validation.action_key(),
        Some(bytes.len() as u64),
        None,
      )
      .is_err(),
      "payload corruption must fail"
    );

    assert!(
      decode_for_action(
        Cursor::new(&bytes),
        validation.action_key(),
        Some(bytes.len() as u64 + 1),
        None,
      )
      .is_err(),
      "declared length drift must fail"
    );

    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(
      decode_for_action(Cursor::new(trailing), validation.action_key(), None, None).is_err(),
      "trailing payload must fail"
    );
  }
}
