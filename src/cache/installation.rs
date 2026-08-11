//! Exact machine installation for transparent local compiler reuse.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::cache::cas::{DEFAULT_CACHE_MAX_BYTES, LocalCacheSelection, LocalCas};
use crate::cargo::CargoConfigSnapshot;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const INSTALLATION_VERSION: u32 = 3;
const INSTALLATION_DIRECTORY: &str = "compiler-cache-v1";
const RECEIPT_FILE: &str = "setup.json";
const SESSION_MEMO_FILE: &str = "session.json";
const SESSION_LOCK_FILE: &str = "session.lock";
const USAGE_FILE: &str = "usage-v1.log";
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_SESSION_MEMO_BYTES: u64 = 256 * 1024;
const MAX_USAGE_BYTES: u64 = 64 * 1024;
#[cfg(not(windows))]
const WRAPPER_FILE: &str = "cargo-rail-native-rustc-wrapper";
#[cfg(windows)]
const WRAPPER_FILE: &str = "cargo-rail-native-rustc-wrapper.exe";
#[cfg(not(windows))]
const WORKER_FILE: &str = "cargo-rail-native-rustc-worker";
#[cfg(windows)]
const WORKER_FILE: &str = "cargo-rail-native-rustc-worker.exe";
const DIRECT_LAUNCHER_ENV: &str = "CARGO_RAIL_DIRECT_CACHE_LAUNCHER";

/// Requested machine policy. Omitted values preserve an existing installation.
#[derive(Debug, Clone, Default)]
pub(crate) struct SetupRequest {
  pub(crate) local_dir: Option<PathBuf>,
  pub(crate) max_bytes: Option<u64>,
}

/// One lossless Cargo configuration and private-state mutation preview.
pub(crate) struct SetupPlan {
  cargo_home: PathBuf,
  config_path: PathBuf,
  config_before: Option<Vec<u8>>,
  config_after: Vec<u8>,
  receipt_before: Option<Vec<u8>>,
  receipt: InstallationReceipt,
  source_wrapper: PathBuf,
  source_wrapper_digest: String,
  source_worker: PathBuf,
  source_worker_digest: String,
  pending: bool,
}

/// One exact uninstall preview. The local compiler-result cache is deliberately
/// outside this authority and is never removed here.
pub(crate) struct RemovalPlan {
  config_path: PathBuf,
  config_before: Option<Vec<u8>>,
  config_after: Option<Vec<u8>>,
  receipt_before: Option<Vec<u8>>,
  receipt: Option<InstallationReceipt>,
  wrapper_before_digest: Option<String>,
  worker_before_digest: Option<String>,
}

impl RemovalPlan {
  pub(crate) fn pending(&self) -> bool {
    self.receipt.is_some()
  }

  pub(crate) fn config_path(&self) -> &Path {
    &self.config_path
  }

  pub(crate) fn wrapper_path(&self) -> Option<&Path> {
    self.receipt.as_ref().map(InstallationReceipt::wrapper_path)
  }

  pub(crate) fn receipt_path(&self) -> Option<PathBuf> {
    self
      .receipt
      .as_ref()
      .and_then(|receipt| receipt.installation_directory().ok())
      .map(|directory| directory.join(RECEIPT_FILE))
  }

  pub(crate) fn config_action(&self) -> &'static str {
    match (&self.config_before, &self.config_after) {
      (None, _) => "unchanged",
      (Some(_), None) => "remove_file",
      (Some(before), Some(after)) if before == after => "unchanged",
      (Some(_), Some(_)) => "remove_field",
    }
  }
}

impl SetupPlan {
  pub(crate) const fn pending(&self) -> bool {
    self.pending
  }

  pub(crate) fn config_path(&self) -> &Path {
    &self.config_path
  }

  pub(crate) fn wrapper_path(&self) -> &Path {
    &self.receipt.wrapper_path
  }

  pub(crate) fn cache_base(&self) -> &Path {
    self.receipt.cache.base()
  }

  pub(crate) const fn max_bytes(&self) -> u64 {
    self.receipt.cache.max_bytes()
  }

  pub(crate) fn receipt_path(&self) -> RailResult<PathBuf> {
    Ok(self.receipt.installation_directory()?.join(RECEIPT_FILE))
  }

  pub(crate) fn config_action(&self) -> &'static str {
    match self.config_before.as_deref() {
      None => "create_file_and_field",
      Some(before) if before == self.config_after => "unchanged",
      Some(_) => "set_field",
    }
  }
}

/// Private authority loaded by the installed wrapper after acquisition-free gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallationReceipt {
  version: u32,
  authority: String,
  cargo_home: PathBuf,
  config_path: PathBuf,
  config_created: bool,
  build_table_created: bool,
  wrapper_path: PathBuf,
  wrapper_digest: String,
  wrapper_generation: Vec<u8>,
  worker_path: PathBuf,
  worker_digest: String,
  worker_generation: Vec<u8>,
  cache: LocalCacheSelection,
}

pub(crate) struct InstallationSessionLock {
  _file: File,
}

/// Bounded observational counts. These never authorize a restore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct InstallationUsageStatus {
  pub(crate) recorded_events: u64,
  pub(crate) hits: u64,
  pub(crate) misses: u64,
  pub(crate) bypasses: u64,
  pub(crate) failures: u64,
  pub(crate) ledger_full: bool,
  pub(crate) early_bypasses: &'static str,
}

impl InstallationReceipt {
  pub(crate) fn authority(&self) -> &str {
    &self.authority
  }

  pub(crate) fn cache(&self) -> &LocalCacheSelection {
    &self.cache
  }

