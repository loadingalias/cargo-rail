//! Private process worker for retained native-pack resource evidence.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{DecodedNativePack, NativeResultDescriptor, decode_counted};
use crate::compiler::native_cache::{
  ACTION_KEY_PREFIX, DEP_INFO_SLOT, METADATA_SLOT, NativeCompilerValidation, NativeResultIdentity,
  PreparedNativeResult, RLIB_SLOT, RemoteAuthorityId, STDERR_SLOT, STDOUT_SLOT, sha256_identity,
};
use crate::error::{RailError, RailResult};
use crate::hermetic::cas::{LocalCas, NativeActionLookup, NativeCacheLookup};

const MODE_ENV: &str = "CARGO_RAIL_NATIVE_PACK_PROCESS_MODE";
const ROOT_ENV: &str = "CARGO_RAIL_NATIVE_PACK_PROCESS_ROOT";
const BATCH_ENV: &str = "CARGO_RAIL_NATIVE_PACK_PROCESS_BATCH";
const REPORT_ENV: &str = "CARGO_RAIL_NATIVE_PACK_PROCESS_REPORT";
const SLOT_BYTES: u64 = 256 * 1024;
const RESULT_PAYLOAD_BYTES: u64 = SLOT_BYTES * 4;
const CAS_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
  volume: u64,
  index: u64,
  bytes: u64,
}

#[cfg(unix)]
fn file_identity(path: &Path) -> RailResult<FileIdentity> {
  use std::os::unix::fs::MetadataExt as _;

  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1 {
    return Err(RailError::message(format!(
      "native-pack evidence path '{}' is not one private regular file",
      path.display()
    )));
  }
  Ok(FileIdentity {
    volume: metadata.dev(),
    index: metadata.ino(),
    bytes: metadata.len(),
  })
}

#[cfg(windows)]
fn file_identity(path: &Path) -> RailResult<FileIdentity> {
  let file = crate::windows_fs::open_for_observation(path)?;
  let observation = crate::windows_fs::observe_file(&file)?;
  crate::windows_fs::prove_local_ntfs(&file, observation.volume_serial_number)?;
  let current = crate::windows_fs::open_for_observation(path)?;
  let current_observation = crate::windows_fs::observe_file(&current)?;
  crate::windows_fs::prove_local_ntfs(&current, current_observation.volume_serial_number)?;
  if !file.metadata()?.is_file() || observation != current_observation || observation.number_of_links != 1 {
    return Err(RailError::message(format!(
      "native-pack evidence path '{}' is not one private regular file",
      path.display()
    )));
  }
  Ok(FileIdentity {
    volume: observation.volume_serial_number,
    index: observation.file_id,
    bytes: observation.size,
  })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_path: &Path) -> RailResult<FileIdentity> {
  Err(RailError::message(
    "native-pack admission identity evidence is unavailable on this platform",
  ))
}

#[derive(Debug, Clone, Copy, Serialize)]
struct LinuxProcessIo {
  rchar: u64,
  wchar: u64,
  syscr: u64,
  syscw: u64,
  read_bytes: u64,
  write_bytes: u64,
  cancelled_write_bytes: u64,
}

impl LinuxProcessIo {
  fn checked_delta(self, before: Self) -> RailResult<Self> {
    Ok(Self {
      rchar: self
        .rchar
        .checked_sub(before.rchar)
        .ok_or_else(|| RailError::message("Linux rchar accounting moved backwards"))?,
      wchar: self
        .wchar
        .checked_sub(before.wchar)
        .ok_or_else(|| RailError::message("Linux wchar accounting moved backwards"))?,
      syscr: self
        .syscr
        .checked_sub(before.syscr)
        .ok_or_else(|| RailError::message("Linux syscr accounting moved backwards"))?,
      syscw: self
        .syscw
        .checked_sub(before.syscw)
        .ok_or_else(|| RailError::message("Linux syscw accounting moved backwards"))?,
      read_bytes: self
        .read_bytes
        .checked_sub(before.read_bytes)
        .ok_or_else(|| RailError::message("Linux read_bytes accounting moved backwards"))?,
      write_bytes: self
        .write_bytes
        .checked_sub(before.write_bytes)
        .ok_or_else(|| RailError::message("Linux write_bytes accounting moved backwards"))?,
      cancelled_write_bytes: self
        .cancelled_write_bytes
        .checked_sub(before.cancelled_write_bytes)
        .ok_or_else(|| RailError::message("Linux cancelled_write_bytes accounting moved backwards"))?,
    })
  }
}