  pub(crate) fn wrapper_path(&self) -> &Path {
    &self.wrapper_path
  }

  fn installation_directory(&self) -> RailResult<&Path> {
    self
      .wrapper_path
      .parent()
      .ok_or_else(|| RailError::message("installed compiler wrapper has no parent"))
  }

  fn session_memo_path(&self) -> RailResult<PathBuf> {
    Ok(self.installation_directory()?.join(SESSION_MEMO_FILE))
  }

  fn session_lock_path(&self) -> RailResult<PathBuf> {
    Ok(self.installation_directory()?.join(SESSION_LOCK_FILE))
  }

  fn usage_path(&self) -> RailResult<PathBuf> {
    Ok(self.installation_directory()?.join(USAGE_FILE))
  }

  fn validate(&self) -> RailResult<()> {
    if self.version != INSTALLATION_VERSION
      || !valid_hex_digest(&self.authority)
      || !valid_sha256(&self.wrapper_digest)
      || !valid_sha256(&self.worker_digest)
      || self.wrapper_generation.is_empty()
      || self.wrapper_generation.len() > 512
      || self.worker_generation.is_empty()
      || self.worker_generation.len() > 512
      || !self.cargo_home.is_absolute()
      || !self.config_path.is_absolute()
      || !self.wrapper_path.is_absolute()
      || !self.worker_path.is_absolute()
      || self.config_path.parent() != Some(self.cargo_home.as_path())
      || self.wrapper_path.parent()
        != Some(
          self
            .cargo_home
            .join("cargo-rail")
            .join(INSTALLATION_DIRECTORY)
            .as_path(),
        )
      || self.worker_path.parent() != self.wrapper_path.parent()
      || self.wrapper_path.file_name() != Some(std::ffi::OsStr::new(WRAPPER_FILE))
      || self.worker_path.file_name() != Some(std::ffi::OsStr::new(WORKER_FILE))
    {
      return Err(RailError::message(
        "transparent compiler-cache installation receipt is invalid",
      ));
    }
    LocalCacheSelection::new(
      self.cache.base().to_path_buf(),
      self.cache.max_bytes(),
      self.cache.trust_domain().map(str::to_string),
    )?;
    Ok(())
  }
}

pub(crate) fn load_session_memo(receipt: &InstallationReceipt) -> RailResult<Option<Vec<u8>>> {
  read_optional_regular(&receipt.session_memo_path()?, MAX_SESSION_MEMO_BYTES)
}

pub(crate) fn lock_session(receipt: &InstallationReceipt) -> RailResult<InstallationSessionLock> {
  let path = receipt.session_lock_path()?;
  let file = crate::utils::open_cache_lock_file(&path, true)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
  }
  if !crate::utils::private_file_matches_path(&file, &path, 0)? {
    return Err(RailError::message(
      "transparent compiler session lock is not a private regular file",
    ));
  }
  file.lock()?;
  if !crate::utils::private_file_matches_path(&file, &path, 0)? {
    return Err(RailError::message(
      "transparent compiler session lock changed while it was acquired",
    ));
  }
  Ok(InstallationSessionLock { _file: file })
}

pub(crate) fn store_session_memo(receipt: &InstallationReceipt, bytes: &[u8]) -> RailResult<()> {
  if bytes.len() as u64 > MAX_SESSION_MEMO_BYTES {
    return Err(RailError::message(
      "transparent compiler session memo exceeds its bound",
    ));
  }
  write_private_atomic(&receipt.session_memo_path()?, bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InstallationStatus {
  pub(crate) state: &'static str,
  pub(crate) healthy: bool,
  pub(crate) cargo_home: String,
  pub(crate) config_path: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) wrapper_path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) cache_base: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) max_bytes: Option<u64>,
  pub(crate) cargo_l0: &'static str,
  pub(crate) usage: InstallationUsageStatus,
  pub(crate) issues: Vec<String>,
}

/// Record one post-context transparent wrapper outcome in a bounded private log.
///
/// This deliberately ignores failures: cache reuse and the compiler process
/// must never depend on observational status accounting.
pub(crate) fn record_usage(receipt: &InstallationReceipt, outcome: u8) {
  if !matches!(outcome, b'H' | b'M' | b'B' | b'F') {
    return;
  }
  let Ok(path) = receipt.usage_path() else {
    return;
  };
  let Ok(mut file) = crate::utils::open_cache_lock_file(&path, true) else {
    return;
  };
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if file.set_permissions(fs::Permissions::from_mode(0o600)).is_err() {
      return;
    }
  }
  if file.lock().is_err() {
    return;
  }
  let Ok(metadata) = file.metadata() else {
    return;
  };
  if metadata.len() >= MAX_USAGE_BYTES
    || !crate::utils::private_file_matches_path(&file, &path, metadata.len()).unwrap_or(false)
    || file.seek(std::io::SeekFrom::End(0)).is_err()
  {
    return;
  }
  let _ = file.write_all(&[outcome]);
}

fn usage_status(receipt: &InstallationReceipt) -> RailResult<InstallationUsageStatus> {
  let path = receipt.usage_path()?;
  let mut file = match crate::utils::open_cache_lock_file(&path, false) {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      return Ok(InstallationUsageStatus {
        early_bypasses: "not_recorded_before_context_acquisition",
        ..InstallationUsageStatus::default()
      });
    }
    Err(error) => return Err(error.into()),
  };
  file.lock()?;
  let metadata = file.metadata()?;
  if metadata.len() > MAX_USAGE_BYTES || !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
    return Err(RailError::message(
      "transparent compiler-cache usage log is not a bounded private regular file",
    ));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  file.read_to_end(&mut bytes)?;
  if bytes.len() as u64 != metadata.len() {
    return Err(RailError::message(
      "transparent compiler-cache usage log changed while it was read",
    ));
  }
  let mut usage = InstallationUsageStatus {
    recorded_events: bytes.len() as u64,
    ledger_full: bytes.len() as u64 == MAX_USAGE_BYTES,
    early_bypasses: "not_recorded_before_context_acquisition",
    ..InstallationUsageStatus::default()
  };
  for byte in bytes {
    match byte {
      b'H' => usage.hits = usage.hits.saturating_add(1),
      b'M' => usage.misses = usage.misses.saturating_add(1),
      b'B' => usage.bypasses = usage.bypasses.saturating_add(1),
      b'F' => usage.failures = usage.failures.saturating_add(1),
      _ => return Err(RailError::message("transparent compiler-cache usage log is malformed")),
    }
  }
  Ok(usage)
}