#[cfg(target_os = "linux")]
fn process_io() -> RailResult<Option<LinuxProcessIo>> {
  let contents = fs::read_to_string("/proc/self/io")?;
  let field = |name: &str| -> RailResult<u64> {
    contents
      .lines()
      .find_map(|line| line.strip_prefix(name))
      .map(str::trim)
      .ok_or_else(|| RailError::message(format!("Linux process I/O omits '{name}'")))?
      .parse::<u64>()
      .map_err(|error| RailError::message(format!("Linux process I/O field '{name}' is malformed: {error}")))
  };
  Ok(Some(LinuxProcessIo {
    rchar: field("rchar:")?,
    wchar: field("wchar:")?,
    syscr: field("syscr:")?,
    syscw: field("syscw:")?,
    read_bytes: field("read_bytes:")?,
    write_bytes: field("write_bytes:")?,
    cancelled_write_bytes: field("cancelled_write_bytes:")?,
  }))
}

#[cfg(not(target_os = "linux"))]
fn process_io() -> RailResult<Option<LinuxProcessIo>> {
  Ok(None)
}

#[derive(Debug, Serialize)]
struct WorkerReport {
  schema_version: u32,
  mode: String,
  batch_size: usize,
  result_payload_bytes: u64,
  logical_payload_bytes: u64,
  logical_pack_bytes: u64,
  logical_staging_bytes: u64,
  logical_admission_bytes: u64,
  logical_restore_bytes: u64,
  results: usize,
  admission_file_identity_preserved: Option<bool>,
  linux_process_io: Option<LinuxProcessIo>,
}

impl WorkerReport {
  fn new(mode: &str, batch_size: usize) -> Self {
    Self {
      schema_version: 1,
      mode: mode.to_string(),
      batch_size,
      result_payload_bytes: RESULT_PAYLOAD_BYTES,
      logical_payload_bytes: RESULT_PAYLOAD_BYTES.saturating_mul(batch_size as u64),
      logical_pack_bytes: 0,
      logical_staging_bytes: 0,
      logical_admission_bytes: 0,
      logical_restore_bytes: 0,
      results: 0,
      admission_file_identity_preserved: None,
      linux_process_io: None,
    }
  }
}

fn worker_root() -> RailResult<PathBuf> {
  std::env::var_os(ROOT_ENV)
    .filter(|value| !value.is_empty())
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message(format!("{ROOT_ENV} is required")))
}

fn worker_batch() -> RailResult<usize> {
  let value = std::env::var(BATCH_ENV).map_err(|_| RailError::message(format!("{BATCH_ENV} is required")))?;
  let batch = value
    .parse::<usize>()
    .map_err(|error| RailError::message(format!("{BATCH_ENV} is invalid: {error}")))?;
  if matches!(batch, 1 | 32 | 256) {
    Ok(batch)
  } else {
    Err(RailError::message(format!("{BATCH_ENV} must be one of 1, 32, or 256")))
  }
}

fn report_path() -> RailResult<PathBuf> {
  std::env::var_os(REPORT_ENV)
    .filter(|value| !value.is_empty())
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message(format!("{REPORT_ENV} is required")))
}

fn write_report(report: &WorkerReport) -> RailResult<()> {
  let path = report_path()?;
  let parent = path
    .parent()
    .ok_or_else(|| RailError::message("native-pack process report has no parent"))?;
  fs::create_dir_all(parent)?;
  let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
  serde_json::to_writer(&mut temporary, report)?;
  temporary.write_all(b"\n")?;
  temporary.as_file().sync_all()?;
  temporary.persist(&path).map_err(|error| error.error)?;
  Ok(())
}