/// Build a no-write setup plan from Cargo's current hierarchy and machine state.
pub(crate) fn plan_setup(current_dir: &Path, request: &SetupRequest) -> RailResult<SetupPlan> {
  ensure_supported_installation_platform()?;
  let cargo_home = resolve_cargo_home(current_dir)?;
  let install_directory = cargo_home.join("cargo-rail").join(INSTALLATION_DIRECTORY);
  let receipt_path = install_directory.join(RECEIPT_FILE);
  let receipt_before = read_optional_regular(&receipt_path, MAX_RECEIPT_BYTES)?;
  let existing = receipt_before.as_deref().map(parse_receipt).transpose()?;

  let config_path = selected_user_config(&cargo_home);
  if let Some(existing) = &existing
    && (existing.cargo_home != cargo_home || existing.config_path != config_path)
  {
    return Err(RailError::message(
      "transparent compiler-cache setup authority does not match the selected Cargo home",
    ));
  }
  reject_shadowing_global_wrapper(current_dir, &config_path)?;

  let config_before = read_optional_regular(&config_path, 16 * 1024 * 1024)?;
  let original = config_before
    .as_deref()
    .map(|bytes| {
      std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| RailError::message("Cargo user configuration is not valid UTF-8"))
    })
    .transpose()?
    .unwrap_or_default();
  let wrapper_path = install_directory.join(WRAPPER_FILE);
  let worker_path = install_directory.join(WORKER_FILE);
  let (config_after, build_table_created) = install_wrapper_value(&original, &wrapper_path, existing.as_ref())?;
  let source_wrapper = crate::compiler::native_cache::direct_wrapper_executable()?;
  let source_wrapper_digest = file_digest(&source_wrapper)?;
  let source_worker = crate::compiler::native_cache::direct_worker_executable()?;
  let source_worker_digest = file_digest(&source_worker)?;
  let wrapper_current = optional_file_digest(&wrapper_path)?;
  let wrapper_generation = if wrapper_current.as_deref() == Some(source_wrapper_digest.as_str()) {
    crate::utils::stable_file_generation(&wrapper_path)
      .ok_or_else(|| RailError::message("installed compiler wrapper has no stable local file generation"))?
  } else {
    vec![0]
  };
  let worker_current = optional_file_digest(&worker_path)?;
  let worker_generation = if worker_current.as_deref() == Some(source_worker_digest.as_str()) {
    crate::utils::stable_file_generation(&worker_path)
      .ok_or_else(|| RailError::message("installed compiler worker has no stable local file generation"))?
  } else {
    vec![0]
  };
  let cache_base = request
    .local_dir
    .as_deref()
    .map(|path| resolve_requested_path(current_dir, path))
    .transpose()?
    .or_else(|| existing.as_ref().map(|receipt| receipt.cache.base().to_path_buf()))
    .unwrap_or_else(|| cargo_home.clone());
  let max_bytes = request
    .max_bytes
    .or_else(|| existing.as_ref().map(|receipt| receipt.cache.max_bytes()))
    .unwrap_or(DEFAULT_CACHE_MAX_BYTES);
  let cache = LocalCacheSelection::new(cache_base, max_bytes, None)?;
  let authority = existing
    .as_ref()
    .map(|receipt| receipt.authority.clone())
    .map_or_else(random_authority, Ok)?;
  let receipt = InstallationReceipt {
    version: INSTALLATION_VERSION,
    authority,
    cargo_home: cargo_home.clone(),
    config_path: config_path.clone(),
    config_created: existing
      .as_ref()
      .map_or(config_before.is_none(), |receipt| receipt.config_created),
    build_table_created: existing
      .as_ref()
      .map_or(build_table_created, |receipt| receipt.build_table_created),
    wrapper_path,
    wrapper_digest: source_wrapper_digest.clone(),
    wrapper_generation,
    worker_path,
    worker_digest: source_worker_digest.clone(),
    worker_generation,
    cache,
  };
  receipt.validate()?;
  let encoded_receipt = encode_receipt(&receipt)?;
  let cache_ready = receipt.cache.configured_root()?.is_some();
  let pending = config_before.as_deref() != Some(config_after.as_bytes())
    || receipt_before.as_deref() != Some(encoded_receipt.as_slice())
    || wrapper_current.as_deref() != Some(source_wrapper_digest.as_str())
    || worker_current.as_deref() != Some(source_worker_digest.as_str())
    || !cache_ready;
  Ok(SetupPlan {
    cargo_home,
    config_path,
    config_before,
    config_after: config_after.into_bytes(),
    receipt_before,
    receipt,
    source_wrapper,
    source_wrapper_digest,
    source_worker,
    source_worker_digest,
    pending,
  })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ensure_supported_installation_platform() -> RailResult<()> {
  Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn ensure_supported_installation_platform() -> RailResult<()> {
  Err(RailError::message(
    "transparent compiler-cache setup is unsupported on this platform",
  ))
}

/// Apply one setup plan after revalidating every opened input.
pub(crate) fn apply_setup(mut plan: SetupPlan) -> RailResult<()> {
  if !plan.pending {
    return Ok(());
  }
  revalidate_optional(&plan.config_path, plan.config_before.as_deref(), 16 * 1024 * 1024)?;
  let receipt_path = plan
    .receipt
    .wrapper_path
    .parent()
    .ok_or_else(|| RailError::message("installed compiler wrapper has no parent"))?
    .join(RECEIPT_FILE);
  revalidate_optional(&receipt_path, plan.receipt_before.as_deref(), MAX_RECEIPT_BYTES)?;
  if file_digest(&plan.source_wrapper)? != plan.source_wrapper_digest {
    return Err(RailError::message(
      "compiler wrapper executable changed after setup planning",
    ));
  }
  if file_digest(&plan.source_worker)? != plan.source_worker_digest {
    return Err(RailError::message(
      "compiler worker executable changed after setup planning",
    ));
  }

  ensure_real_directory(&plan.cargo_home)?;
  let owner = plan.cargo_home.join("cargo-rail");
  create_private_directory(&owner)?;
  let install_directory = owner.join(INSTALLATION_DIRECTORY);
  create_private_directory(&install_directory)?;
  let _session_lock = lock_session(&plan.receipt)?;
  if read_optional_regular(&plan.receipt.session_memo_path()?, MAX_SESSION_MEMO_BYTES)?.is_some() {
    fs::remove_file(plan.receipt.session_memo_path()?)?;
  }
  LocalCas::open_selected(&plan.receipt.cache)?;
  install_executable_atomic(&plan.source_wrapper, &plan.receipt.wrapper_path)?;
  install_executable_atomic(&plan.source_worker, &plan.receipt.worker_path)?;
  if file_digest(&plan.receipt.wrapper_path)? != plan.receipt.wrapper_digest {
    return Err(RailError::message(
      "installed compiler wrapper failed content verification",
    ));
  }
  plan.receipt.wrapper_generation = crate::utils::stable_file_generation(&plan.receipt.wrapper_path)
    .ok_or_else(|| RailError::message("installed compiler wrapper has no stable local file generation"))?;
  if file_digest(&plan.receipt.worker_path)? != plan.receipt.worker_digest {
    return Err(RailError::message(
      "installed compiler worker failed content verification",
    ));
  }
  plan.receipt.worker_generation = crate::utils::stable_file_generation(&plan.receipt.worker_path)
    .ok_or_else(|| RailError::message("installed compiler worker has no stable local file generation"))?;
  write_private_atomic(&receipt_path, &encode_receipt(&plan.receipt)?)?;
  revalidate_optional(&plan.config_path, plan.config_before.as_deref(), 16 * 1024 * 1024)?;
  crate::utils::write_file_atomic(&plan.config_path, &plan.config_after)?;
  Ok(())
}

/// Build a no-write exact removal plan for the selected Cargo home.
pub(crate) fn plan_removal(current_dir: &Path) -> RailResult<RemovalPlan> {
  let cargo_home = resolve_cargo_home(current_dir)?;
  let config_path = selected_user_config(&cargo_home);
  let receipt_path = cargo_home
    .join("cargo-rail")
    .join(INSTALLATION_DIRECTORY)
    .join(RECEIPT_FILE);
  let receipt_before = read_optional_regular(&receipt_path, MAX_RECEIPT_BYTES)?;
  let Some(receipt_bytes) = receipt_before else {
    return Ok(RemovalPlan {
      config_path,
      config_before: None,
      config_after: None,
      receipt_before: None,
      receipt: None,
      wrapper_before_digest: None,
      worker_before_digest: None,
    });
  };
  let receipt = parse_receipt(&receipt_bytes)?;
  if receipt.cargo_home != cargo_home || receipt.config_path != config_path {
    return Err(RailError::message(
      "transparent compiler-cache removal authority does not match the selected Cargo home",
    ));
  }
  let config_before = read_optional_regular(&config_path, 16 * 1024 * 1024)?;
  if config_before.is_none() && !receipt.config_created {
    return Err(RailError::message(
      "Cargo user configuration owned by another authority is missing",
    ));
  }
  let config_after = config_before
    .as_deref()
    .map(|bytes| {
      std::str::from_utf8(bytes)
        .map_err(|_| RailError::message("Cargo user configuration is not valid UTF-8"))
        .and_then(|contents| remove_wrapper_value(contents, &receipt))
    })
    .transpose()?
    .flatten();
  let wrapper_before_digest = match optional_file_digest(&receipt.wrapper_path)? {
    Some(digest) if digest == receipt.wrapper_digest => Some(digest),
    Some(_) => {
      return Err(RailError::message(
        "installed compiler wrapper content changed; removal refused",
      ));
    }
    None => None,
  };
  let worker_before_digest = match optional_file_digest(&receipt.worker_path)? {
    Some(digest) if digest == receipt.worker_digest => Some(digest),
    Some(_) => {
      return Err(RailError::message(
        "installed compiler worker content changed; removal refused",
      ));
    }
    None => None,
  };
  Ok(RemovalPlan {
    config_path,
    config_before,
    config_after,
    receipt_before: Some(receipt_bytes),
    receipt: Some(receipt),
    wrapper_before_digest,
    worker_before_digest,
  })
}

/// Remove only configuration and private installation state bound by one receipt.
pub(crate) fn apply_removal(plan: RemovalPlan) -> RailResult<()> {
  let Some(receipt) = plan.receipt else {
    return Ok(());
  };
  let install_directory = receipt
    .wrapper_path
    .parent()
    .ok_or_else(|| RailError::message("installed compiler wrapper has no parent"))?;
  let receipt_path = install_directory.join(RECEIPT_FILE);
  revalidate_optional(&plan.config_path, plan.config_before.as_deref(), 16 * 1024 * 1024)?;
  revalidate_optional(&receipt_path, plan.receipt_before.as_deref(), MAX_RECEIPT_BYTES)?;
  match (
    &plan.wrapper_before_digest,
    optional_file_digest(&receipt.wrapper_path)?,
  ) {
    (Some(expected), Some(current)) if expected == &current => {}
    (None, None) => {}
    _ => {
      return Err(RailError::with_help(
        "installed compiler wrapper changed after removal planning",
        "rerun the command after inspecting the installation drift",
      ));
    }
  }
  match (&plan.worker_before_digest, optional_file_digest(&receipt.worker_path)?) {
    (Some(expected), Some(current)) if expected == &current => {}
    (None, None) => {}
    _ => {
      return Err(RailError::with_help(
        "installed compiler worker changed after removal planning",
        "rerun the command after inspecting the installation drift",
      ));
    }
  }

  match plan.config_after {
    Some(contents) => crate::utils::write_file_atomic(&plan.config_path, &contents)?,
    None if plan.config_before.is_some() => fs::remove_file(&plan.config_path)?,
    None => {}
  }
  revalidate_optional(&receipt_path, plan.receipt_before.as_deref(), MAX_RECEIPT_BYTES)?;
  let session_lock = lock_session(&receipt)?;
  if plan.wrapper_before_digest.is_some() {
    fs::remove_file(&receipt.wrapper_path)?;
  }
  if plan.worker_before_digest.is_some() {
    fs::remove_file(&receipt.worker_path)?;
  }
  if read_optional_regular(&receipt.session_memo_path()?, MAX_SESSION_MEMO_BYTES)?.is_some() {
    fs::remove_file(receipt.session_memo_path()?)?;
  }
  if read_optional_regular(&receipt.usage_path()?, MAX_USAGE_BYTES)?.is_some() {
    fs::remove_file(receipt.usage_path()?)?;
  }
  fs::remove_file(&receipt_path)?;
  let session_lock_path = receipt.session_lock_path()?;
  drop(session_lock);
  if read_optional_regular(&session_lock_path, 0)?.is_some() {
    fs::remove_file(session_lock_path)?;
  }
  match fs::remove_dir(install_directory) {
    Ok(()) => {}
    Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
    Err(error) => return Err(error.into()),
  }
  if let Some(owner) = install_directory.parent() {
    match fs::remove_dir(owner) {
      Ok(()) => {}
      Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
      Err(error) => return Err(error.into()),
    }
  }
  Ok(())
}

/// Load and validate the receipt adjacent to one installed wrapper process.
pub(crate) fn load_for_wrapper(invoked: &Path) -> RailResult<InstallationReceipt> {
  let invoked = absolute(invoked)?;
  let launcher = std::env::var_os(DIRECT_LAUNCHER_ENV)
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message("installed compiler worker has no launcher capability"))?;
  let launcher = absolute(&launcher)?;
  let directory = invoked
    .parent()
    .ok_or_else(|| RailError::message("installed compiler wrapper has no parent directory"))?;
  let bytes = read_required_regular(&directory.join(RECEIPT_FILE), MAX_RECEIPT_BYTES)?;
  let receipt = parse_receipt(&bytes)?;
  if crate::utils::canonicalize_existing(&receipt.worker_path)? != crate::utils::canonicalize_existing(&invoked)?
    || crate::utils::canonicalize_existing(&receipt.wrapper_path)? != crate::utils::canonicalize_existing(&launcher)?
  {
    return Err(RailError::message(
      "installed compiler launcher or worker does not match its setup authority",
    ));
  }
  if crate::utils::stable_file_generation(&invoked).as_ref() != Some(&receipt.worker_generation)
    || crate::utils::stable_file_generation(&launcher).as_ref() != Some(&receipt.wrapper_generation)
  {
    return Err(RailError::message(
      "installed compiler launcher or worker changed after verified setup",
    ));
  }
  Ok(receipt)
}