fn slot_byte(slot: &str) -> RailResult<u8> {
  match slot {
    DEP_INFO_SLOT => Ok(b'd'),
    METADATA_SLOT => Ok(b'm'),
    RLIB_SLOT => Ok(b'r'),
    STDOUT_SLOT => Ok(b'o'),
    STDERR_SLOT => Ok(b'e'),
    _ => Err(RailError::message(format!(
      "native-pack evidence encountered unknown slot '{slot}'"
    ))),
  }
}

fn slot_digest(slot: &str) -> RailResult<String> {
  let bytes = vec![slot_byte(slot)?; SLOT_BYTES as usize];
  Ok(format!("sha256:{}", crate::source::ContentDigest::sha256(&bytes)))
}

fn set_slot_mode(path: &Path, mode: u32) -> RailResult<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
  }
  #[cfg(not(unix))]
  {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, permissions)?;
  }
  Ok(())
}

fn write_slot(path: &Path, slot: &str, mode: u32) -> RailResult<()> {
  let parent = path
    .parent()
    .ok_or_else(|| RailError::message("native-pack evidence slot has no parent"))?;
  fs::create_dir_all(parent)?;
  let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
  let buffer = vec![slot_byte(slot)?; 64 * 1024];
  let mut remaining = SLOT_BYTES;
  while remaining > 0 {
    let write = usize::try_from(remaining.min(buffer.len() as u64))
      .map_err(|_| RailError::message("native-pack evidence write size is out of range"))?;
    output.write_all(&buffer[..write])?;
    remaining -= write as u64;
  }
  output.sync_all()?;
  set_slot_mode(path, mode)
}

fn write_result_slots(root: &Path, validation: &NativeCompilerValidation) -> RailResult<Vec<PathBuf>> {
  let bindings = validation
    .cas_output_bindings()
    .chain(validation.cas_stream_bindings())
    .collect::<Vec<_>>();
  let mut paths = Vec::with_capacity(bindings.len());
  for (slot, _, bytes, mode) in bindings {
    if bytes != SLOT_BYTES {
      return Err(RailError::message(
        "native-pack evidence fixture has an unexpected slot length",
      ));
    }
    let path = root.join(slot);
    write_slot(&path, slot, mode)?;
    paths.push(path);
  }
  Ok(paths)
}

fn validation_template() -> RailResult<NativeCompilerValidation> {
  let stdout = vec![slot_byte(STDOUT_SLOT)?; SLOT_BYTES as usize];
  let mut validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(&stdout);
  for output in &mut validation.outputs {
    output.bytes = SLOT_BYTES;
    output.content_digest = slot_digest(&output.slot)?;
  }
  for (observed, output) in validation
    .observation
    .emitted_outputs
    .iter_mut()
    .zip(&validation.outputs)
  {
    observed.content_digest.clone_from(&output.content_digest);
  }
  validation.stdout_bytes = SLOT_BYTES;
  validation.stdout_digest = slot_digest(STDOUT_SLOT)?;
  validation.stderr_bytes = SLOT_BYTES;
  validation.stderr_digest = slot_digest(STDERR_SLOT)?;
  Ok(validation)
}

fn validation_for_index(template: &NativeCompilerValidation, index: usize) -> RailResult<NativeCompilerValidation> {
  let mut validation = template.clone();
  let index = (index as u64).to_le_bytes();
  validation.action_key = sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-pack-process-evidence-action\0",
    &[(b"index", &index)],
  );
  validation.result_key = NativeResultDescriptor::from_identity(NativeResultIdentity {
    action_key: validation.action_key(),
    witness: &validation.witness,
    outputs: &validation.outputs,
    stdout_digest: &validation.stdout_digest,
    stdout_bytes: validation.stdout_bytes,
    stderr_digest: &validation.stderr_digest,
    stderr_bytes: validation.stderr_bytes,
  })?
  .result_key()?;
  validation.validate_object()?;
  Ok(validation)
}