/// Inspect installation health without creating Cargo or cache state.
pub(crate) fn status(current_dir: &Path) -> RailResult<InstallationStatus> {
  let cargo_home = resolve_cargo_home(current_dir)?;
  let config_path = selected_user_config(&cargo_home);
  let receipt_path = cargo_home
    .join("cargo-rail")
    .join(INSTALLATION_DIRECTORY)
    .join(RECEIPT_FILE);
  let Some(bytes) = read_optional_regular(&receipt_path, MAX_RECEIPT_BYTES)? else {
    return Ok(InstallationStatus {
      state: "not_installed",
      healthy: true,
      cargo_home: cargo_home.to_string_lossy().into_owned(),
      config_path: config_path.to_string_lossy().into_owned(),
      wrapper_path: None,
      cache_base: None,
      max_bytes: None,
      cargo_l0: "owned_by_cargo_not_observable_when_rustc_is_not_launched",
      usage: InstallationUsageStatus {
        early_bypasses: "not_recorded_before_context_acquisition",
        ..InstallationUsageStatus::default()
      },
      issues: Vec::new(),
    });
  };
  let receipt = parse_receipt(&bytes)?;
  let mut issues = Vec::new();
  if receipt.cargo_home != cargo_home || receipt.config_path != config_path {
    issues.push("selected Cargo home or active user config changed".to_string());
  }
  match read_optional_regular(&config_path, 16 * 1024 * 1024) {
    Ok(Some(bytes)) => match std::str::from_utf8(&bytes)
      .map_err(|_| RailError::message("Cargo user configuration is not valid UTF-8"))
      .and_then(wrapper_value)
    {
      Ok(Some(value)) if Path::new(&value) == receipt.wrapper_path => {}
      Ok(_) => issues.push("Cargo build.rustc-wrapper no longer selects the installed wrapper".to_string()),
      Err(error) => issues.push(error.to_string()),
    },
    Ok(None) => issues.push("Cargo user configuration is missing".to_string()),
    Err(error) => issues.push(error.to_string()),
  }
  match file_digest(&receipt.wrapper_path) {
    Ok(digest) if digest == receipt.wrapper_digest => {}
    Ok(_) => issues.push("installed wrapper content changed".to_string()),
    Err(error) => issues.push(format!("installed wrapper is unavailable: {error}")),
  }
  match file_digest(&receipt.worker_path) {
    Ok(digest) if digest == receipt.worker_digest => {}
    Ok(_) => issues.push("installed worker content changed".to_string()),
    Err(error) => issues.push(format!("installed worker is unavailable: {error}")),
  }
  match receipt.cache.configured_root() {
    Ok(Some(_)) => {}
    Ok(None) => issues.push("local cache is unavailable; run cargo rail cache setup to repair it".to_string()),
    Err(error) => issues.push(format!("local cache authority is invalid: {error}")),
  }
  if let Err(error) = reject_shadowing_global_wrapper(current_dir, &config_path) {
    issues.push(error.to_string());
  }
  let usage = match usage_status(&receipt) {
    Ok(usage) => usage,
    Err(error) => {
      issues.push(error.to_string());
      InstallationUsageStatus {
        early_bypasses: "not_recorded_before_context_acquisition",
        ..InstallationUsageStatus::default()
      }
    }
  };
  Ok(InstallationStatus {
    state: if issues.is_empty() { "installed" } else { "drifted" },
    healthy: issues.is_empty(),
    cargo_home: cargo_home.to_string_lossy().into_owned(),
    config_path: config_path.to_string_lossy().into_owned(),
    wrapper_path: Some(receipt.wrapper_path.to_string_lossy().into_owned()),
    cache_base: Some(receipt.cache.base().to_string_lossy().into_owned()),
    max_bytes: Some(receipt.cache.max_bytes()),
    cargo_l0: "owned_by_cargo_not_observable_when_rustc_is_not_launched",
    usage,
    issues,
  })
}

/// Inspect the exact local CAS selected by a healthy installation receipt.
pub(crate) fn local_cache_status(current_dir: &Path) -> RailResult<Option<crate::cache::cas::LocalCasStatus>> {
  let cargo_home = resolve_cargo_home(current_dir)?;
  let receipt_path = cargo_home
    .join("cargo-rail")
    .join(INSTALLATION_DIRECTORY)
    .join(RECEIPT_FILE);
  let Some(bytes) = read_optional_regular(&receipt_path, MAX_RECEIPT_BYTES)? else {
    return Ok(None);
  };
  let receipt = parse_receipt(&bytes)?;
  let Some(root) = receipt.cache.configured_root()? else {
    return Ok(None);
  };
  crate::cache::cas::status_at_with_max(&root, receipt.cache.max_bytes())
}

/// Remove only the CAS selected by an installation receipt, when one exists.
/// `Some` means the receipt was authoritative even when its CAS was absent.
pub(crate) fn remove_local_cache(current_dir: &Path) -> RailResult<Option<Vec<(PathBuf, u64)>>> {
  let cargo_home = resolve_cargo_home(current_dir)?;
  let receipt_path = cargo_home
    .join("cargo-rail")
    .join(INSTALLATION_DIRECTORY)
    .join(RECEIPT_FILE);
  let Some(bytes) = read_optional_regular(&receipt_path, MAX_RECEIPT_BYTES)? else {
    return Ok(None);
  };
  let receipt = parse_receipt(&bytes)?;
  if receipt.cargo_home != cargo_home || receipt.config_path != selected_user_config(&cargo_home) {
    return Err(RailError::message(
      "transparent compiler-cache cleanup authority does not match the selected Cargo home",
    ));
  }
  let mut removed = Vec::new();
  if let Some(root) = receipt.cache.configured_root()?
    && let Some(entry) = crate::cache::cas::remove_owned_root_at(&root)?
  {
    removed.push(entry);
  }
  Ok(Some(removed))
}

fn resolve_cargo_home(current_dir: &Path) -> RailResult<PathBuf> {
  let selected = CargoConfigSnapshot::cargo_home(current_dir)?;
  resolve_requested_path(current_dir, &selected)
}

fn selected_user_config(cargo_home: &Path) -> PathBuf {
  let legacy = cargo_home.join("config");
  if legacy.is_file() {
    legacy
  } else {
    cargo_home.join("config.toml")
  }
}

fn reject_shadowing_global_wrapper(current_dir: &Path, user_config: &Path) -> RailResult<()> {
  for name in ["CARGO_BUILD_RUSTC_WRAPPER", "RUSTC_WRAPPER"] {
    if std::env::var_os(name).is_some() {
      return Err(RailError::with_help(
        format!("{name} shadows Cargo's user rustc-wrapper setting"),
        format!("unset {name} before installing transparent compiler reuse"),
      ));
    }
  }
  let captured = CargoConfigSnapshot::capture(current_dir)?;
  for source in captured.provenance() {
    if source.path() != user_config
      && source
        .settings()
        .get("build")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|build| build.contains_key("rustc-wrapper"))
    {
      return Err(RailError::with_help(
        format!(
          "Cargo configuration '{}' shadows the user rustc-wrapper setting",
          source.path().display()
        ),
        "remove the higher-precedence build.rustc-wrapper or keep Cargo-Rail uninstalled for this workspace",
      ));
    }
  }
  Ok(())
}