fn validations_path(root: &Path) -> PathBuf {
  root.join("validations.json")
}

fn load_validations(root: &Path, batch_size: usize) -> RailResult<Vec<NativeCompilerValidation>> {
  let bytes = fs::read(validations_path(root))?;
  let validations = serde_json::from_slice::<Vec<NativeCompilerValidation>>(&bytes)?;
  if validations.len() != batch_size {
    return Err(RailError::message(format!(
      "native-pack evidence expected {batch_size} validations, found {}",
      validations.len()
    )));
  }
  for validation in &validations {
    validation.validate_object()?;
  }
  Ok(validations)
}

fn source_base(root: &Path) -> PathBuf {
  root.join("source-cas")
}

fn destination_base(root: &Path) -> PathBuf {
  root.join("destination-cas")
}

fn pack_path(root: &Path, index: usize) -> PathBuf {
  root.join("packs").join(format!("{index:03}.pack"))
}

fn manifest_for(validation: &NativeCompilerValidation) -> RailResult<crate::hermetic::OutputManifest> {
  let outputs = validation.cas_output_bindings().collect::<Vec<_>>();
  let streams = validation.cas_stream_bindings();
  let slots = outputs.iter().copied().chain(streams).collect::<Vec<_>>();
  crate::hermetic::manifest_from_verified_native_slots(&slots)
}

fn setup(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  fs::create_dir_all(root)?;
  let source = LocalCas::open_at(&source_base(root), CAS_BYTES)?;
  let _destination = LocalCas::open_at(&destination_base(root), CAS_BYTES)?;
  fs::create_dir_all(root.join("packs"))?;
  let template = validation_template()?;
  let mut validations = Vec::with_capacity(batch_size);
  for index in 0..batch_size {
    let validation = validation_for_index(&template, index)?;
    let staging = tempfile::Builder::new()
      .prefix("native-pack-process-source-")
      .tempdir_in(root)?;
    write_result_slots(staging.path(), &validation)?;
    let manifest = manifest_for(&validation)?;
    source.store_native(PreparedNativeResult::from_verified_staging(
      staging,
      manifest,
      validation.clone(),
    ))?;
    validations.push(validation);
  }
  fs::write(validations_path(root), serde_json::to_vec(&validations)?)?;
  let mut report = WorkerReport::new("setup", batch_size);
  report.results = validations.len();
  Ok(report)
}

fn baseline(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  let validations = load_validations(root, batch_size)?;
  let _destination = LocalCas::open_at(&destination_base(root), CAS_BYTES)?;
  let before = process_io()?;
  let after = process_io()?;
  let mut report = WorkerReport::new("baseline", batch_size);
  report.results = validations.len();
  report.linux_process_io = after
    .zip(before)
    .map(|(after, before)| after.checked_delta(before))
    .transpose()?;
  Ok(report)
}

fn control(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  let control = root.join("control");
  fs::create_dir(&control)?;
  let buffer = vec![b'c'; 64 * 1024];
  let before = process_io()?;
  for result in 0..batch_size {
    for slot in 0..4 {
      let path = control.join(format!("{result:03}-{slot}.payload"));
      let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
      let mut remaining = SLOT_BYTES;
      while remaining > 0 {
        let write = usize::try_from(remaining.min(buffer.len() as u64))
          .map_err(|_| RailError::message("native-pack control write size is out of range"))?;
        output.write_all(&buffer[..write])?;
        remaining -= write as u64;
      }
      output.sync_all()?;
    }
  }
  let after = process_io()?;
  let mut report = WorkerReport::new("control", batch_size);
  report.results = batch_size;
  report.logical_staging_bytes = report.logical_payload_bytes;
  report.linux_process_io = after
    .zip(before)
    .map(|(after, before)| after.checked_delta(before))
    .transpose()?;
  Ok(report)
}

fn export(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  let validations = load_validations(root, batch_size)?;
  let source = LocalCas::open_at(&source_base(root), CAS_BYTES)?;
  let before = process_io()?;
  let mut pack_bytes = 0u64;
  for (index, validation) in validations.iter().enumerate() {
    let NativeActionLookup::Hit(hit) = source.native_action(validation.action_key())? else {
      return Err(RailError::message(
        "native-pack export source action is not authoritative",
      ));
    };
    let mut output = OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(pack_path(root, index))?;
    let exported = hit.export_pack(&mut output)?;
    output.sync_all()?;
    if exported.content_length != exported.bytes_written {
      return Err(RailError::message(
        "native-pack export returned inconsistent byte counts",
      ));
    }
    pack_bytes = pack_bytes
      .checked_add(exported.bytes_written)
      .ok_or_else(|| RailError::message("native-pack export byte count overflow"))?;
  }
  let after = process_io()?;
  let mut report = WorkerReport::new("export", batch_size);
  report.results = validations.len();
  report.logical_pack_bytes = pack_bytes;
  report.linux_process_io = after
    .zip(before)
    .map(|(after, before)| after.checked_delta(before))
    .transpose()?;
  Ok(report)
}

fn directory_entries(path: &Path) -> RailResult<BTreeSet<String>> {
  fs::read_dir(path)?
    .map(|entry| {
      entry?
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("native-pack evidence found a non-UTF-8 directory entry"))
    })
    .collect()
}

fn import(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  let validations = load_validations(root, batch_size)?;
  let destination = LocalCas::open_at(&destination_base(root), CAS_BYTES)?;
  let authority = RemoteAuthorityId::for_test("native-pack-process-evidence")?;
  let before = process_io()?;
  let mut pack_bytes = 0u64;
  let mut staging_bytes = 0u64;
  let mut admission_bytes = 0u64;
  let mut identity_preserved = true;
  for (index, validation) in validations.iter().enumerate() {
    let pack = File::open(pack_path(root, index))?;
    let length = pack.metadata()?.len();
    let (decoded, staged) = decode_counted(
      pack,
      validation.action_key(),
      validation.result_key(),
      Some(length),
      Some(destination.native_result_staging()?),
    )?;
    let association = super::association(validation)?;
    if decoded.descriptor.encode()? != association.bytes() {
      return Err(RailError::message(
        "native-pack import descriptor differs from its validated association",
      ));
    }
    let staged_identities = decoded
      .descriptor
      .slots()
      .into_iter()
      .map(|slot| file_identity(&decoded.staging.path().join(slot.path)))
      .collect::<RailResult<BTreeSet<_>>>()?;
    let manifest = manifest_for(validation)?;
    let results = destination.root().join("results");
    let before_results = directory_entries(&results)?;
    let DecodedNativePack {
      staging,
      descriptor: _,
      bytes_read,
    } = decoded;
    let (staging, staging_lock, command_scoped) = staging.into_parts();
    if staging_lock.is_none() || command_scoped {
      return Err(RailError::message(
        "native-pack import did not retain guarded same-filesystem staging",
      ));
    }
    let prepared = PreparedNativeResult::from_authenticated_pack(
      staging,
      staging_lock,
      manifest,
      validation.clone(),
      authority.clone(),
    );
    let (_, stats) = destination.store_native(prepared)?;
    let after_results = directory_entries(&results)?;
    let added = after_results
      .difference(&before_results)
      .map(String::as_str)
      .collect::<Vec<_>>();
    if added.len() != 1 {
      return Err(RailError::message(format!(
        "native-pack admission exposed {} new result directories instead of one",
        added.len()
      )));
    }
    let admitted_identities = fs::read_dir(results.join(added[0]).join("blobs"))?
      .map(|entry| file_identity(&entry?.path()))
      .collect::<RailResult<BTreeSet<_>>>()?;
    identity_preserved &= admitted_identities == staged_identities;
    let NativeActionLookup::Hit(hit) =
      destination.native_action_for_authority(validation.action_key(), Some(&authority))?
    else {
      return Err(RailError::message(
        "native-pack admission did not create remote-scoped local authority",
      ));
    };
    if hit.validation != *validation {
      return Err(RailError::message(
        "native-pack admission changed the validated result descriptor",
      ));
    }
    pack_bytes = pack_bytes
      .checked_add(bytes_read)
      .ok_or_else(|| RailError::message("native-pack import byte count overflow"))?;
    staging_bytes = staging_bytes
      .checked_add(staged)
      .ok_or_else(|| RailError::message("native-pack staging byte count overflow"))?;
    admission_bytes = admission_bytes
      .checked_add(stats.bytes_written)
      .ok_or_else(|| RailError::message("native-pack admission byte count overflow"))?;
  }
  let after = process_io()?;
  let mut report = WorkerReport::new("import", batch_size);
  report.results = validations.len();
  report.logical_pack_bytes = pack_bytes;
  report.logical_staging_bytes = staging_bytes;
  report.logical_admission_bytes = admission_bytes;
  report.admission_file_identity_preserved = Some(identity_preserved);
  report.linux_process_io = after
    .zip(before)
    .map(|(after, before)| after.checked_delta(before))
    .transpose()?;
  Ok(report)
}