fn install_wrapper_value(
  original: &str,
  wrapper: &Path,
  existing: Option<&InstallationReceipt>,
) -> RailResult<(String, bool)> {
  let mut document = if original.is_empty() {
    DocumentMut::new()
  } else {
    original
      .parse::<DocumentMut>()
      .map_err(|error| RailError::message(format!("failed to parse Cargo user configuration: {error}")))?
  };
  let selected = wrapper_value_from_document(&document)?;
  if let Some(selected) = selected
    && existing.is_none_or(|receipt| Path::new(selected) != receipt.wrapper_path)
  {
    return Err(RailError::with_help(
      format!("Cargo user configuration already selects rustc wrapper '{selected}'"),
      "remove the existing global wrapper before installing Cargo-Rail",
    ));
  }
  let encoded = wrapper
    .to_str()
    .ok_or_else(|| RailError::message("installed compiler wrapper path is not valid UTF-8"))?;
  let build_table_created = document.get("build").is_none();
  match document.get_mut("build") {
    Some(Item::Table(table)) => {
      table.insert("rustc-wrapper", toml_edit::value(encoded));
    }
    Some(Item::Value(Value::InlineTable(table))) => {
      table.insert("rustc-wrapper", Value::from(encoded));
    }
    Some(_) => return Err(RailError::message("Cargo configuration key 'build' is not a table")),
    None => {
      let mut table = Table::new();
      table.insert("rustc-wrapper", toml_edit::value(encoded));
      document.insert("build", Item::Table(table));
    }
  }
  Ok((document.to_string(), build_table_created))
}

fn wrapper_value(contents: &str) -> RailResult<Option<String>> {
  let document = contents
    .parse::<DocumentMut>()
    .map_err(|error| RailError::message(format!("failed to parse Cargo user configuration: {error}")))?;
  wrapper_value_from_document(&document).map(|value| value.map(str::to_string))
}

fn wrapper_value_from_document(document: &DocumentMut) -> RailResult<Option<&str>> {
  let Some(build) = document.get("build") else {
    return Ok(None);
  };
  let value = match build {
    Item::Table(table) => table.get("rustc-wrapper").and_then(Item::as_str),
    Item::Value(Value::InlineTable(table)) => table.get("rustc-wrapper").and_then(Value::as_str),
    _ => return Err(RailError::message("Cargo configuration key 'build' is not a table")),
  };
  if value.is_none()
    && match build {
      Item::Table(table) => table.contains_key("rustc-wrapper"),
      Item::Value(Value::InlineTable(table)) => table.contains_key("rustc-wrapper"),
      _ => false,
    }
  {
    return Err(RailError::message(
      "Cargo configuration key 'build.rustc-wrapper' is not a string",
    ));
  }
  Ok(value)
}

fn remove_wrapper_value(contents: &str, receipt: &InstallationReceipt) -> RailResult<Option<Vec<u8>>> {
  let mut document = contents
    .parse::<DocumentMut>()
    .map_err(|error| RailError::message(format!("failed to parse Cargo user configuration: {error}")))?;
  if let Some(selected) = wrapper_value_from_document(&document)?
    && Path::new(selected) != receipt.wrapper_path
  {
    return Err(RailError::message(
      "Cargo build.rustc-wrapper changed after setup; removal refused",
    ));
  }
  if let Some(build) = document.get_mut("build") {
    match build {
      Item::Table(table) => {
        table.remove("rustc-wrapper");
      }
      Item::Value(Value::InlineTable(table)) => {
        table.remove("rustc-wrapper");
      }
      _ => return Err(RailError::message("Cargo configuration key 'build' is not a table")),
    }
  }
  if receipt.build_table_created
    && document.get("build").is_some_and(|build| match build {
      Item::Table(table) => table.is_empty(),
      Item::Value(Value::InlineTable(table)) => table.is_empty(),
      _ => false,
    })
  {
    document.remove("build");
  }
  let bytes = document.to_string().into_bytes();
  if receipt.config_created && bytes.iter().all(u8::is_ascii_whitespace) {
    Ok(None)
  } else {
    Ok(Some(bytes))
  }
}

fn resolve_requested_path(current_dir: &Path, path: &Path) -> RailResult<PathBuf> {
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    current_dir.join(path)
  };
  crate::utils::canonicalize_allow_missing(&path).map_err(Into::into)
}

fn create_private_directory(path: &Path) -> RailResult<()> {
  fs::create_dir_all(path)?;
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(format!(
      "private installation path '{}' is not a real directory",
      path.display()
    )));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

fn ensure_real_directory(path: &Path) -> RailResult<()> {
  fs::create_dir_all(path)?;
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(format!(
      "Cargo home '{}' is not a real directory",
      path.display()
    )));
  }
  Ok(())
}