fn restore(root: &Path, batch_size: usize) -> RailResult<WorkerReport> {
  let validations = load_validations(root, batch_size)?;
  let destination = LocalCas::open_at(&destination_base(root), CAS_BYTES)?;
  let authority = RemoteAuthorityId::for_test("native-pack-process-evidence")?;
  let restore_root = root.join("restored");
  fs::create_dir(&restore_root)?;
  let before = process_io()?;
  let mut restored = 0u64;
  for (index, validation) in validations.iter().enumerate() {
    let NativeActionLookup::Hit(hit) =
      destination.native_action_for_authority(validation.action_key(), Some(&authority))?
    else {
      return Err(RailError::message("native-pack restore action is not authoritative"));
    };
    let output = restore_root.join(format!("{index:03}"));
    let NativeCacheLookup::Hit(stats) = hit.restore(&output) else {
      return Err(RailError::message("native-pack ordinary restore missed"));
    };
    for (slot, _, bytes, _) in validation.cas_output_bindings().chain(validation.cas_stream_bindings()) {
      if fs::metadata(output.join(slot))?.len() != bytes {
        return Err(RailError::message(format!(
          "native-pack ordinary restore changed slot '{slot}' length"
        )));
      }
    }
    restored = restored
      .checked_add(stats.bytes_restored)
      .ok_or_else(|| RailError::message("native-pack restore byte count overflow"))?;
  }
  let after = process_io()?;
  let mut report = WorkerReport::new("restore", batch_size);
  report.results = validations.len();
  report.logical_restore_bytes = restored;
  report.linux_process_io = after
    .zip(before)
    .map(|(after, before)| after.checked_delta(before))
    .transpose()?;
  Ok(report)
}

fn run_worker() -> RailResult<()> {
  let mode = std::env::var(MODE_ENV).map_err(|_| RailError::message(format!("{MODE_ENV} is required")))?;
  let root = worker_root()?;
  let batch_size = worker_batch()?;
  let report = match mode.as_str() {
    "setup" => setup(&root, batch_size)?,
    "baseline" => baseline(&root, batch_size)?,
    "control" => control(&root, batch_size)?,
    "export" => export(&root, batch_size)?,
    "import" => import(&root, batch_size)?,
    "restore" => restore(&root, batch_size)?,
    _ => return Err(RailError::message(format!("unknown native-pack process mode '{mode}'"))),
  };
  write_report(&report)
}

#[test]
#[ignore = "retained process evidence; run through scripts/bench/native-pack-process-evidence.py"]
fn native_pack_process_worker() {
  run_worker().expect("native-pack process worker should complete");
}