fn install_executable_atomic(source: &Path, destination: &Path) -> RailResult<()> {
  let parent = destination
    .parent()
    .ok_or_else(|| RailError::message("installed wrapper has no parent"))?;
  let source_metadata = fs::symlink_metadata(source)?;
  if !source_metadata.is_file() || crate::utils::is_symlink_or_reparse(&source_metadata) {
    return Err(RailError::message("compiler wrapper source is not a real regular file"));
  }
  let mut source = File::open(source)?;
  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-wrapper-")
    .tempfile_in(parent)?;
  std::io::copy(&mut source, &mut temporary)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    temporary.as_file().set_permissions(fs::Permissions::from_mode(0o700))?;
  }
  temporary.as_file().sync_all()?;
  crate::utils::persist_file_atomic(temporary, destination)?;
  Ok(())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let parent = path
    .parent()
    .ok_or_else(|| RailError::message("private installation file has no parent"))?;
  let mut builder = tempfile::Builder::new();
  builder.prefix(".cargo-rail-install-");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    builder.permissions(fs::Permissions::from_mode(0o600));
  }
  let mut temporary = builder.tempfile_in(parent)?;
  temporary.write_all(bytes)?;
  temporary.as_file().sync_all()?;
  crate::utils::persist_file_atomic(temporary, path)?;
  Ok(())
}

fn read_optional_regular(path: &Path, max_bytes: u64) -> RailResult<Option<Vec<u8>>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => {
      if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > max_bytes {
        return Err(RailError::message(format!(
          "installation input '{}' is not a bounded regular file",
          path.display()
        )));
      }
      let file = File::open(path)?;
      if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(format!(
          "installation input '{}' changed before it was opened",
          path.display()
        )));
      }
      let mut bytes = Vec::with_capacity(metadata.len() as usize);
      file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
      if bytes.len() as u64 != metadata.len() {
        return Err(RailError::message(format!(
          "installation input '{}' changed while it was read",
          path.display()
        )));
      }
      Ok(Some(bytes))
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

fn read_required_regular(path: &Path, max_bytes: u64) -> RailResult<Vec<u8>> {
  read_optional_regular(path, max_bytes)?
    .ok_or_else(|| RailError::message(format!("required installation file '{}' is missing", path.display())))
}

fn revalidate_optional(path: &Path, expected: Option<&[u8]>, max_bytes: u64) -> RailResult<()> {
  if read_optional_regular(path, max_bytes)?.as_deref() == expected {
    Ok(())
  } else {
    Err(RailError::with_help(
      format!("installation input '{}' changed after planning", path.display()),
      "rerun the command to build a new exact mutation plan",
    ))
  }
}

fn parse_receipt(bytes: &[u8]) -> RailResult<InstallationReceipt> {
  let receipt: InstallationReceipt = serde_json::from_slice(bytes)?;
  receipt.validate()?;
  if encode_receipt(&receipt)? != bytes {
    return Err(RailError::message(
      "transparent compiler-cache installation receipt is not canonical",
    ));
  }
  Ok(receipt)
}

fn encode_receipt(receipt: &InstallationReceipt) -> RailResult<Vec<u8>> {
  let mut bytes = serde_json::to_vec_pretty(receipt)?;
  bytes.push(b'\n');
  if bytes.len() as u64 > MAX_RECEIPT_BYTES {
    return Err(RailError::message(
      "transparent compiler-cache receipt exceeds its bound",
    ));
  }
  Ok(bytes)
}

fn file_digest(path: &Path) -> RailResult<String> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(format!(
      "'{}' is not a real regular file",
      path.display()
    )));
  }
  let mut file = File::open(path)?;
  if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
    return Err(RailError::message(format!(
      "'{}' changed before it was opened",
      path.display()
    )));
  }
  let mut hasher = Sha256::new();
  let mut buffer = [0_u8; 64 * 1024];
  let mut read = 0_u64;
  loop {
    let count = file.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    read = read
      .checked_add(count as u64)
      .ok_or_else(|| RailError::message("compiler wrapper size overflow"))?;
    hasher.update(&buffer[..count]);
  }
  if read != metadata.len() || !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
    return Err(RailError::message(format!(
      "'{}' changed while it was read",
      path.display()
    )));
  }
  Ok(format!(
    "sha256:{}",
    ContentDigest::from_sha256_bytes(hasher.finalize().into())
  ))
}

fn optional_file_digest(path: &Path) -> RailResult<Option<String>> {
  match fs::symlink_metadata(path) {
    Ok(_) => file_digest(path).map(Some),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

fn absolute(path: &Path) -> RailResult<PathBuf> {
  if path.is_absolute() {
    Ok(path.to_path_buf())
  } else {
    Ok(std::env::current_dir()?.join(path))
  }
}

fn random_authority() -> RailResult<String> {
  let mut bytes = [0_u8; 32];
  getrandom::fill(&mut bytes)
    .map_err(|error| RailError::message(format!("failed to acquire installation authority randomness: {error}")))?;
  Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn valid_hex_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
  value.strip_prefix("sha256:").is_some_and(valid_hex_digest)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lossless_wrapper_edit_preserves_existing_cargo_configuration() {
    let original = "# retained\n[net]\noffline = true\n\n[build] # retained table\ntarget-dir = 'out'\n";
    let wrapper = Path::new("/tools/cargo-rail-native-rustc-wrapper");
    let (edited, created) = install_wrapper_value(original, wrapper, None).expect("valid edit");
    assert!(!created);
    assert_eq!(edited, format!("{original}rustc-wrapper = \"{}\"\n", wrapper.display()));
  }

  #[test]
  fn setup_refuses_to_adopt_an_unowned_wrapper() {
    let error = install_wrapper_value("[build]\nrustc-wrapper = 'sccache'\n", Path::new("/tools/rail"), None)
      .expect_err("existing wrapper must conflict");
    assert!(error.to_string().contains("already selects rustc wrapper"));
  }

  #[test]
  fn mutation_revalidation_rejects_byte_drift() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, b"before\n").expect("write original");
    fs::write(&path, b"after\n").expect("write drift");
    let error = revalidate_optional(&path, Some(b"before\n"), 1024).expect_err("drift must fail");
    assert!(error.to_string().contains("changed after planning"));
  }
}
