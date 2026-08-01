//! Explicit isolated execution and reproducibility proof for Rust actions.

pub(crate) mod cas;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufReader, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cargo_metadata::{Message, Metadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::action::{ActionKind, ActionWorkingDirectory, ExpandedAction};
#[cfg(target_os = "macos")]
use crate::action_key::{DeclaredInputEntry, DeclaredInputKind};
use crate::action_key::{DeclaredInputManifest, declared_input_manifest};
use crate::cargo::resolution::ResolutionInputs;
use crate::cargo::{CargoConfigSnapshot, ToolchainIdentity};
use crate::compiler::observation::{FileObservation, ObservationPath, RawCompilerInvocation, load_raw};
use crate::compiler::wrapper::{
  CACHE_WRAPPER_MARKER, OBSERVATION_DIRECTORY_ENV, OBSERVATION_ONLY_ENV, OBSERVATION_SOURCE_ROOT_ENV, WRAPPER_MARKER,
};
use crate::error::{RailError, RailResult};
use crate::executable::{ExecutableIdentity, ToolchainExecutableScope};
use crate::git::SystemGit;
use crate::source::{ContentDigest, SourceEntryKind};
#[cfg(target_os = "macos")]
use crate::source::{GitWorktreeCapture, SourceSnapshot};
use crate::workspace::snapshot::CapturedLockfile;
use crate::workspace::{WorkspaceContext, WorkspaceSnapshot};

const HERMETIC_PROFILE_VERSION: u32 = 1;
#[cfg(target_os = "macos")]
const MACOS_SANDBOX_POLICY_VERSION: u32 = 1;
const FETCH_ACTION_VERSION: u32 = 1;
const OUTPUT_MANIFEST_VERSION: u32 = 1;
const RESULT_VERSION: u32 = 3;
const FAST_CACHE_VALIDATION_VERSION: u32 = 1;
const MAX_OUTPUT_ENTRIES: usize = 1_000_000;
const MAX_DEP_INFO_BYTES: u64 = 64 * 1024 * 1024;
const IO_BUFFER_BYTES: usize = 64 * 1024;
const CARGO_MUTABLE_CACHE_FILES: &[&str] = &[".global-cache", ".package-cache", ".package-cache-mutate"];
const FETCH_INVENTORY_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MATERIALIZATIONS_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const REPORTS_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORTS: usize = 128;

/// Maximum retained bytes under `target/cargo-rail/hermetic` after an operation.
pub(crate) const HERMETIC_WORKSPACE_MAX_BYTES: u64 =
  FETCH_INVENTORY_MAX_BYTES + MATERIALIZATIONS_MAX_BYTES + REPORTS_MAX_BYTES;

#[cfg(target_os = "macos")]
const MACOS_REQUIRED_HOST_FILES: &[&str] = &["/private/etc/ssl/openssl.cnf"];

fn create_state_directory(workspace_root: &Path, suffix: &[&str]) -> RailResult<PathBuf> {
  let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  let mut directory = workspace_root.clone();
  for component in ["target", "cargo-rail", "hermetic"]
    .into_iter()
    .chain(suffix.iter().copied())
  {
    directory.push(component);
    match fs::create_dir(&directory) {
      Ok(()) => {}
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
      Err(error) => {
        return Err(RailError::message(format!(
          "failed to create hermetic state directory '{}': {error}",
          directory.display()
        )));
      }
    }
    validate_real_state_directory(&directory)?;
  }
  let canonical = crate::utils::canonicalize_existing(&directory)?;
  if !canonical.starts_with(&workspace_root) {
    return Err(RailError::message(format!(
      "hermetic state directory '{}' escapes workspace '{}'",
      directory.display(),
      workspace_root.display()
    )));
  }
  Ok(canonical)
}

fn existing_state_directory(workspace_root: &Path, suffix: &[&str]) -> RailResult<Option<PathBuf>> {
  let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  let mut directory = workspace_root.clone();
  for component in ["target", "cargo-rail", "hermetic"]
    .into_iter()
    .chain(suffix.iter().copied())
  {
    directory.push(component);
    match fs::symlink_metadata(&directory) {
      Ok(_) => validate_real_state_directory(&directory)?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error.into()),
    }
  }
  let canonical = crate::utils::canonicalize_existing(&directory)?;
  if !canonical.starts_with(&workspace_root) {
    return Err(RailError::message(format!(
      "hermetic state directory '{}' escapes workspace '{}'",
      directory.display(),
      workspace_root.display()
    )));
  }
  Ok(Some(canonical))
}

fn validate_real_state_directory(path: &Path) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  if metadata.is_dir() && !metadata.file_type().is_symlink() {
    return Ok(());
  }
  Err(RailError::with_help(
    format!("hermetic state path '{}' is not a real directory", path.display()),
    "remove the hostile or corrupt path before retrying; cargo-rail will not follow state symlinks",
  ))
}

fn reclaim_workspace_state(workspace_root: &Path) -> RailResult<()> {
  if let Some(root) = existing_state_directory(workspace_root, &[])? {
    let obsolete_reference = root.join("local-cas-v1.json");
    match fs::symlink_metadata(&obsolete_reference) {
      Ok(_) => remove_owned_path(&obsolete_reference)?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
  }
  if let Some(runs) = existing_state_directory(workspace_root, &["runs"])? {
    remove_children(&runs, None)?;
  }
  if let Some(materializations) = existing_state_directory(workspace_root, &["materializations"])? {
    prune_children(&materializations, None, MATERIALIZATIONS_MAX_BYTES, usize::MAX)?;
  }
  if let Some(reports) = existing_state_directory(workspace_root, &["reports"])? {
    prune_children(&reports, None, REPORTS_MAX_BYTES, MAX_REPORTS)?;
  }
  Ok(())
}

fn retain_fetch_inventory(root: &Path, retained: &Path) -> RailResult<()> {
  remove_children(root, Some(retained))?;
  if retained.exists() {
    ensure_tree_bytes(retained, FETCH_INVENTORY_MAX_BYTES, "hermetic fetch inventory")?;
  }
  Ok(())
}

fn prune_children(root: &Path, retained: Option<&Path>, max_bytes: u64, max_entries: usize) -> RailResult<()> {
  let mut entries = fs::read_dir(root)?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|entry| {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
      let bytes = if crate::utils::is_symlink_or_reparse(&metadata) {
        metadata.len()
      } else {
        crate::cache::path_status(&path)?.map_or(0, |status| status.0)
      };
      Ok((modified, entry.file_name(), path, bytes))
    })
    .collect::<RailResult<Vec<_>>>()?;
  entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
  let mut bytes = entries.iter().try_fold(0u64, |total, entry| {
    total
      .checked_add(entry.3)
      .ok_or_else(|| RailError::message("workspace hermetic state size overflow"))
  })?;
  let mut count = entries.len();
  for (_, _, path, entry_bytes) in entries {
    if bytes <= max_bytes && count <= max_entries {
      break;
    }
    if retained.is_some_and(|retained| path == retained) {
      continue;
    }
    remove_owned_path(&path)?;
    bytes = bytes.saturating_sub(entry_bytes);
    count = count.saturating_sub(1);
  }
  if bytes > max_bytes || count > max_entries {
    return Err(RailError::message(format!(
      "retained workspace hermetic state exceeds its {max_bytes}-byte/{max_entries}-entry bound"
    )));
  }
  Ok(())
}

fn remove_children(root: &Path, retained: Option<&Path>) -> RailResult<()> {
  let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let path = entry.path();
    if retained.is_some_and(|retained| path == retained) {
      continue;
    }
    remove_owned_path(&path)?;
  }
  Ok(())
}

fn remove_owned_path(path: &Path) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) {
    set_tree_owner_writable(path)?;
    fs::remove_dir_all(path)?;
  } else if metadata.is_dir() {
    fs::remove_dir(path)?;
  } else {
    fs::remove_file(path)?;
  }
  Ok(())
}

fn ensure_tree_bytes(root: &Path, max_bytes: u64, description: &str) -> RailResult<u64> {
  let bytes = crate::cache::path_status(root)?.map_or(0, |status| status.0);
  if bytes > max_bytes {
    return Err(RailError::with_help(
      format!("{description} '{}' exceeds its {max_bytes}-byte bound", root.display()),
      "reduce the retained inputs or outputs; cargo-rail will not keep unbounded workspace cache state",
    ));
  }
  Ok(bytes)
}

pub(crate) fn remove_state(workspace_root: &Path) -> RailResult<()> {
  let Some(root) = existing_state_directory(workspace_root, &[])? else {
    return Ok(());
  };
  set_tree_owner_writable(&root)?;
  fs::remove_dir_all(&root)
    .map_err(|error| RailError::message(format!("failed to remove hermetic cache '{}': {error}", root.display())))
}

pub(crate) fn remove_local_cache() -> RailResult<Option<(PathBuf, u64)>> {
  let Some(root) = cas::configured_root()? else {
    return Ok(None);
  };
  cas::remove_owned_root_at(&root)
}

pub(crate) fn local_cache_status() -> RailResult<Option<cas::LocalCasStatus>> {
  let Some(root) = cas::configured_root()? else {
    return Ok(None);
  };
  cas::status_at(&root)
}

/// Support earned by one concrete action execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HermeticSupport {
  Eligible,
  PlatformLimited,
  Uncacheable,
}

impl HermeticSupport {
  pub(crate) const fn as_str(self) -> &'static str {
    match self {
      Self::Eligible => "eligible",
      Self::PlatformLimited => "platform_limited",
      Self::Uncacheable => "uncacheable",
    }
  }
}

/// Operating-system boundary applied around a hermetic action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnforcementLevel {
  FilesystemAndNetwork,
  CargoOfflineOnly,
}

/// Redaction-safe result of one explicit hermetic execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HermeticExecutionReport {
  version: u32,
  profile_version: u32,
  action_id: String,
  action_class: String,
  support: HermeticSupport,
  enforcement: EnforcementLevel,
  #[serde(skip_serializing_if = "Option::is_none")]
  action_key: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  result_digest: Option<String>,
  fetch: FetchSummary,
  #[serde(skip_serializing_if = "Option::is_none")]
  output_manifest: Option<OutputManifest>,
  #[serde(skip_serializing_if = "Option::is_none")]
  materialized_root: Option<String>,
  cache: LocalCacheSummary,
  compiler_units: usize,
  elapsed_ms: u128,
  reasons: BTreeSet<String>,
}

/// A process-free local action-cache probe either restored a result or identified a safe miss.
pub(crate) enum PreContextCacheAttempt {
  #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
  Hit(Box<PreContextCacheHit>),
  Miss(String),
}

/// Data needed to preserve the ordinary `run` output and decision receipt after an early hit.
pub(crate) struct PreContextCacheHit {
  pub(crate) report: HermeticExecutionReport,
  pub(crate) report_path: PathBuf,
  pub(crate) selected_packages: Vec<String>,
  pub(crate) argv: Vec<String>,
}

impl HermeticExecutionReport {
  pub(crate) fn support(&self) -> HermeticSupport {
    self.support
  }

  pub(crate) fn action_key(&self) -> Option<&str> {
    self.action_key.as_deref()
  }

  pub(crate) fn result_digest(&self) -> Option<&str> {
    self.result_digest.as_deref()
  }

  pub(crate) fn cache_status(&self) -> &'static str {
    self.cache.status.as_str()
  }

  pub(crate) fn cache_reason(&self) -> &str {
    &self.cache.reason
  }

  pub(crate) fn cargo_check_executed(&self) -> bool {
    self.cache.cargo_check_executed
  }

  pub(crate) fn compiler_units_executed(&self) -> bool {
    self.cache.compiler_units_executed
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalCacheStatus {
  #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
  Hit,
  Miss,
  Disabled,
  Uncacheable,
}

impl LocalCacheStatus {
  const fn as_str(self) -> &'static str {
    match self {
      Self::Hit => "hit",
      Self::Miss => "miss",
      Self::Disabled => "disabled",
      Self::Uncacheable => "uncacheable",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LocalCacheSummary {
  version: u32,
  status: LocalCacheStatus,
  reason: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  action_result: Option<String>,
  objects_verified: u64,
  bytes_read: u64,
  bytes_restored: u64,
  objects_written: u64,
  bytes_written: u64,
  stored: bool,
  cargo_check_executed: bool,
  compiler_units_executed: bool,
}

impl LocalCacheSummary {
  fn new(status: LocalCacheStatus, reason: impl Into<String>) -> Self {
    Self {
      version: 1,
      status,
      reason: reason.into(),
      action_result: None,
      objects_verified: 0,
      bytes_read: 0,
      bytes_restored: 0,
      objects_written: 0,
      bytes_written: 0,
      stored: false,
      cargo_check_executed: false,
      compiler_units_executed: false,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FetchSummary {
  version: u32,
  key: String,
  result_digest: String,
  packages: usize,
  source_entries: usize,
  inventory_entries: usize,
  credential_capabilities: BTreeSet<String>,
  reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FetchManifest {
  version: u32,
  key: String,
  result_digest: String,
  cargo_home_digest: String,
  cargo_home_entries: usize,
  packages: Vec<InventoryPackage>,
  credential_capabilities: BTreeSet<String>,
}

/// Non-authorizing exact observations used only to find a P6 action key before Cargo starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FastCacheValidation {
  version: u32,
  lookup_key: String,
  action_key: String,
  workspace_prefix: String,
  cargo_current_directory: String,
  declared_inputs: DeclaredInputManifest,
  cargo_config_identity: String,
  cargo_environment_identity: String,
  toolchain_selector_identity: String,
  rustc_sysroot: String,
  host_target: String,
  platform_identity: String,
  inventory_result_digest: String,
  inventory_packages: Vec<InventoryPackage>,
  fetch: FetchSummary,
  selected_packages: Vec<String>,
  argv: Vec<String>,
}

impl FastCacheValidation {
  fn validate_object(&self) -> RailResult<()> {
    if self.version != FAST_CACHE_VALIDATION_VERSION {
      return Err(RailError::message(
        "local cache validation manifest has an incompatible schema",
      ));
    }
    cas::validate_fast_identity(&self.action_key, &self.lookup_key)?;
    self.declared_inputs.validate()?;
    validate_relative_prefix(&self.workspace_prefix, true)?;
    validate_relative_prefix(&self.cargo_current_directory, true)?;
    let sysroot = Path::new(&self.rustc_sysroot);
    if !sysroot.is_absolute() {
      return Err(RailError::message(
        "local cache validation manifest has an invalid toolchain sysroot boundary",
      ));
    }
    if self.argv != ["cargo", "check", "--workspace"]
      || self.selected_packages.is_empty()
      || self.selected_packages.windows(2).any(|pair| pair[0] >= pair[1])
    {
      return Err(RailError::message(
        "local cache validation manifest is outside the graduated action request",
      ));
    }
    for digest in [
      &self.cargo_config_identity,
      &self.cargo_environment_identity,
      &self.toolchain_selector_identity,
    ] {
      validate_sha256_identity(digest)?;
    }
    if !canonical_identity(&self.platform_identity, "platform-v1-sha256-")
      || self
        .host_target
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_whitespace())
      || self
        .selected_packages
        .iter()
        .any(|package| package.is_empty() || package.bytes().any(|byte| byte == 0 || byte.is_ascii_control()))
    {
      return Err(RailError::message(
        "local cache validation manifest has a malformed platform or package identity",
      ));
    }
    for package in &self.inventory_packages {
      if package.identity.is_empty()
        || package
          .identity
          .bytes()
          .any(|byte| byte == 0 || byte.is_ascii_control())
        || package.source_entries == 0
        || validate_sha256_identity(&package.source_tree_digest).is_err()
        || package.checksum.as_ref().is_some_and(|checksum| {
          checksum.len() != 64
            || !checksum
              .bytes()
              .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
      {
        return Err(RailError::message(
          "local cache validation manifest has malformed dependency-result evidence",
        ));
      }
    }
    if self.inventory_packages.windows(2).any(|pair| pair[0] >= pair[1]) {
      return Err(RailError::message(
        "local cache validation dependency results are not strictly sorted",
      ));
    }
    if self.rustc_sysroot.is_empty()
      || self.host_target.is_empty()
      || self.platform_identity.is_empty()
      || self.inventory_result_digest.is_empty()
      || self.argv.is_empty()
    {
      return Err(RailError::message("local cache validation manifest is incomplete"));
    }
    if self.fetch.version != FETCH_ACTION_VERSION
      || self.fetch.result_digest != self.inventory_result_digest
      || self.fetch.packages != self.inventory_packages.len()
      || self.fetch.source_entries
        != self
          .inventory_packages
          .iter()
          .map(|package| package.source_entries)
          .sum::<usize>()
      || !canonical_identity(&self.fetch.key, "fetch-v1-sha256-")
      || !canonical_identity(&self.inventory_result_digest, "fetch-result-v1-sha256-")
      || fetch_result_digest(&self.inventory_packages)? != self.inventory_result_digest
      || self.fetch.reused
    {
      return Err(RailError::message(
        "local cache validation manifest has an invalid fetch-result binding",
      ));
    }
    Ok(())
  }

  #[cfg(test)]
  fn fixture(action_key: &str, lookup_key: &str) -> Self {
    let digest = format!("sha256-{}", "0".repeat(64));
    let inventory_result_digest = fetch_result_digest(&[]).expect("empty dependency-result identity");
    Self {
      version: FAST_CACHE_VALIDATION_VERSION,
      lookup_key: lookup_key.to_string(),
      action_key: action_key.to_string(),
      workspace_prefix: String::new(),
      cargo_current_directory: String::new(),
      declared_inputs: DeclaredInputManifest::empty_for_test(),
      cargo_config_identity: format!("sha256:{}", "0".repeat(64)),
      cargo_environment_identity: format!("sha256:{}", "0".repeat(64)),
      toolchain_selector_identity: format!("sha256:{}", "0".repeat(64)),
      rustc_sysroot: Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-toolchain")
        .to_string_lossy()
        .into_owned(),
      host_target: "test-target".to_string(),
      platform_identity: format!("platform-v1-{digest}"),
      inventory_result_digest: inventory_result_digest.clone(),
      inventory_packages: Vec::new(),
      fetch: FetchSummary {
        version: FETCH_ACTION_VERSION,
        key: format!("fetch-v1-{digest}"),
        result_digest: inventory_result_digest,
        packages: 0,
        source_entries: 0,
        inventory_entries: 0,
        credential_capabilities: BTreeSet::new(),
        reused: false,
      },
      selected_packages: vec!["fixture".to_string()],
      argv: vec!["cargo".to_string(), "check".to_string(), "--workspace".to_string()],
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct InventoryPackage {
  identity: String,
  checksum: Option<String>,
  source_tree_digest: String,
  source_entries: usize,
}

#[derive(Clone)]
pub(crate) struct FetchInventory {
  cargo_home: PathBuf,
  cargo_home_digest: String,
  cargo_home_entries: usize,
  packages: Vec<InventoryPackage>,
  summary: FetchSummary,
}

#[derive(Clone)]
struct LockedFetchPackage {
  name: String,
  version: String,
  source: Option<String>,
  checksum: Option<String>,
}

struct FetchInputs {
  source_root: PathBuf,
  workspace_root: PathBuf,
  cargo_current_dir: PathBuf,
  lock_fingerprint: String,
  locked_packages: Vec<LockedFetchPackage>,
  cargo_config: Arc<CargoConfigSnapshot>,
  toolchain: ToolchainIdentity,
  cargo_implementation: ExecutableIdentity,
}

pub(crate) struct HermeticBootstrap {
  pub(crate) metadata: Metadata,
  pub(crate) resolution_inputs: ResolutionInputs,
  pub(crate) cargo_current_dir: PathBuf,
  pub(crate) inventory: FetchInventory,
  pub(crate) lockfile: CapturedLockfile,
  pub(crate) workspace_cache_lock: crate::cache::WorkspaceCacheLock,
}

pub(crate) fn prepare_bootstrap(workspace_root: &Path) -> RailResult<HermeticBootstrap> {
  let requested_workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  let source_root = match crate::git::SystemGit::open(&requested_workspace_root) {
    Ok(git) => git.worktree_root,
    Err(RailError::Git(crate::error::GitError::RepoNotFound { .. })) => requested_workspace_root.clone(),
    Err(error) => return Err(error),
  };
  let process_current_dir = std::env::current_dir()
    .map_err(|error| RailError::message(format!("failed to determine hermetic Cargo current directory: {error}")))?;
  let process_current_dir = crate::utils::canonicalize_existing(&process_current_dir)?;
  let cargo_current_dir = if process_current_dir.starts_with(&source_root) {
    process_current_dir
  } else {
    requested_workspace_root.clone()
  };
  let cargo_config = Arc::new(CargoConfigSnapshot::capture(&cargo_current_dir)?);
  let mut configuration_reasons = BTreeSet::new();
  classify_cargo_configuration(&cargo_config, &mut configuration_reasons);
  for name in ["RUSTC", "RUSTDOC"] {
    if std::env::var_os(name).is_some() {
      configuration_reasons.insert("cargo_tool_override_not_graduated".to_string());
    }
  }
  ensure_execution_semantics_supported(&configuration_reasons)?;
  let resolution_inputs = ResolutionInputs::from_hermetic_config(&cargo_current_dir, cargo_config)?;
  let local_metadata = bootstrap_local_metadata(&requested_workspace_root, &cargo_current_dir, &resolution_inputs)?;
  ensure_local_units_can_reach_hermetic_fetch(&local_metadata)?;
  let workspace_root = crate::utils::canonicalize_existing(local_metadata.workspace_root.as_std_path())?;
  if !workspace_root.starts_with(&source_root) {
    return Err(RailError::with_help(
      format!(
        "Cargo workspace root '{}' is outside hermetic source root '{}'",
        workspace_root.display(),
        source_root.display()
      ),
      "select a Cargo workspace contained by the repository or declared source boundary",
    ));
  }
  let workspace_cache_lock = crate::cache::lock_workspace(&workspace_root)?;
  reclaim_workspace_state(&workspace_root)?;
  let lock_path = workspace_root.join("Cargo.lock");
  let (lock_fingerprint, locked_packages, lockfile_bytes) = capture_fetch_lockfile(&lock_path)?;
  let executables = crate::executable::ToolchainExecutableIdentities::capture(
    &resolution_inputs.toolchain,
    &cargo_current_dir,
    &source_root,
    ToolchainExecutableScope::Compilation,
  )?;
  let cargo_implementation = executables
    .cargo_implementation()
    .cloned()
    .ok_or_else(|| RailError::message("selected Rust toolchain has no exact Cargo implementation identity"))?;
  let inputs = FetchInputs {
    source_root,
    workspace_root,
    cargo_current_dir,
    lock_fingerprint,
    locked_packages,
    cargo_config: Arc::clone(&resolution_inputs.cargo_config),
    toolchain: resolution_inputs.toolchain.clone(),
    cargo_implementation,
  };
  let inventory = prepare_fetch_inventory_with_inputs(&inputs)?;
  let metadata = isolated_metadata(&inputs, &inventory.cargo_home)?;
  inventory.validate_unchanged()?;
  Ok(HermeticBootstrap {
    metadata,
    resolution_inputs,
    cargo_current_dir: inputs.cargo_current_dir,
    inventory,
    lockfile: CapturedLockfile::new(lock_path, lockfile_bytes),
    workspace_cache_lock,
  })
}

fn bootstrap_local_metadata(
  workspace_root: &Path,
  cargo_current_dir: &Path,
  inputs: &ResolutionInputs,
) -> RailResult<Metadata> {
  crate::instrumentation::record_cargo_metadata_load(false);
  let mut command = cargo_metadata::MetadataCommand::new();
  command
    .cargo_path(toolchain_program(inputs.toolchain.rustc_sysroot(), "cargo"))
    .current_dir(cargo_current_dir)
    .manifest_path(workspace_root.join("Cargo.toml"))
    .no_deps()
    .other_options(vec!["--locked".to_string(), "--offline".to_string()])
    .env("RUSTC", toolchain_program(inputs.toolchain.rustc_sysroot(), "rustc"))
    .env(
      "RUSTDOC",
      toolchain_program(inputs.toolchain.rustc_sysroot(), "rustdoc"),
    )
    .env("CARGO_NET_OFFLINE", "true");
  for (name, value) in inputs.cargo_config.non_secret_acquisition_environment() {
    command.env(name, value);
  }
  command.exec().map_err(|error| {
    RailError::with_help(
      format!("hermetic preflight could not inspect the locked local workspace: {error}"),
      "commit an exact Cargo.lock and fix local manifest/configuration errors before hermetic execution",
    )
  })
}

fn ensure_local_units_can_reach_hermetic_fetch(metadata: &Metadata) -> RailResult<()> {
  let workspace_members = metadata.workspace_members.iter().collect::<BTreeSet<_>>();
  let has_build_script = metadata.packages.iter().any(|package| {
    workspace_members.contains(&package.id)
      && package.targets.iter().any(|target| {
        target
          .kind
          .iter()
          .any(|kind| kind == &cargo_metadata::TargetKind::CustomBuild)
      })
  });
  let has_proc_macro = metadata.packages.iter().any(|package| {
    workspace_members.contains(&package.id)
      && package.targets.iter().any(|target| {
        target
          .kind
          .iter()
          .any(|kind| kind == &cargo_metadata::TargetKind::ProcMacro)
      })
  });
  let mut reasons = Vec::new();
  if has_build_script {
    reasons.push("build_scripts_not_hermetic");
  }
  if has_proc_macro {
    reasons.push("proc_macros_not_hermetic");
  }
  if reasons.is_empty() {
    return Ok(());
  }
  Err(RailError::with_help(
    format!("hermetic action is explicitly uncacheable: {}", reasons.join(", ")),
    "run without --hermetic; normal Cargo execution remains available for these units",
  ))
}

fn capture_fetch_lockfile(path: &Path) -> RailResult<(String, Vec<LockedFetchPackage>, Vec<u8>)> {
  #[derive(Deserialize)]
  struct Document {
    #[serde(default)]
    package: Vec<Package>,
  }
  #[derive(Deserialize)]
  struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
  }

  let before = fs::symlink_metadata(path).map_err(|error| {
    RailError::with_help(
      format!(
        "hermetic execution requires exact Cargo.lock '{}': {error}",
        path.display()
      ),
      "generate and commit Cargo.lock before running the hermetic profile",
    )
  })?;
  if !before.is_file() || before.file_type().is_symlink() {
    return Err(RailError::with_help(
      format!("hermetic Cargo.lock '{}' is not a regular file", path.display()),
      "replace it with an exact regular lockfile before retrying",
    ));
  }
  let bytes = fs::read(path)?;
  let after = fs::symlink_metadata(path)?;
  ensure_exact_capture_metadata(path, &before, &after)?;
  let text = std::str::from_utf8(&bytes)
    .map_err(|_| RailError::message(format!("Cargo.lock '{}' is not valid UTF-8", path.display())))?;
  let document: Document = toml_edit::de::from_str(text).map_err(|_| {
    RailError::with_help(
      format!("failed to parse Cargo.lock '{}'", path.display()),
      "fix Cargo.lock syntax; raw parser context is suppressed because source URLs may contain credentials",
    )
  })?;
  let mut packages = document
    .package
    .into_iter()
    .map(|package| LockedFetchPackage {
      name: package.name,
      version: package.version,
      source: package.source,
      checksum: package.checksum,
    })
    .collect::<Vec<_>>();
  packages.sort_by(|left, right| {
    (&left.name, &left.version, &left.source).cmp(&(&right.name, &right.version, &right.source))
  });
  if let Some(duplicate) = packages
    .windows(2)
    .find(|pair| pair[0].name == pair[1].name && pair[0].version == pair[1].version && pair[0].source == pair[1].source)
  {
    return Err(RailError::message(format!(
      "Cargo.lock contains duplicate package identity '{} {}'",
      duplicate[0].name, duplicate[0].version
    )));
  }
  if let Some(package) = packages.iter().find(|package| {
    package
      .source
      .as_deref()
      .is_some_and(crate::cargo::resolution::credential_bearing_url)
  }) {
    return Err(RailError::with_help(
      format!(
        "Cargo.lock package '{} {}' contains a credential-bearing source URL",
        package.name, package.version
      ),
      "move credentials to Cargo's credential provider and regenerate the lockfile",
    ));
  }
  let digest = ContentDigest::sha256(&bytes);
  Ok((format!("sha256:{digest}"), packages, bytes))
}

impl FetchInventory {
  pub(crate) fn cargo_home(&self) -> &Path {
    &self.cargo_home
  }

  fn validate_unchanged(&self) -> RailResult<()> {
    let ignored = cargo_mutable_cache_paths(&self.cargo_home);
    let captured = capture_exact_tree(&self.cargo_home, &ignored)?;
    if captured.digest != self.cargo_home_digest || captured.entries != self.cargo_home_entries {
      return Err(RailError::with_help(
        "immutable Cargo fetch inventory changed during hermetic execution",
        "remove this workspace's hermetic cache with `cargo rail cache clean --scope workspace` and retry",
      ));
    }
    Ok(())
  }
}

#[derive(Debug)]
struct ExactTreeCapture {
  digest: String,
  entries: usize,
}

#[derive(Debug)]
enum ExactTreeEntryKind {
  File { digest: ContentDigest, executable: bool },
  Symlink { target: String },
}

#[derive(Debug)]
struct ExactTreeEntry {
  path: crate::source::RepositoryPath,
  kind: ExactTreeEntryKind,
}

/// Exact declared compiler-output tree for one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputManifest {
  version: u32,
  digest: String,
  entries: Vec<OutputEntry>,
  files: usize,
  directories: usize,
  symlinks: usize,
  bytes: u64,
}

impl OutputManifest {
  pub(crate) fn digest(&self) -> &str {
    &self.digest
  }

  fn validate_unchanged(&self, root: &Path) -> RailResult<()> {
    for entry in &self.entries {
      let relative = crate::source::RepositoryPath::new(Path::new(&entry.path))?;
      let path = root.join(relative.as_path());
      let metadata = fs::symlink_metadata(&path).map_err(|error| {
        RailError::message(format!(
          "declared compiler output '{}' disappeared before publication: {error}",
          entry.path
        ))
      })?;
      let unchanged = match &entry.kind {
        OutputEntryKind::Directory { mode } => {
          metadata.is_dir() && !metadata.file_type().is_symlink() && output_mode(&metadata) == *mode
        }
        OutputEntryKind::File { digest, mode, bytes } => {
          if !metadata.is_file() || metadata.file_type().is_symlink() || output_mode(&metadata) != *mode {
            false
          } else {
            let (current, current_bytes, _) = digest_and_scan_file(&path, &[])?;
            current_bytes == *bytes && format!("sha256:{current}") == *digest
          }
        }
        OutputEntryKind::Symlink { target } => {
          metadata.file_type().is_symlink()
            && fs::read_link(&path)
              .ok()
              .and_then(|current| current.to_str().map(str::to_string))
              .is_some_and(|current| current == *target)
        }
      };
      if !unchanged {
        return Err(RailError::with_help(
          format!("declared compiler output '{}' changed before publication", entry.path),
          "retry after concurrent writes to the isolated execution root have stopped",
        ));
      }
    }
    Ok(())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct OutputEntry {
  path: String,
  #[serde(flatten)]
  kind: OutputEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OutputEntryKind {
  Directory { mode: u32 },
  File { digest: String, mode: u32, bytes: u64 },
  Symlink { target: String },
}

struct RunLayout {
  _directory: tempfile::TempDir,
  root: PathBuf,
  workspace: PathBuf,
  target: PathBuf,
  build: PathBuf,
  temporary: PathBuf,
  home: PathBuf,
  cargo_home: PathBuf,
  observations: PathBuf,
}

impl RunLayout {
  fn create(ctx: &WorkspaceContext) -> RailResult<Self> {
    let parent = create_state_directory(ctx.workspace_root(), &["runs"])?;
    remove_children(&parent, None)?;
    let directory = tempfile::Builder::new()
      .prefix("run-")
      .tempdir_in(&parent)
      .map_err(|error| RailError::message(format!("failed to create isolated execution root: {error}")))?;
    let root = directory.path().to_path_buf();
    let layout = Self {
      workspace: root.join("workspace"),
      target: root.join("target"),
      build: root.join("build"),
      temporary: root.join("tmp"),
      home: root.join("home"),
      cargo_home: root.join("home/cargo"),
      observations: root.join("observations"),
      root,
      _directory: directory,
    };
    for path in [
      &layout.workspace,
      &layout.target,
      &layout.build,
      &layout.temporary,
      &layout.home,
      &layout.cargo_home,
      &layout.observations,
    ] {
      fs::create_dir_all(path)?;
    }
    File::create(layout.home.join("openssl.cnf"))?;
    Ok(layout)
  }
}

impl Drop for RunLayout {
  fn drop(&mut self) {
    let _ = set_tree_owner_writable(&self.workspace);
  }
}

fn prepare_fetch_inventory_with_inputs(inputs: &FetchInputs) -> RailResult<FetchInventory> {
  let workspace_root = &inputs.workspace_root;
  let credential_capabilities = fetch_credential_capabilities(&inputs.cargo_config);
  let key = fetch_key(inputs, &credential_capabilities)?;
  let inventory_root = create_state_directory(workspace_root, &["inventories"])?;
  let destination = inventory_root.join(key.trim_start_matches("fetch-v1-sha256-"));
  retain_fetch_inventory(&inventory_root, &destination)?;
  match fs::symlink_metadata(&destination) {
    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
      return load_fetch_inventory(&destination, &key, &credential_capabilities, true);
    }
    Ok(_) => {
      return Err(RailError::with_help(
        format!(
          "hermetic fetch inventory '{}' is not a real directory",
          destination.display()
        ),
        "remove the hostile or corrupt path and retry, or run `cargo rail cache clean --scope workspace`",
      ));
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => return Err(error.into()),
  }

  let temporary = tempfile::Builder::new()
    .prefix("fetch-")
    .tempdir_in(&inventory_root)
    .map_err(|error| RailError::message(format!("failed to create isolated fetch root: {error}")))?;
  let payload = temporary.path().join("inventory");
  let cargo_home = payload.join("cargo-home");
  let home = temporary.path().join("home");
  fs::create_dir_all(&cargo_home)?;
  fs::create_dir_all(&home)?;
  let mut command = Command::new(toolchain_program(inputs.toolchain.rustc_sysroot(), "cargo"));
  command
    .current_dir(&inputs.cargo_current_dir)
    .args(["fetch", "--locked", "--manifest-path"])
    .arg(workspace_root.join("Cargo.toml"));
  configure_fetch_environment(
    &mut command,
    inputs.toolchain.rustc_sysroot(),
    &cargo_home,
    &home,
    false,
  );
  apply_acquisition_environment(&mut command, &inputs.cargo_config);
  crate::instrumentation::record_hermetic_fetch_execution();
  let status = command
    .status()
    .map_err(|error| RailError::message(format!("hermetic fetch action failed to start: {error}")))?;
  if !status.success() {
    return Err(RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }
  ensure_package_cache_file(&cargo_home)?;
  let manifest = capture_fetch_manifest(inputs, &cargo_home, &key, credential_capabilities.clone())?;
  crate::utils::write_file_atomic(&payload.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
  ensure_tree_bytes(&payload, FETCH_INVENTORY_MAX_BYTES, "hermetic fetch inventory")?;
  seal_fetch_inventory(&cargo_home)?;
  let published = match fs::rename(&payload, &destination) {
    Ok(()) => true,
    Err(error)
      if error.kind() == std::io::ErrorKind::AlreadyExists
        && fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()) =>
    {
      false
    }
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to publish isolated fetch inventory '{}': {error}",
        destination.display()
      )));
    }
  };
  if published {
    Ok(fetch_inventory_from_manifest(&destination, manifest, false))
  } else {
    load_fetch_inventory(&destination, &key, &credential_capabilities, false)
  }
}

fn fetch_key(inputs: &FetchInputs, credential_capabilities: &BTreeSet<String>) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-fetch-action\0");
  identity.frame(b"version", &FETCH_ACTION_VERSION.to_le_bytes());
  identity.frame(b"lock", inputs.lock_fingerprint.as_bytes());
  identity.frame(
    b"cargo-config",
    &inputs.cargo_config.portable_acquisition_identity(&inputs.source_root)?,
  );
  let cargo = &inputs.cargo_implementation;
  identity.frame(b"cargo-content", cargo.content_digest().as_bytes());
  identity.frame(b"cargo-executable", &[u8::from(cargo.is_executable())]);
  identity.frame(b"cargo-version", inputs.toolchain.cargo_verbose_version().as_bytes());
  for capability in credential_capabilities {
    identity.frame(b"credential-capability", capability.as_bytes());
  }
  Ok(format!("fetch-v{FETCH_ACTION_VERSION}-sha256-{}", identity.finish()))
}

fn fetch_credential_capabilities(cargo_config: &CargoConfigSnapshot) -> BTreeSet<String> {
  let mut capabilities = BTreeSet::new();
  if cargo_config.has_credential_capability() {
    capabilities.insert("cargo-config-credentials".to_string());
  }
  for (name, _) in std::env::vars_os() {
    let Some(name) = name.to_str() else {
      continue;
    };
    let upper = name.to_ascii_uppercase();
    if upper == "CARGO_REGISTRY_TOKEN"
      || (upper.starts_with("CARGO_REGISTRIES_") && upper.ends_with("_TOKEN"))
      || upper == "GIT_SSH_COMMAND"
      || upper == "SSH_AUTH_SOCK"
    {
      capabilities.insert(name.to_string());
    }
  }
  capabilities
}

fn configure_fetch_environment(command: &mut Command, sysroot: &Path, cargo_home: &Path, home: &Path, offline: bool) {
  command.env_clear();
  for name in [
    "PATH",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CARGO_HTTP_CAINFO",
    "CARGO_NET_GIT_FETCH_WITH_CLI",
    "GIT_SSH_COMMAND",
    "SSH_AUTH_SOCK",
    "CARGO_REGISTRY_TOKEN",
  ] {
    if let Some(value) = std::env::var_os(name) {
      command.env(name, value);
    }
  }
  for (name, value) in std::env::vars_os() {
    if name
      .to_str()
      .is_some_and(|name| name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN"))
    {
      command.env(name, value);
    }
  }
  command
    .env("CARGO_HOME", cargo_home)
    .env("CARGO_CACHE_RUSTC_INFO", "0")
    .env("RUSTC", toolchain_program(sysroot, "rustc"))
    .env("RUSTDOC", toolchain_program(sysroot, "rustdoc"))
    .env("HOME", home)
    .env("LANG", "C")
    .env("LC_ALL", "C")
    .env("TZ", "UTC");
  if offline {
    command.env("CARGO_NET_OFFLINE", "true");
  }
}

fn apply_acquisition_environment(command: &mut Command, cargo_config: &CargoConfigSnapshot) {
  for (name, value) in cargo_config.non_secret_acquisition_environment() {
    command.env(name, value);
  }
}

fn ensure_package_cache_file(cargo_home: &Path) -> RailResult<()> {
  let path = cargo_home.join(".package-cache");
  if !path.exists() {
    File::create(&path).map_err(|error| {
      RailError::message(format!(
        "failed to create isolated Cargo package-cache lock '{}': {error}",
        path.display()
      ))
    })?;
  }
  Ok(())
}

fn load_fetch_inventory(
  root: &Path,
  key: &str,
  credential_capabilities: &BTreeSet<String>,
  reused: bool,
) -> RailResult<FetchInventory> {
  let manifest_path = root.join("manifest.json");
  let metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
    RailError::message(format!(
      "failed to inspect hermetic fetch inventory '{}': {error}",
      root.display()
    ))
  })?;
  if !metadata.is_file() || metadata.file_type().is_symlink() {
    return Err(RailError::with_help(
      format!(
        "hermetic fetch inventory manifest '{}' is not a regular file",
        manifest_path.display()
      ),
      "remove this workspace's hermetic cache with `cargo rail cache clean --scope workspace` and retry",
    ));
  }
  let bytes = fs::read(&manifest_path).map_err(|error| {
    RailError::message(format!(
      "failed to read hermetic fetch inventory '{}': {error}",
      root.display()
    ))
  })?;
  let stored: FetchManifest = serde_json::from_slice(&bytes).map_err(|error| {
    RailError::message(format!(
      "failed to parse hermetic fetch inventory '{}': {error}",
      root.display()
    ))
  })?;
  if stored.version != FETCH_ACTION_VERSION
    || stored.key != key
    || &stored.credential_capabilities != credential_capabilities
    || fetch_result_digest(&stored.packages)? != stored.result_digest
  {
    return Err(RailError::with_help(
      "hermetic fetch inventory is incompatible with the current action",
      "remove this workspace's hermetic cache with `cargo rail cache clean --scope workspace` and retry",
    ));
  }
  let cargo_home = root.join("cargo-home");
  let ignored = cargo_mutable_cache_paths(&cargo_home);
  let current = capture_exact_tree(&cargo_home, &ignored)?;
  if current.digest != stored.cargo_home_digest || current.entries != stored.cargo_home_entries {
    return Err(RailError::with_help(
      "hermetic fetch inventory failed exact source revalidation",
      "remove this workspace's hermetic cache with `cargo rail cache clean --scope workspace` and fetch again",
    ));
  }
  Ok(fetch_inventory_from_manifest(root, stored, reused))
}

fn fetch_inventory_from_manifest(root: &Path, stored: FetchManifest, reused: bool) -> FetchInventory {
  let source_entries = stored.packages.iter().map(|package| package.source_entries).sum();
  let packages = stored.packages;
  FetchInventory {
    cargo_home: root.join("cargo-home"),
    cargo_home_digest: stored.cargo_home_digest,
    cargo_home_entries: stored.cargo_home_entries,
    summary: FetchSummary {
      version: FETCH_ACTION_VERSION,
      key: stored.key,
      result_digest: stored.result_digest,
      packages: packages.len(),
      source_entries,
      inventory_entries: stored.cargo_home_entries,
      credential_capabilities: stored.credential_capabilities,
      reused,
    },
    packages,
  }
}

fn capture_fetch_manifest(
  inputs: &FetchInputs,
  cargo_home: &Path,
  key: &str,
  credential_capabilities: BTreeSet<String>,
) -> RailResult<FetchManifest> {
  let metadata = isolated_metadata(inputs, cargo_home)?;
  let cargo_home = crate::utils::canonicalize_existing(cargo_home)?;
  let source_root = crate::utils::canonicalize_existing(&inputs.source_root)?;
  let mut packages = Vec::new();
  for package in &metadata.packages {
    let root = package
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message(format!("external package '{}' has no source root", package.id)))?
      .as_std_path();
    let canonical = crate::utils::canonicalize_existing(root)
      .map_err(|error| RailError::message(format!("failed to resolve external package '{}': {error}", package.id)))?;
    if !canonical.starts_with(&cargo_home) {
      if canonical.starts_with(&source_root) {
        continue;
      }
      return Err(RailError::with_help(
        format!(
          "external package '{}' resolved outside the isolated Cargo home",
          package.name
        ),
        "use a locked registry/Git source or a repository-local vendored source",
      ));
    }
    let ignored = [canonical.join(".git"), canonical.join("target")];
    let source = capture_exact_tree(&canonical, &ignored)?;
    let version = package.version.to_string();
    let checksum = inputs.locked_packages.iter().find_map(|locked| {
      (locked.name == package.name.as_str()
        && locked.version == version
        && locked.source.as_deref() == package.source.as_ref().map(|source| source.repr.as_str()))
      .then(|| locked.checksum.clone())
      .flatten()
    });
    let source_digest = ContentDigest::sha256(package.id.repr.as_bytes());
    packages.push(InventoryPackage {
      identity: format!("{}@{}#source-sha256-{source_digest}", package.name, package.version),
      checksum,
      source_tree_digest: source.digest,
      source_entries: source.entries,
    });
  }
  packages.sort();
  packages.dedup();
  let ignored = cargo_mutable_cache_paths(&cargo_home);
  let cargo_home_snapshot = capture_exact_tree(&cargo_home, &ignored)?;
  let cargo_home_digest = cargo_home_snapshot.digest;
  let cargo_home_entries = cargo_home_snapshot.entries;
  let result_digest = fetch_result_digest(&packages)?;
  Ok(FetchManifest {
    version: FETCH_ACTION_VERSION,
    key: key.to_string(),
    result_digest,
    cargo_home_digest,
    cargo_home_entries,
    packages,
    credential_capabilities,
  })
}

fn isolated_metadata(inputs: &FetchInputs, cargo_home: &Path) -> RailResult<Metadata> {
  crate::instrumentation::record_cargo_metadata_load(false);
  let home = tempfile::Builder::new()
    .prefix("cargo-rail-metadata-")
    .tempdir()
    .map_err(|error| RailError::message(format!("failed to create isolated Cargo metadata home: {error}")))?;
  let mut command = Command::new(toolchain_program(inputs.toolchain.rustc_sysroot(), "cargo"));
  command
    .current_dir(&inputs.cargo_current_dir)
    .args([
      "metadata",
      "--format-version",
      "1",
      "--locked",
      "--offline",
      "--manifest-path",
    ])
    .arg(inputs.workspace_root.join("Cargo.toml"));
  configure_fetch_environment(
    &mut command,
    inputs.toolchain.rustc_sysroot(),
    cargo_home,
    home.path(),
    true,
  );
  apply_acquisition_environment(&mut command, &inputs.cargo_config);
  let output = command
    .output()
    .map_err(|error| RailError::message(format!("failed to inspect isolated Cargo source inventory: {error}")))?;
  if !output.status.success() {
    return Err(RailError::with_help(
      format!(
        "isolated Cargo source inventory is incomplete: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      ),
      "run the hermetic profile with network access so its explicit fetch action can complete",
    ));
  }
  serde_json::from_slice(&output.stdout).map_err(|error| {
    RailError::message(format!(
      "Cargo emitted invalid metadata for the fetch inventory: {error}"
    ))
  })
}

fn capture_exact_tree(root: &Path, ignored_roots: &[PathBuf]) -> RailResult<ExactTreeCapture> {
  let requested_root = root.to_path_buf();
  let root = crate::utils::canonicalize_existing(root)?;
  let ignored = ignored_roots
    .iter()
    .map(|path| {
      if path.is_relative() {
        root.join(path)
      } else if let Ok(relative) = path.strip_prefix(&requested_root) {
        root.join(relative)
      } else {
        path.clone()
      }
    })
    .collect::<Vec<_>>();
  let metadata = fs::symlink_metadata(&root)?;
  if !metadata.is_dir() || metadata.file_type().is_symlink() {
    return Err(RailError::message(format!(
      "exact source inventory root '{}' is not a real directory",
      root.display()
    )));
  }

  let mut entries = Vec::new();
  let mut pending = vec![root.clone()];
  while let Some(path) = pending.pop() {
    if path != root
      && ignored
        .iter()
        .any(|ignored| path == *ignored || path.starts_with(ignored))
    {
      continue;
    }
    let before = fs::symlink_metadata(&path)?;
    if before.is_dir() && !before.file_type().is_symlink() {
      let mut children = fs::read_dir(&path)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
      let after = fs::symlink_metadata(&path)?;
      ensure_exact_capture_metadata(&path, &before, &after)?;
      children.sort();
      children.reverse();
      pending.extend(children);
      continue;
    }

    let relative = path.strip_prefix(&root).map_err(|_| {
      RailError::message(format!(
        "exact source inventory entry '{}' escaped root '{}'",
        path.display(),
        root.display()
      ))
    })?;
    let relative = crate::source::RepositoryPath::new(relative)?;
    let kind = if before.file_type().is_symlink() {
      let target = fs::read_link(&path)?;
      let target = target.to_str().ok_or_else(|| {
        RailError::message(format!(
          "source inventory symlink '{}' has a non-UTF-8 target",
          path.display()
        ))
      })?;
      if symlink_target_escapes(&relative, target) {
        return Err(RailError::with_help(
          format!(
            "Cargo fetch inventory symlink '{}' escapes the immutable store",
            relative
          ),
          "use registry, Git, or vendored dependencies whose source trees are self-contained",
        ));
      }
      ExactTreeEntryKind::Symlink {
        target: target.to_string(),
      }
    } else if before.is_file() {
      let (digest, _, _) = digest_and_scan_file(&path, &[])?;
      ExactTreeEntryKind::File {
        digest,
        executable: exact_tree_executable(&before),
      }
    } else {
      return Err(RailError::with_help(
        format!("source inventory entry '{}' has an unsupported file type", relative),
        "use dependency sources containing only regular files, directories, and bounded symlinks",
      ));
    };
    let after = fs::symlink_metadata(&path)?;
    ensure_exact_capture_metadata(&path, &before, &after)?;
    entries.push(ExactTreeEntry { path: relative, kind });
  }
  entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));

  let mut identity = FramedHasher::new(b"cargo-rail-fetch-source-tree\0");
  for entry in &entries {
    identity.frame(b"path", entry.path.as_str().as_bytes());
    match &entry.kind {
      ExactTreeEntryKind::File { digest, executable } => {
        identity.frame(b"kind", b"file");
        identity.frame(b"content", digest.as_bytes());
        identity.frame(b"executable", &[u8::from(*executable)]);
      }
      ExactTreeEntryKind::Symlink { target } => {
        identity.frame(b"kind", b"symlink");
        identity.frame(b"target", target.as_bytes());
      }
    }
  }
  Ok(ExactTreeCapture {
    digest: format!("sha256:{}", identity.finish()),
    entries: entries.len(),
  })
}

fn ensure_exact_capture_metadata(path: &Path, before: &fs::Metadata, after: &fs::Metadata) -> RailResult<()> {
  if exact_capture_metadata_unchanged(before, after) {
    return Ok(());
  }
  Err(RailError::with_help(
    format!(
      "source inventory entry '{}' changed while it was hashed",
      path.display()
    ),
    "retry after dependency acquisition and concurrent filesystem writes have finished",
  ))
}

#[cfg(target_os = "macos")]
fn exact_capture_metadata_unchanged(before: &fs::Metadata, after: &fs::Metadata) -> bool {
  read_boundary_metadata(before) == read_boundary_metadata(after)
}

#[cfg(not(target_os = "macos"))]
fn exact_capture_metadata_unchanged(before: &fs::Metadata, after: &fs::Metadata) -> bool {
  before.file_type().is_dir() == after.file_type().is_dir()
    && before.file_type().is_file() == after.file_type().is_file()
    && before.file_type().is_symlink() == after.file_type().is_symlink()
    && before.len() == after.len()
    && output_mode(before) == output_mode(after)
    && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
fn exact_tree_executable(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn exact_tree_executable(_metadata: &fs::Metadata) -> bool {
  false
}

fn fetch_result_digest(packages: &[InventoryPackage]) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-fetch-result\0");
  identity.frame(b"version", &FETCH_ACTION_VERSION.to_le_bytes());
  for package in packages {
    identity.frame(b"package", &serde_json::to_vec(package)?);
  }
  Ok(format!(
    "fetch-result-v{FETCH_ACTION_VERSION}-sha256-{}",
    identity.finish()
  ))
}

fn seal_fetch_inventory(cargo_home: &Path) -> RailResult<()> {
  set_tree_read_only(cargo_home)?;
  for path in cargo_mutable_cache_paths(cargo_home) {
    if path.is_file() {
      make_private_path_owner_writable(&path)?;
    }
  }
  Ok(())
}

fn cargo_mutable_cache_paths(cargo_home: &Path) -> Vec<PathBuf> {
  CARGO_MUTABLE_CACHE_FILES
    .iter()
    .map(|name| cargo_home.join(name))
    .collect()
}

#[cfg(target_os = "macos")]
fn immutable_inventory_paths(cargo_home: &Path) -> RailResult<(Vec<PathBuf>, Vec<PathBuf>)> {
  let mut directories = Vec::new();
  let mut files = Vec::new();
  for entry in fs::read_dir(cargo_home)? {
    let entry = entry?;
    if CARGO_MUTABLE_CACHE_FILES
      .iter()
      .any(|name| entry.file_name() == OsStr::new(name))
    {
      continue;
    }
    let path = entry.path();
    if fs::metadata(&path)?.is_dir() {
      directories.push(path);
    } else {
      files.push(path);
    }
  }
  directories.sort();
  files.sort();
  Ok((directories, files))
}

fn materialize_runtime_cargo_home(
  layout: &RunLayout,
  inventory: &FetchInventory,
  snapshot: &WorkspaceSnapshot,
) -> RailResult<()> {
  for entry in fs::read_dir(&inventory.cargo_home)? {
    let entry = entry?;
    if CARGO_MUTABLE_CACHE_FILES
      .iter()
      .any(|name| entry.file_name() == OsStr::new(name))
    {
      continue;
    }
    link_inventory_entry(&entry.path(), &layout.cargo_home.join(entry.file_name()))?;
  }
  File::create(layout.cargo_home.join(".package-cache"))?;
  if let Some(source) = snapshot.cargo_config().effective_file_settings().get("source") {
    if !remote_registry_source_configuration(source) {
      return Err(RailError::message(
        "unsupported Cargo source configuration reached hermetic materialization",
      ));
    }
    let configuration = serde_json::json!({ "source": source });
    let rendered = toml_edit::ser::to_string_pretty(&configuration)?;
    let path = layout.cargo_home.join("config.toml");
    crate::utils::write_file_atomic(&path, rendered.as_bytes())?;
    #[cfg(unix)]
    {
      let mut permissions = fs::metadata(&path)?.permissions();
      permissions.set_readonly(true);
      fs::set_permissions(path, permissions)?;
    }
  }
  Ok(())
}

#[cfg(unix)]
fn link_inventory_entry(source: &Path, destination: &Path) -> RailResult<()> {
  std::os::unix::fs::symlink(source, destination).map_err(|error| {
    RailError::message(format!(
      "failed to link immutable Cargo inventory '{}': {error}",
      destination.display()
    ))
  })
}

#[cfg(windows)]
fn link_inventory_entry(source: &Path, destination: &Path) -> RailResult<()> {
  let result = if fs::metadata(source)?.is_dir() {
    std::os::windows::fs::symlink_dir(source, destination)
  } else {
    std::os::windows::fs::symlink_file(source, destination)
  };
  result.map_err(|error| {
    RailError::message(format!(
      "failed to link immutable Cargo inventory '{}': {error}",
      destination.display()
    ))
  })
}

#[cfg(not(any(unix, windows)))]
fn link_inventory_entry(_source: &Path, destination: &Path) -> RailResult<()> {
  Err(RailError::message(format!(
    "immutable Cargo inventory links are unsupported on this platform: '{}'",
    destination.display()
  )))
}

fn materialize_source(snapshot: &WorkspaceSnapshot, destination: &Path) -> RailResult<()> {
  for entry in snapshot.source().tree().entries() {
    let target = destination.join(entry.path.as_path());
    let parent = target
      .parent()
      .ok_or_else(|| RailError::message(format!("source entry '{}' has no parent", entry.path)))?;
    fs::create_dir_all(parent)?;
    let source = snapshot.source_root().join(entry.path.as_path());
    match &entry.kind {
      SourceEntryKind::RegularFile { digest, executable } => {
        copy_verified_file(&source, &target, *digest, *executable)?;
      }
      SourceEntryKind::Symlink { target: link_target } => {
        if symlink_target_escapes(&entry.path, link_target) {
          return Err(RailError::with_help(
            format!("source symlink '{}' escapes the hermetic source root", entry.path),
            "replace it with a repository-contained relative symlink or declare the external input explicitly",
          ));
        }
        create_symlink(Path::new(link_target), &target, &source)?;
      }
      SourceEntryKind::Deleted => {
        return Err(RailError::message(format!(
          "captured source contains deleted entry '{}'",
          entry.path
        )));
      }
    }
  }
  if let Some(lockfile) = snapshot.lockfile() {
    let file = lockfile.file();
    let path = destination.join(file.path().as_path());
    let parent = path
      .parent()
      .ok_or_else(|| RailError::message(format!("lockfile '{}' has no parent", file.path())))?;
    fs::create_dir_all(parent)?;
    crate::utils::write_file_atomic(&path, file.bytes())?;
  }
  set_tree_read_only(destination)
}

fn copy_verified_file(source: &Path, destination: &Path, expected: ContentDigest, executable: bool) -> RailResult<()> {
  let mut input = File::open(source)
    .map_err(|error| RailError::message(format!("failed to read source '{}': {error}", source.display())))?;
  let mut output = File::create(destination).map_err(|error| {
    RailError::message(format!(
      "failed to materialize isolated source '{}': {error}",
      destination.display()
    ))
  })?;
  let mut hasher = Sha256::new();
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let read = input
      .read(&mut buffer)
      .map_err(|error| RailError::message(format!("failed to read source '{}': {error}", source.display())))?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    output.write_all(&buffer[..read]).map_err(|error| {
      RailError::message(format!(
        "failed to materialize isolated source '{}': {error}",
        destination.display()
      ))
    })?;
  }
  let actual = ContentDigest::from_sha256_bytes(hasher.finalize().into());
  if actual != expected {
    return Err(RailError::with_help(
      format!(
        "source '{}' changed while the isolated root was materialized",
        source.display()
      ),
      "retry after concurrent workspace writes have stopped",
    ));
  }
  set_executable(destination, executable)?;
  Ok(())
}

fn symlink_target_escapes(path: &crate::source::RepositoryPath, target: &str) -> bool {
  let target = Path::new(target);
  if target.is_absolute() {
    return true;
  }
  let mut depth = path.as_path().parent().map_or(0usize, |parent| {
    parent
      .components()
      .filter(|component| matches!(component, Component::Normal(_)))
      .count()
  });
  for component in target.components() {
    match component {
      Component::CurDir => {}
      Component::Normal(_) => depth = depth.saturating_add(1),
      Component::ParentDir if depth > 0 => depth -= 1,
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
    }
  }
  false
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path, _source: &Path) -> RailResult<()> {
  std::os::unix::fs::symlink(target, destination).map_err(|error| {
    RailError::message(format!(
      "failed to materialize symlink '{}': {error}",
      destination.display()
    ))
  })
}

#[cfg(windows)]
fn create_symlink(target: &Path, destination: &Path, source: &Path) -> RailResult<()> {
  if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
    std::os::windows::fs::symlink_dir(target, destination)
  } else {
    std::os::windows::fs::symlink_file(target, destination)
  }
  .map_err(|error| {
    RailError::message(format!(
      "failed to materialize symlink '{}': {error}",
      destination.display()
    ))
  })
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(_target: &Path, destination: &Path, _source: &Path) -> RailResult<()> {
  Err(RailError::message(format!(
    "symlink materialization is unsupported on this platform: '{}'",
    destination.display()
  )))
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let mut permissions = fs::metadata(path)?.permissions();
  let mut mode = permissions.mode();
  if executable {
    mode |= 0o111;
  } else {
    mode &= !0o111;
  }
  permissions.set_mode(mode);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> RailResult<()> {
  Ok(())
}

fn set_tree_read_only(root: &Path) -> RailResult<()> {
  let mut paths = vec![root.to_path_buf()];
  let mut directories = Vec::new();
  while let Some(path) = paths.pop() {
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
      continue;
    }
    if metadata.is_dir() {
      directories.push(path.clone());
      let entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
      paths.extend(entries.into_iter().map(|entry| entry.path()));
    } else {
      make_path_read_only(&path, metadata.permissions())?;
    }
  }
  directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
  for directory in directories {
    let permissions = fs::metadata(&directory)?.permissions();
    make_path_read_only(&directory, permissions)?;
  }
  Ok(())
}

#[cfg(unix)]
fn set_tree_owner_writable(root: &Path) -> RailResult<()> {
  use rustix::fd::OwnedFd;
  use rustix::fs::{Dir, Mode, OFlags};
  use std::os::unix::fs::MetadataExt as _;
  use std::path::Component;

  fn io_error(error: rustix::io::Errno) -> RailError {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
  }

  fn open_directory<Fd: std::os::fd::AsFd>(parent: Fd, name: &std::ffi::OsStr) -> Result<OwnedFd, rustix::io::Errno> {
    rustix::fs::openat(
      parent,
      name,
      OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
      Mode::empty(),
    )
  }

  fn make_directories_writable(directory: OwnedFd) -> RailResult<()> {
    let stat = rustix::fs::fstat(&directory).map_err(io_error)?;
    rustix::fs::fchmod(&directory, Mode::from_raw_mode(stat.st_mode) | Mode::RWXU).map_err(io_error)?;
    let mut entries = Dir::read_from(&directory).map_err(io_error)?;
    for entry in &mut entries {
      let entry = entry.map_err(io_error)?;
      let name = entry.file_name();
      if name.to_bytes() == b"." || name.to_bytes() == b".." {
        continue;
      }
      match rustix::fs::openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
      ) {
        Ok(child) => make_directories_writable(child)?,
        Err(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP | rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(io_error(error)),
      }
    }
    Ok(())
  }

  if !root.is_absolute() {
    return Err(RailError::message(format!(
      "refusing to reclaim non-absolute cache path '{}'",
      root.display()
    )));
  }
  let expected = fs::symlink_metadata(root)?;
  if !expected.is_dir() || crate::utils::is_symlink_or_reparse(&expected) {
    return Err(RailError::message(format!(
      "refusing to reclaim non-directory cache path '{}'",
      root.display()
    )));
  }
  let canonical = fs::canonicalize(root)?;
  let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
  let mut directory = rustix::fs::open("/", flags, Mode::empty()).map_err(io_error)?;
  for component in canonical.components() {
    match component {
      Component::RootDir => {}
      Component::Normal(name) => directory = open_directory(&directory, name).map_err(io_error)?,
      Component::CurDir => {}
      Component::ParentDir | Component::Prefix(_) => {
        return Err(RailError::message(format!(
          "refusing to reclaim non-normalized cache path '{}'",
          root.display()
        )));
      }
    }
  }
  let opened = rustix::fs::fstat(&directory).map_err(io_error)?;
  if u64::try_from(opened.st_dev).ok() != Some(expected.dev()) || opened.st_ino != expected.ino() {
    return Err(RailError::message(format!(
      "cache path '{}' changed while reclamation authority was acquired",
      root.display()
    )));
  }
  make_directories_writable(directory)
}

#[cfg(not(unix))]
fn set_tree_owner_writable(root: &Path) -> RailResult<()> {
  if !root.exists() {
    return Ok(());
  }
  let mut paths = vec![root.to_path_buf()];
  while let Some(path) = paths.pop() {
    let metadata = fs::symlink_metadata(&path)?;
    if crate::utils::is_symlink_or_reparse(&metadata) {
      continue;
    }
    if metadata.is_dir() {
      paths.extend(
        fs::read_dir(&path)?
          .collect::<Result<Vec<_>, _>>()?
          .into_iter()
          .map(|entry| entry.path()),
      );
    }
    make_path_owner_writable(&path)?;
  }
  Ok(())
}

#[cfg(unix)]
fn make_path_read_only(path: &Path, mut permissions: fs::Permissions) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  permissions.set_mode(permissions.mode() & !0o222);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[cfg(not(unix))]
fn make_path_read_only(path: &Path, mut permissions: fs::Permissions) -> RailResult<()> {
  permissions.set_readonly(true);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[cfg(not(unix))]
fn make_path_owner_writable(path: &Path) -> RailResult<()> {
  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_readonly(false);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

// These Cargo bookkeeping files are still inside an unpublished private
// TempDir, so no external actor can replace a parent during this path-based
// permission change. Published-tree reclamation uses descriptor-relative
// traversal above.
#[cfg(unix)]
fn make_private_path_owner_writable(path: &Path) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_mode(permissions.mode() | 0o600);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[cfg(not(unix))]
fn make_private_path_owner_writable(path: &Path) -> RailResult<()> {
  make_path_owner_writable(path)
}

fn run_cargo_check(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
  inventory: &FetchInventory,
  platform: &PlatformBoundary,
  cargo_environment: &[(String, OsString, String)],
) -> RailResult<Output> {
  let cargo = toolchain_program(snapshot.toolchain().rustc_sysroot(), "cargo");
  let (program, mut command) =
    if let (Some(sandbox), Some(policy)) = (&platform.sandbox_program, &platform.sandbox_policy) {
      let mut command = Command::new(sandbox);
      command.arg("-p").arg(policy).arg(&cargo);
      (sandbox.as_os_str(), command)
    } else {
      (cargo.as_os_str(), Command::new(&cargo))
    };
  let arguments = hermetic_cargo_arguments(action, snapshot)?;
  let working_directory = hermetic_working_directory(action, snapshot, layout)?;
  let rustc = toolchain_program(snapshot.toolchain().rustc_sysroot(), "rustc");
  let current_executable = std::env::current_exe()
    .map_err(|error| RailError::message(format!("failed to locate cargo-rail compiler observer: {error}")))?;
  command.args(arguments).current_dir(&working_directory).env_clear();
  let rustflags = hermetic_rustflags(action, snapshot, layout, inventory)?;
  for (name, value, _) in cargo_environment {
    command.env(name, value);
  }
  command
    .env("PATH", snapshot.toolchain().rustc_sysroot().join("bin"))
    .env("RUSTC", rustc)
    .env("HOME", &layout.home)
    .env("CARGO_HOME", &layout.cargo_home)
    .env("CARGO_TARGET_DIR", &layout.target)
    .env("CARGO_BUILD_BUILD_DIR", &layout.build)
    .env("CARGO_BUILD_DEP_INFO_BASEDIR", ".")
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_NET_OFFLINE", "true")
    .env("CARGO_CACHE_RUSTC_INFO", "0")
    .env("TMPDIR", &layout.temporary)
    .env("TMP", &layout.temporary)
    .env("TEMP", &layout.temporary)
    .env("LANG", "C")
    .env("LC_ALL", "C")
    .env("TZ", "UTC")
    .env("SOURCE_DATE_EPOCH", "1")
    .env("OPENSSL_CONF", layout.home.join("openssl.cnf"))
    .env("CARGO_ENCODED_RUSTFLAGS", rustflags.join("\x1f"))
    .env("RUSTC_WRAPPER", &current_executable)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove(crate::compiler::wrapper::INNER_WRAPPER_ENV)
    .env(WRAPPER_MARKER, "1")
    .env(OBSERVATION_ONLY_ENV, "1")
    .env(OBSERVATION_DIRECTORY_ENV, &layout.observations)
    .env(OBSERVATION_SOURCE_ROOT_ENV, &layout.root);
  command.output().map_err(|error| {
    RailError::message(format!(
      "failed to execute hermetic Cargo check through '{}': {error}",
      program.to_string_lossy()
    ))
  })
}

fn hermetic_cargo_arguments(action: &ExpandedAction, snapshot: &WorkspaceSnapshot) -> RailResult<Vec<OsString>> {
  let arguments = action.argv().iter().skip(1).map(OsString::from).collect::<Vec<_>>();
  let separator = arguments
    .iter()
    .position(|argument| argument == "--")
    .unwrap_or(arguments.len());
  let mut cargo_arguments = Vec::with_capacity(arguments.len() + 3);
  let mut index = 0usize;
  while index < separator {
    let argument = &arguments[index];
    if argument == "--message-format" {
      index = index.saturating_add(2);
      continue;
    }
    if argument
      .to_str()
      .is_some_and(|argument| argument.starts_with("--message-format="))
    {
      index += 1;
      continue;
    }
    cargo_arguments.push(argument.clone());
    index += 1;
  }
  for required in ["--locked", "--offline"] {
    if !cargo_arguments.iter().any(|argument| argument == required) {
      cargo_arguments.push(OsString::from(required));
    }
  }
  let target_selected_in_argv = cargo_arguments.iter().any(|argument| {
    argument == "--target"
      || argument
        .to_str()
        .is_some_and(|argument| argument.starts_with("--target="))
  });
  let target = hermetic_target(action, snapshot)?;
  if target.is_build_target() && !target_selected_in_argv {
    let crate::cargo::TargetSpecificationIdentity::BuiltIn(target) = target.specification() else {
      return Err(RailError::message(
        "custom target execution reached the graduated hermetic Cargo argument boundary",
      ));
    };
    cargo_arguments.push(OsString::from("--target"));
    cargo_arguments.push(OsString::from(target));
  }
  cargo_arguments.push(OsString::from("--message-format=json-render-diagnostics"));
  cargo_arguments.extend(arguments.into_iter().skip(separator));
  Ok(cargo_arguments)
}

fn hermetic_cargo_environment(
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
) -> RailResult<Vec<(String, OsString, String)>> {
  let mut bindings = snapshot
    .cargo_config()
    .materialized_environment(snapshot.source_root(), &layout.workspace)?;
  let configured = bindings
    .iter()
    .map(|(name, _, _)| name.clone())
    .collect::<BTreeSet<_>>();
  for (name, value) in snapshot.cargo_config().environment() {
    if name.starts_with("CARGO_PROFILE_") && !configured.contains(name) {
      bindings.push((name.clone(), OsString::from(value), value.clone()));
    }
  }
  for (name, value) in snapshot.cargo_config().non_secret_acquisition_environment() {
    if cargo_offline_resolution_environment(&name)
      && !hermetic_environment_name_is_controlled(&name)
      && !configured.contains(&name)
    {
      let path = Path::new(&value);
      let (materialized, identity) = if path.is_absolute() {
        path.strip_prefix(snapshot.source_root()).map_or_else(
          |_| (OsString::from(&value), value.clone()),
          |relative| {
            (
              layout.workspace.join(relative).into_os_string(),
              format!("repository:{}", crate::utils::path_to_git_format(relative)),
            )
          },
        )
      } else {
        (OsString::from(&value), value.clone())
      };
      bindings.push((name, materialized, identity));
    }
  }
  bindings.sort_by(|left, right| left.0.cmp(&right.0));
  Ok(bindings)
}

fn cargo_offline_resolution_environment(name: &str) -> bool {
  name.starts_with("CARGO_NET_")
    || name.starts_with("CARGO_REGISTRIES_")
    || name.starts_with("CARGO_REGISTRY_")
    || name.starts_with("CARGO_SOURCE_")
}

fn hermetic_working_directory(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
) -> RailResult<PathBuf> {
  let workspace_relative = snapshot
    .cargo_current_dir()
    .strip_prefix(snapshot.source_root())
    .map_err(|_| RailError::message("Cargo workspace lies outside the captured source root"))?;
  let workspace = layout.workspace.join(workspace_relative);
  let directory = match action.working_directory() {
    ActionWorkingDirectory::Workspace => workspace,
    ActionWorkingDirectory::Repository(path) => workspace.join(path.as_path()),
  };
  if !directory.is_dir() {
    return Err(RailError::message(format!(
      "isolated action working directory '{}' is unavailable",
      directory.display()
    )));
  }
  Ok(directory)
}

fn hermetic_rustflags(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
  inventory: &FetchInventory,
) -> RailResult<Vec<String>> {
  let target = hermetic_target(action, snapshot)?;
  let mut flags = target.rustflags().to_vec();
  for (physical, logical) in [
    (layout.workspace.as_path(), "/workspace"),
    (layout.target.as_path(), "/target"),
    (layout.build.as_path(), "/build"),
    (layout.cargo_home.as_path(), "/cargo-home"),
    (inventory.cargo_home.as_path(), "/cargo-home"),
    (snapshot.toolchain().rustc_sysroot(), "/rust-toolchain"),
  ] {
    flags.push(format!("--remap-path-prefix={}={logical}", physical.display()));
  }
  Ok(flags)
}

fn hermetic_target<'a>(
  action: &ExpandedAction,
  snapshot: &'a WorkspaceSnapshot,
) -> RailResult<&'a crate::cargo::TargetIdentity> {
  let target = if let Some(requested) = action.selected_targets().first() {
    snapshot.targets().iter().find(|target| match target.specification() {
      crate::cargo::TargetSpecificationIdentity::BuiltIn(name) => name == requested,
      crate::cargo::TargetSpecificationIdentity::Custom(specification) => specification.name() == requested,
    })
  } else {
    snapshot
      .targets()
      .iter()
      .find(|target| target.is_build_target())
      .or_else(|| snapshot.targets().iter().find(|target| target.is_host()))
  }
  .ok_or_else(|| RailError::message("hermetic action has no captured compilation target"))?;
  Ok(target)
}

fn validate_invocations(invocations: &[RawCompilerInvocation], reasons: &mut BTreeSet<String>) {
  if invocations.is_empty() {
    reasons.insert("compiler_observations_unavailable".to_string());
  }
  for invocation in invocations {
    if !invocation.success {
      reasons.insert("compiler_execution_failed".to_string());
    }
    if invocation.emitted_outputs.is_empty() {
      reasons.insert("compiler_outputs_unobserved".to_string());
    }
    reasons.extend(invocation.bypasses.iter().cloned());
    if invocation
      .environment_reads
      .iter()
      .any(|environment| environment.secret_capability)
    {
      reasons.insert("secret_compiler_environment".to_string());
    }
  }
}

fn normalize_dep_info_outputs(
  invocations: &mut [RawCompilerInvocation],
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
  inventory: &FetchInventory,
) -> RailResult<()> {
  let mut mappings = Vec::<(Vec<u8>, &'static [u8])>::new();
  for (physical, logical) in [
    (layout.workspace.as_path(), b"/workspace".as_slice()),
    (layout.target.as_path(), b"/target".as_slice()),
    (layout.build.as_path(), b"/build".as_slice()),
    (layout.cargo_home.as_path(), b"/cargo-home".as_slice()),
    (inventory.cargo_home.as_path(), b"/cargo-home".as_slice()),
    (snapshot.toolchain().rustc_sysroot(), b"/rust-toolchain".as_slice()),
    (layout.root.as_path(), b"/run".as_slice()),
  ] {
    for alias in [physical.to_path_buf(), fs::canonicalize(physical)?] {
      let bytes = alias.as_os_str().as_encoded_bytes().to_vec();
      if !bytes.is_empty() && !mappings.iter().any(|(existing, _)| existing == &bytes) {
        mappings.push((bytes, logical));
      }
    }
  }
  mappings.sort_unstable_by_key(|(physical, _)| std::cmp::Reverse(physical.len()));

  let mut normalized = BTreeMap::<ObservationPath, FileObservation>::new();
  for invocation in invocations.iter() {
    if !invocation.emit_modes.contains("dep-info") {
      continue;
    }
    for output in &invocation.emitted_outputs {
      let path = output.path.resolve(&layout.root);
      if path.extension() != Some(OsStr::new("d")) || normalized.contains_key(&output.path) {
        continue;
      }
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(RailError::message(format!(
          "dep-info output '{}' is not a regular file",
          path.display()
        )));
      }
      if metadata.len() > MAX_DEP_INFO_BYTES {
        return Err(RailError::message(format!(
          "dep-info output '{}' exceeds {MAX_DEP_INFO_BYTES} bytes",
          path.display()
        )));
      }
      let mut bytes = fs::read(&path)?;
      for (physical, logical) in &mappings {
        bytes = replace_bytes(&bytes, physical, logical);
      }
      fs::write(&path, bytes)?;
      normalized.insert(
        output.path.clone(),
        FileObservation::capture(&path, &layout.root, &layout.root)?,
      );
    }
  }
  for invocation in invocations {
    for output in &mut invocation.emitted_outputs {
      if let Some(replacement) = normalized.get(&output.path) {
        *output = replacement.clone();
      }
    }
  }
  Ok(())
}

fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
  if needle.is_empty() || needle.len() > input.len() {
    return input.to_vec();
  }
  let mut output = Vec::with_capacity(input.len());
  let mut cursor = 0usize;
  while let Some(offset) = input[cursor..]
    .windows(needle.len())
    .position(|window| window == needle)
  {
    let match_start = cursor + offset;
    output.extend_from_slice(&input[cursor..match_start]);
    output.extend_from_slice(replacement);
    cursor = match_start + needle.len();
  }
  output.extend_from_slice(&input[cursor..]);
  output
}

struct CargoArtifactOutputs {
  files: Vec<FileObservation>,
  crate_names: Vec<String>,
}

fn capture_cargo_artifact_outputs(output: &Output, layout: &RunLayout) -> RailResult<CargoArtifactOutputs> {
  let mut outputs = Vec::new();
  let mut crate_names = Vec::new();
  for message in Message::parse_stream(BufReader::new(output.stdout.as_slice())) {
    let message =
      message.map_err(|error| RailError::message(format!("failed to parse stable Cargo JSON message: {error}")))?;
    let Message::CompilerArtifact(artifact) = message else {
      continue;
    };
    crate_names.push(artifact.target.name.replace('-', "_"));
    for filename in artifact.filenames {
      outputs.push(FileObservation::capture(
        filename.as_std_path(),
        &layout.root,
        &layout.root,
      )?);
    }
    if let Some(executable) = artifact.executable {
      let executable = FileObservation::capture(executable.as_std_path(), &layout.root, &layout.root)?;
      if !outputs.contains(&executable) {
        outputs.push(executable);
      }
    }
  }
  outputs.sort();
  outputs.dedup();
  crate_names.sort();
  Ok(CargoArtifactOutputs {
    files: outputs,
    crate_names,
  })
}

fn capture_compiler_outputs(
  run_root: &Path,
  invocations: &[RawCompilerInvocation],
  cargo_outputs: &[FileObservation],
  forbidden_roots: &[&Path],
  reasons: &mut BTreeSet<String>,
) -> RailResult<OutputManifest> {
  let mut declared = BTreeMap::<String, (PathBuf, String)>::new();
  for output in invocations
    .iter()
    .flat_map(|invocation| &invocation.emitted_outputs)
    .chain(cargo_outputs)
  {
    let (logical, physical) = match &output.path {
      ObservationPath::Repository(path) => (path.clone(), run_root.join(path)),
      ObservationPath::Host(path) => {
        reasons.insert("compiler_output_outside_isolated_root".to_string());
        (format!("host:{path}"), PathBuf::from(path))
      }
    };
    if !logical.starts_with("target/") && !logical.starts_with("build/") {
      reasons.insert("compiler_output_outside_declared_roots".to_string());
    }
    if let Some((_, existing)) = declared.get(&logical)
      && existing != &output.content_digest
    {
      reasons.insert("compiler_output_observation_conflict".to_string());
    }
    declared.insert(logical, (physical, output.content_digest.clone()));
  }

  let forbidden = forbidden_roots
    .iter()
    .filter_map(|root| root.to_str())
    .filter(|root| !root.is_empty())
    .map(|root| root.as_bytes().to_vec())
    .collect::<Vec<_>>();
  let mut entries = BTreeMap::<String, OutputEntryKind>::new();
  let mut file_count = 0usize;
  let mut symlink_count = 0usize;
  let mut bytes = 0u64;
  for (logical, (physical, expected)) in declared {
    let mut metadata = fs::symlink_metadata(&physical).map_err(|error| {
      RailError::message(format!(
        "failed to inspect declared compiler output '{logical}': {error}"
      ))
    })?;
    add_output_parent_directories(run_root, &physical, &mut entries)?;
    if metadata.file_type().is_symlink() {
      let target = fs::read_link(&physical)
        .map_err(|error| RailError::message(format!("failed to read compiler output symlink '{logical}': {error}")))?;
      let target = target
        .to_str()
        .ok_or_else(|| RailError::message(format!("compiler output symlink '{logical}' is not valid UTF-8")))?;
      let repository_path = crate::source::RepositoryPath::new(Path::new(&logical))?;
      if symlink_target_escapes(&repository_path, target) {
        reasons.insert("compiler_output_symlink_escape".to_string());
      }
      entries.insert(
        logical,
        OutputEntryKind::Symlink {
          target: target.to_string(),
        },
      );
      symlink_count += 1;
    } else if metadata.is_file() {
      normalize_output_mode(&physical, &metadata, false)?;
      metadata = fs::symlink_metadata(&physical)?;
      let (digest, length, contains_forbidden) = digest_and_scan_file(&physical, &forbidden)?;
      if contains_forbidden {
        reasons.insert("physical_path_in_compiler_output".to_string());
      }
      let digest = format!("sha256:{digest}");
      if expected != digest {
        reasons.insert("compiler_output_changed_after_observation".to_string());
      }
      entries.insert(
        logical,
        OutputEntryKind::File {
          digest,
          mode: output_mode(&metadata),
          bytes: length,
        },
      );
      file_count += 1;
      bytes = bytes.saturating_add(length);
    } else {
      reasons.insert("compiler_output_type_unsupported".to_string());
    }
    if entries.len() > MAX_OUTPUT_ENTRIES {
      return Err(RailError::message(format!(
        "compiler output manifest exceeds {MAX_OUTPUT_ENTRIES} entries"
      )));
    }
  }
  let entries = entries
    .into_iter()
    .map(|(path, kind)| OutputEntry { path, kind })
    .collect::<Vec<_>>();
  let directories = entries
    .iter()
    .filter(|entry| matches!(entry.kind, OutputEntryKind::Directory { .. }))
    .count();
  let digest = output_manifest_digest(&entries)?;
  Ok(OutputManifest {
    version: OUTPUT_MANIFEST_VERSION,
    digest,
    entries,
    files: file_count,
    directories,
    symlinks: symlink_count,
    bytes,
  })
}

/// Capture a bounded compiler-cache staging tree with the same output-manifest
/// validation used by the hermetic action CAS.
pub(crate) fn capture_native_compiler_outputs(root: &Path, paths: &[PathBuf]) -> RailResult<OutputManifest> {
  let mut outputs = paths
    .iter()
    .map(|path| FileObservation::capture(path, root, root))
    .collect::<RailResult<Vec<_>>>()?;
  outputs.sort();
  outputs.dedup();
  if outputs.len() != paths.len() {
    return Err(RailError::message("native compiler cache staging paths are not unique"));
  }
  let mut reasons = BTreeSet::new();
  let manifest = capture_compiler_outputs(root, &[], &outputs, &[], &mut reasons)?;
  if let Some(reason) = reasons.into_iter().next() {
    return Err(RailError::message(format!(
      "native compiler cache staging output is not reusable: {reason}"
    )));
  }
  Ok(manifest)
}

fn add_output_parent_directories(
  run_root: &Path,
  output: &Path,
  entries: &mut BTreeMap<String, OutputEntryKind>,
) -> RailResult<()> {
  let mut current = output.parent();
  while let Some(directory) = current {
    if directory == run_root {
      break;
    }
    let relative = directory.strip_prefix(run_root).map_err(|_| {
      RailError::message(format!(
        "compiler output directory '{}' escapes the isolated root",
        directory.display()
      ))
    })?;
    let logical = crate::utils::path_to_git_format(relative);
    let mut metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
      return Err(RailError::message(format!(
        "compiler output parent '{}' is not a real directory",
        directory.display()
      )));
    }
    normalize_output_mode(directory, &metadata, true)?;
    metadata = fs::symlink_metadata(directory)?;
    entries.entry(logical).or_insert_with(|| OutputEntryKind::Directory {
      mode: output_mode(&metadata),
    });
    current = directory.parent();
  }
  Ok(())
}

#[cfg(unix)]
fn normalize_output_mode(path: &Path, metadata: &fs::Metadata, directory: bool) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let executable = metadata.permissions().mode() & 0o111 != 0;
  let mode = if directory || executable { 0o755 } else { 0o644 };
  let mut permissions = metadata.permissions();
  permissions.set_mode(mode);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[cfg(windows)]
fn normalize_output_mode(path: &Path, metadata: &fs::Metadata, _directory: bool) -> RailResult<()> {
  if metadata.permissions().readonly() {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
  }
  Ok(())
}

#[cfg(not(any(unix, windows)))]
fn normalize_output_mode(_path: &Path, _metadata: &fs::Metadata, _directory: bool) -> RailResult<()> {
  Ok(())
}

fn digest_and_scan_file(path: &Path, patterns: &[Vec<u8>]) -> RailResult<(ContentDigest, u64, bool)> {
  let mut file = File::open(path)
    .map_err(|error| RailError::message(format!("failed to read compiler output '{}': {error}", path.display())))?;
  let mut hasher = Sha256::new();
  let mut bytes = 0u64;
  let mut found = false;
  let max_pattern = patterns.iter().map(Vec::len).max().unwrap_or(0);
  let mut carry = Vec::new();
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let read = file
      .read(&mut buffer)
      .map_err(|error| RailError::message(format!("failed to read compiler output '{}': {error}", path.display())))?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    bytes = bytes.saturating_add(read as u64);
    if !found && !patterns.is_empty() {
      carry.extend_from_slice(&buffer[..read]);
      found = patterns
        .iter()
        .any(|pattern| !pattern.is_empty() && contains_bytes(&carry, pattern));
      if !found && max_pattern > 1 && carry.len() >= max_pattern {
        let retain = max_pattern - 1;
        carry.drain(..carry.len() - retain);
      } else if !found && max_pattern <= 1 {
        carry.clear();
      }
    }
  }
  crate::instrumentation::record_hash(bytes as usize);
  crate::instrumentation::record_hashed_file_bytes_read(bytes as usize);
  Ok((ContentDigest::from_sha256_bytes(hasher.finalize().into()), bytes, found))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn output_manifest_digest(entries: &[OutputEntry]) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-output-manifest\0");
  identity.frame(b"version", &OUTPUT_MANIFEST_VERSION.to_le_bytes());
  for entry in entries {
    identity.frame(b"entry", &serde_json::to_vec(entry)?);
  }
  Ok(format!(
    "output-manifest-v{OUTPUT_MANIFEST_VERSION}-sha256-{}",
    identity.finish()
  ))
}

#[cfg(unix)]
fn output_mode(metadata: &fs::Metadata) -> u32 {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o7777
}

#[cfg(windows)]
fn output_mode(metadata: &fs::Metadata) -> u32 {
  if metadata.is_dir() { 0o755 } else { 0o644 }
}

#[cfg(not(any(unix, windows)))]
fn output_mode(metadata: &fs::Metadata) -> u32 {
  u32::from(metadata.permissions().readonly())
}

pub(crate) fn pre_context_lookup_key(workspace_root: &Path) -> RailResult<String> {
  let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  let mut identity = FramedHasher::new(b"cargo-rail-local-action-lookup\0");
  identity.frame(b"version", &FAST_CACHE_VALIDATION_VERSION.to_le_bytes());
  identity.frame(b"profile", &HERMETIC_PROFILE_VERSION.to_le_bytes());
  identity.frame(b"request", b"explicit-all-build");
  identity.frame(b"family", std::env::consts::FAMILY.as_bytes());
  identity.frame(b"os", std::env::consts::OS.as_bytes());
  identity.frame(b"arch", std::env::consts::ARCH.as_bytes());
  for (relative, required) in [
    ("Cargo.toml", true),
    ("Cargo.lock", true),
    (".cargo/config.toml", false),
    (".cargo/config", false),
    ("rust-toolchain.toml", false),
    ("rust-toolchain", false),
  ] {
    frame_lookup_file(&mut identity, relative, &workspace_root.join(relative), required)?;
  }
  Ok(format!("local-lookup-v1-sha256-{}", identity.finish()))
}

fn frame_lookup_file(identity: &mut FramedHasher, label: &str, path: &Path, required: bool) -> RailResult<()> {
  let before = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
      identity.frame(label.as_bytes(), b"absent");
      return Ok(());
    }
    Err(error) => {
      return Err(RailError::message(format!(
        "process-free cache lookup requires '{}': {error}",
        path.display()
      )));
    }
  };
  if !before.is_file() || before.file_type().is_symlink() || before.len() > MAX_DEP_INFO_BYTES {
    return Err(RailError::message(format!(
      "process-free cache lookup input '{}' is not a bounded regular file",
      path.display()
    )));
  }
  let bytes = fs::read(path)?;
  let after = fs::symlink_metadata(path)?;
  if bytes.len() as u64 != before.len() || validation_metadata(&before) != validation_metadata(&after) {
    return Err(RailError::message(format!(
      "process-free cache lookup input '{}' changed while it was read",
      path.display()
    )));
  }
  crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
  let mut framed = Vec::with_capacity(8 + bytes.len());
  framed.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
  framed.extend_from_slice(&bytes);
  identity.frame(label.as_bytes(), &framed);
  Ok(())
}

#[cfg(test)]
fn test_pre_context_lookup_key() -> String {
  let mut identity = FramedHasher::new(b"cargo-rail-local-action-lookup\0");
  identity.frame(b"version", &FAST_CACHE_VALIDATION_VERSION.to_le_bytes());
  identity.frame(b"test", b"fixture");
  format!("local-lookup-v1-sha256-{}", identity.finish())
}

fn validate_sha256_identity(identity: &str) -> RailResult<()> {
  let hex = identity
    .strip_prefix("sha256:")
    .ok_or_else(|| RailError::message("validation identity has the wrong domain"))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(RailError::message("validation identity is not canonical SHA-256"));
  }
  Ok(())
}

fn canonical_identity(identity: &str, prefix: &str) -> bool {
  identity.strip_prefix(prefix).is_some_and(|hex| {
    hex.len() == 64
      && hex
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  })
}

fn validate_relative_prefix(prefix: &str, allow_empty: bool) -> RailResult<()> {
  if prefix.is_empty() && allow_empty {
    return Ok(());
  }
  let path = crate::source::RepositoryPath::new(Path::new(prefix))?;
  if path.as_str() != prefix {
    return Err(RailError::message("validation path prefix is not canonical"));
  }
  Ok(())
}

fn sha256_identity(bytes: &[u8]) -> String {
  format!("sha256:{}", ContentDigest::sha256(bytes))
}

fn cargo_config_identity(config: &CargoConfigSnapshot, source_root: &Path) -> RailResult<String> {
  Ok(sha256_identity(&config.portable_snapshot_identity(source_root)?))
}

fn cargo_environment_identity(environment: &[(String, OsString, String)]) -> RailResult<String> {
  let portable = environment
    .iter()
    .map(|(name, _, identity)| (name, identity))
    .collect::<Vec<_>>();
  Ok(sha256_identity(&serde_json::to_vec(&portable)?))
}

#[cfg(target_os = "macos")]
fn validation_cargo_environment(
  config: &CargoConfigSnapshot,
  source_root: &Path,
) -> RailResult<Vec<(String, OsString, String)>> {
  config.materialized_environment(source_root, Path::new("/workspace"))
}

fn toolchain_selector_identity(source_root: &Path, current_dir: &Path) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-fast-toolchain-selector\0");
  identity.frame(b"version", &FAST_CACHE_VALIDATION_VERSION.to_le_bytes());
  let selected_rustc = ExecutableIdentity::capture(OsStr::new("rustc"), current_dir, source_root)?;
  identity.frame(b"rustc-selector-executable", &selected_rustc.identity_bytes()?);
  for name in [
    "PATH",
    "HOME",
    "USERPROFILE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
  ] {
    let mut binding = Vec::new();
    binding.extend_from_slice(&(name.len() as u64).to_le_bytes());
    binding.extend_from_slice(name.as_bytes());
    match std::env::var_os(name) {
      Some(value) => {
        binding.push(1);
        binding.extend_from_slice(&(value.as_encoded_bytes().len() as u64).to_le_bytes());
        binding.extend_from_slice(value.as_encoded_bytes());
      }
      None => binding.push(0),
    }
    identity.frame(b"environment", &binding);
  }

  let mut selector_files = Vec::new();
  for directory in current_dir.ancestors() {
    for name in ["rust-toolchain.toml", "rust-toolchain"] {
      let path = directory.join(name);
      if fs::symlink_metadata(&path).is_ok() {
        selector_files.push(path);
      }
    }
  }
  selector_files.sort();
  selector_files.dedup();
  for path in selector_files {
    frame_selector_file(&mut identity, source_root, &path, false)?;
  }

  let rustup_home = std::env::var_os("RUSTUP_HOME").map(PathBuf::from).or_else(|| {
    std::env::var_os("HOME")
      .or_else(|| std::env::var_os("USERPROFILE"))
      .map(|home| PathBuf::from(home).join(".rustup"))
  });
  if let Some(rustup_home) = rustup_home {
    if !rustup_home.is_absolute() {
      return Err(RailError::message(
        "relative RUSTUP_HOME is not eligible for process-free cache validation",
      ));
    }
    let settings = rustup_home.join("settings.toml");
    match fs::symlink_metadata(&settings) {
      Ok(_) => frame_selector_file(&mut identity, source_root, &settings, true)?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => identity.frame(b"rustup-settings", b"absent"),
      Err(error) => return Err(error.into()),
    }
  } else {
    identity.frame(b"rustup-settings", b"unavailable");
  }
  Ok(format!("sha256:{}", identity.finish()))
}

fn frame_selector_file(
  identity: &mut FramedHasher,
  source_root: &Path,
  path: &Path,
  rustup_settings: bool,
) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
    return Err(RailError::message(format!(
      "toolchain selector '{}' is not a bounded regular file",
      path.display()
    )));
  }
  let bytes = fs::read(path)?;
  let after = fs::symlink_metadata(path)?;
  if validation_metadata(&metadata) != validation_metadata(&after) || bytes.len() as u64 != metadata.len() {
    return Err(RailError::message(format!(
      "toolchain selector '{}' changed while it was read",
      path.display()
    )));
  }
  if rustup_settings {
    let text = std::str::from_utf8(&bytes).map_err(|_| RailError::message("rustup settings are not valid UTF-8"))?;
    let settings: serde_json::Value = toml_edit::de::from_str(text)
      .map_err(|_| RailError::message("rustup settings could not be parsed for cache validation"))?;
    if settings
      .get("overrides")
      .and_then(serde_json::Value::as_object)
      .is_some_and(|overrides| !overrides.is_empty())
    {
      return Err(RailError::message(
        "rustup directory overrides are not eligible for process-free cache validation",
      ));
    }
  }
  let label = if let Ok(relative) = path.strip_prefix(source_root) {
    format!("repository:{}", crate::utils::path_to_git_format(relative))
  } else {
    path
      .to_str()
      .ok_or_else(|| RailError::message("toolchain selector path is not valid UTF-8"))?
      .to_string()
  };
  let mut framed = Vec::new();
  framed.extend_from_slice(&(label.len() as u64).to_le_bytes());
  framed.extend_from_slice(label.as_bytes());
  framed.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
  framed.extend_from_slice(&bytes);
  identity.frame(
    if rustup_settings {
      b"rustup-settings"
    } else {
      b"toolchain-file"
    },
    &framed,
  );
  Ok(())
}

#[cfg(unix)]
fn validation_metadata(metadata: &fs::Metadata) -> (u64, u64, u32, u64, u64, i64, i64, i64, i64) {
  use std::os::unix::fs::MetadataExt as _;
  (
    metadata.dev(),
    metadata.ino(),
    metadata.mode(),
    metadata.nlink(),
    metadata.size(),
    metadata.mtime(),
    metadata.mtime_nsec(),
    metadata.ctime(),
    metadata.ctime_nsec(),
  )
}

#[cfg(not(unix))]
fn validation_metadata(metadata: &fs::Metadata) -> (u64, bool, bool) {
  (
    metadata.len(),
    metadata.permissions().readonly(),
    metadata.file_type().is_symlink(),
  )
}

#[cfg(target_os = "macos")]
fn declared_inputs_match(
  manifest: &DeclaredInputManifest,
  snapshot: &SourceSnapshot,
  source_root: &Path,
) -> RailResult<Result<(), String>> {
  manifest.validate()?;
  if manifest.entries.len() > MAX_OUTPUT_ENTRIES {
    return Err(RailError::message(
      "declared-input validation manifest exceeds the entry bound",
    ));
  }
  let roots = manifest
    .roots
    .iter()
    .map(|root| {
      Ok(
        crate::source::RepositoryPath::new(Path::new(root))?
          .as_path()
          .to_path_buf(),
      )
    })
    .collect::<RailResult<Vec<_>>>()?;
  let selected = |path: &Path| manifest.all_source || roots.iter().any(|root| path == root || path.starts_with(root));
  let expected = manifest
    .entries
    .iter()
    .map(|entry| (entry.path.as_str(), entry))
    .collect::<BTreeMap<_, _>>();
  let mut current = BTreeMap::<String, DeclaredInputEntry>::new();
  for entry in snapshot.tree().entries() {
    if !selected(entry.path.as_path()) {
      continue;
    }
    let path = entry.path.as_str().to_string();
    let kind = match &entry.kind {
      SourceEntryKind::RegularFile { digest, executable } => DeclaredInputKind::File {
        digest: format!("sha256:{digest}"),
        executable: *executable,
      },
      SourceEntryKind::Symlink { target } => DeclaredInputKind::Symlink { target: target.clone() },
      SourceEntryKind::Deleted => {
        return Err(RailError::message(
          "current source snapshot contains a deleted final entry",
        ));
      }
    };
    current.insert(
      path.clone(),
      DeclaredInputEntry {
        authoritative_file: expected
          .get(path.as_str())
          .is_some_and(|entry| entry.authoritative_file),
        path,
        kind,
      },
    );
  }
  for entry in manifest.entries.iter().filter(|entry| entry.authoritative_file) {
    if !current.contains_key(&entry.path) {
      current.insert(entry.path.clone(), capture_authoritative_input(source_root, entry)?);
    }
  }
  let current = current.into_values().collect::<Vec<_>>();
  if current == manifest.entries {
    return Ok(Ok(()));
  }
  let mut index = 0usize;
  while index < current.len() && index < manifest.entries.len() && current[index] == manifest.entries[index] {
    index += 1;
  }
  let reason = match (current.get(index), manifest.entries.get(index)) {
    (Some(actual), Some(expected)) if actual.path == expected.path => {
      format!("declared_input_changed:{}", expected.path)
    }
    (Some(actual), Some(expected)) if actual.path < expected.path => {
      format!("declared_input_added:{}", actual.path)
    }
    (Some(_), Some(expected)) => format!("declared_input_removed:{}", expected.path),
    (Some(actual), None) => format!("declared_input_added:{}", actual.path),
    (None, Some(expected)) => format!("declared_input_removed:{}", expected.path),
    (None, None) => "declared_input_root_changed".to_string(),
  };
  Ok(Err(reason))
}

#[cfg(target_os = "macos")]
fn capture_authoritative_input(source_root: &Path, expected: &DeclaredInputEntry) -> RailResult<DeclaredInputEntry> {
  let logical = crate::source::RepositoryPath::new(Path::new(&expected.path))?;
  let path = source_root.join(logical.as_path());
  let before = fs::symlink_metadata(&path).map_err(|error| {
    RailError::message(format!(
      "declared authoritative input '{}' is unavailable: {error}",
      expected.path
    ))
  })?;
  let kind = if before.file_type().is_symlink() {
    let target = fs::read_link(&path)?;
    let target = target
      .to_str()
      .ok_or_else(|| RailError::message("declared authoritative symlink target is not valid UTF-8"))?;
    DeclaredInputKind::Symlink {
      target: target.to_string(),
    }
  } else if before.is_file() {
    let bytes = fs::read(&path)?;
    crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
    DeclaredInputKind::File {
      digest: format!("sha256:{}", ContentDigest::sha256(&bytes)),
      executable: source_executable(&before),
    }
  } else {
    return Err(RailError::message(format!(
      "declared authoritative input '{}' has an unsupported file type",
      expected.path
    )));
  };
  let after = fs::symlink_metadata(&path)?;
  if validation_metadata(&before) != validation_metadata(&after) {
    return Err(RailError::message(format!(
      "declared authoritative input '{}' changed during validation",
      expected.path
    )));
  }
  Ok(DeclaredInputEntry {
    path: expected.path.clone(),
    authoritative_file: true,
    kind,
  })
}

#[cfg(all(target_os = "macos", unix))]
fn source_executable(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;
  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(all(target_os = "macos", not(unix)))]
fn source_executable(_metadata: &fs::Metadata) -> bool {
  false
}

fn logical_prefix(root: &Path, path: &Path, description: &str) -> RailResult<String> {
  let relative = path.strip_prefix(root).map_err(|_| {
    RailError::message(format!(
      "{description} '{}' is outside source root '{}'",
      path.display(),
      root.display()
    ))
  })?;
  if relative.as_os_str().is_empty() {
    return Ok(String::new());
  }
  let relative = crate::source::RepositoryPath::new(relative)?;
  Ok(relative.as_str().to_string())
}

struct FastCacheValidationRequest<'a> {
  ctx: &'a WorkspaceContext,
  action: &'a ExpandedAction,
  snapshot: &'a WorkspaceSnapshot,
  lookup_key: &'a str,
  action_key: &'a str,
  platform: &'a PlatformBoundary,
  cargo_environment: &'a [(String, OsString, String)],
  inventory: &'a FetchInventory,
}

fn build_fast_cache_validation(request: FastCacheValidationRequest<'_>) -> RailResult<FastCacheValidation> {
  let FastCacheValidationRequest {
    ctx,
    action,
    snapshot,
    lookup_key,
    action_key,
    platform,
    cargo_environment,
    inventory,
  } = request;
  cas::validate_fast_identity(action_key, lookup_key)?;
  let git = SystemGit::open(snapshot.source_root())?;
  if git.worktree_root != snapshot.source_root() {
    return Err(RailError::message(
      "process-free cache validation requires one canonical Git source root",
    ));
  }
  let workspace_prefix = logical_prefix(snapshot.source_root(), ctx.workspace_root(), "workspace root")?;
  let cargo_current_directory = logical_prefix(
    snapshot.source_root(),
    snapshot.cargo_current_dir(),
    "Cargo current directory",
  )?;
  let rustc_sysroot = snapshot
    .toolchain()
    .rustc_sysroot()
    .to_str()
    .filter(|path| !path.is_empty())
    .map(str::to_string)
    .ok_or_else(|| RailError::message("rustc sysroot is not canonical UTF-8"))?;
  let validation = FastCacheValidation {
    version: FAST_CACHE_VALIDATION_VERSION,
    lookup_key: lookup_key.to_string(),
    action_key: action_key.to_string(),
    workspace_prefix,
    cargo_current_directory,
    declared_inputs: declared_input_manifest(action, snapshot)?,
    cargo_config_identity: cargo_config_identity(snapshot.cargo_config(), snapshot.source_root())?,
    cargo_environment_identity: cargo_environment_identity(cargo_environment)?,
    toolchain_selector_identity: toolchain_selector_identity(snapshot.source_root(), snapshot.cargo_current_dir())?,
    rustc_sysroot,
    host_target: snapshot.toolchain().host_target().to_string(),
    platform_identity: platform.identity.clone(),
    inventory_result_digest: inventory.summary.result_digest.clone(),
    inventory_packages: inventory.packages.clone(),
    fetch: FetchSummary {
      reused: false,
      ..inventory.summary.clone()
    },
    selected_packages: action.selected_packages().to_vec(),
    argv: action.argv().to_vec(),
  };
  validation.validate_object()?;
  Ok(validation)
}

#[cfg(target_os = "macos")]
fn current_cargo_directory(source_root: &Path, requested_workspace: &Path) -> RailResult<PathBuf> {
  let current = std::env::current_dir()
    .map_err(|error| RailError::message(format!("failed to determine current directory: {error}")))?;
  let current = crate::utils::canonicalize_existing(&current)?;
  if current.starts_with(source_root) {
    Ok(current)
  } else {
    Ok(requested_workspace.to_path_buf())
  }
}

#[cfg(target_os = "macos")]
struct FastCacheRequestGuard {
  requested_workspace: PathBuf,
  source_root: PathBuf,
  cargo_current_directory: PathBuf,
  git: SystemGit,
  source: GitWorktreeCapture,
  cargo_config_identity: String,
  cargo_environment_identity: String,
  toolchain_selector_identity: String,
  lookup_key: String,
}

#[cfg(target_os = "macos")]
impl FastCacheRequestGuard {
  fn capture(requested_workspace: &Path) -> RailResult<Self> {
    let requested_workspace = crate::utils::canonicalize_existing(requested_workspace)?;
    let git = SystemGit::open(&requested_workspace)?;
    let source_root = git.worktree_root.clone();
    let cargo_current_directory = current_cargo_directory(&source_root, &requested_workspace)?;
    let source =
      GitWorktreeCapture::capture_excluding(&git, &[crate::workspace::cargo_rail_state_root(&requested_workspace)])?;
    let config = CargoConfigSnapshot::capture(&cargo_current_directory)?;
    let mut configuration_reasons = BTreeSet::new();
    classify_cargo_configuration(&config, &mut configuration_reasons);
    for name in ["RUSTC", "RUSTDOC"] {
      if std::env::var_os(name).is_some() {
        configuration_reasons.insert("cargo_tool_override_not_graduated".to_string());
      }
    }
    ensure_execution_semantics_supported(&configuration_reasons)?;
    let cargo_config_identity = cargo_config_identity(&config, &source_root)?;
    let cargo_environment_identity = cargo_environment_identity(&validation_cargo_environment(&config, &source_root)?)?;
    let toolchain_selector_identity = toolchain_selector_identity(&source_root, &cargo_current_directory)?;
    let lookup_key = pre_context_lookup_key(&requested_workspace)?;
    Ok(Self {
      requested_workspace,
      source_root,
      cargo_current_directory,
      git,
      source,
      cargo_config_identity,
      cargo_environment_identity,
      toolchain_selector_identity,
      lookup_key,
    })
  }

  fn candidate_matches(
    &self,
    candidate_action_key: &str,
    validation: &FastCacheValidation,
  ) -> RailResult<Result<(), String>> {
    validation.validate_object()?;
    if candidate_action_key != validation.action_key {
      return Err(RailError::message(
        "local cache candidate action key does not match its validation manifest",
      ));
    }
    let expected_workspace = if validation.workspace_prefix.is_empty() {
      self.source_root.clone()
    } else {
      self.source_root.join(&validation.workspace_prefix)
    };
    let expected_workspace = crate::utils::canonicalize_existing(&expected_workspace)?;
    if self.requested_workspace != expected_workspace {
      return Ok(Err("workspace_prefix_changed".to_string()));
    }
    if logical_prefix(
      &self.source_root,
      &self.cargo_current_directory,
      "Cargo current directory",
    )? != validation.cargo_current_directory
    {
      return Ok(Err("cargo_current_directory_changed".to_string()));
    }
    if let Err(reason) = declared_inputs_match(&validation.declared_inputs, self.source.snapshot(), &self.source_root)?
    {
      return Ok(Err(reason));
    }
    if self.cargo_config_identity != validation.cargo_config_identity {
      return Ok(Err("cargo_configuration_changed".to_string()));
    }
    if self.cargo_environment_identity != validation.cargo_environment_identity {
      return Ok(Err("cargo_environment_changed".to_string()));
    }
    if self.toolchain_selector_identity != validation.toolchain_selector_identity {
      return Ok(Err("toolchain_selection_changed".to_string()));
    }
    if self.lookup_key != validation.lookup_key {
      return Ok(Err("lookup_input_changed".to_string()));
    }
    Ok(Ok(()))
  }

  fn validate_after_restore(&self, validation: &FastCacheValidation) -> RailResult<Result<(), String>> {
    if fast_platform_identity(
      Path::new(&validation.rustc_sysroot),
      &validation.host_target,
      &self.source_root,
    )? != validation.platform_identity
    {
      return Ok(Err("platform_or_toolchain_changed".to_string()));
    }
    let final_config = CargoConfigSnapshot::capture(&self.cargo_current_directory)?;
    if cargo_config_identity(&final_config, &self.source_root)? != validation.cargo_config_identity
      || cargo_environment_identity(&validation_cargo_environment(&final_config, &self.source_root)?)?
        != validation.cargo_environment_identity
    {
      return Ok(Err("cargo_configuration_changed_during_validation".to_string()));
    }
    if toolchain_selector_identity(&self.source_root, &self.cargo_current_directory)?
      != validation.toolchain_selector_identity
    {
      return Ok(Err("toolchain_selection_changed_during_validation".to_string()));
    }
    if pre_context_lookup_key(&self.requested_workspace)? != validation.lookup_key {
      return Ok(Err("lookup_input_changed".to_string()));
    }
    if let Err(reason) = declared_inputs_match(&validation.declared_inputs, self.source.snapshot(), &self.source_root)?
    {
      return Ok(Err(reason));
    }
    self.source.validate_unchanged(&self.git)?;
    Ok(Ok(()))
  }
}

#[cfg(target_os = "macos")]
fn record_cache_candidate_miss(first: &mut Option<String>, reason: String) {
  fn specificity(reason: &str) -> u8 {
    if matches!(reason, "workspace_prefix_changed" | "cargo_current_directory_changed") {
      0
    } else if reason.starts_with("declared_input_") {
      1
    } else {
      2
    }
  }

  if first
    .as_deref()
    .is_none_or(|current| specificity(&reason) > specificity(current))
  {
    *first = Some(reason);
  }
}

/// Restore a verified action result before any Cargo or rustc process is started.
pub(crate) fn try_restore_pre_context(workspace_root: &Path) -> RailResult<PreContextCacheAttempt> {
  #[cfg(not(target_os = "macos"))]
  {
    let _ = workspace_root;
    Ok(PreContextCacheAttempt::Miss(
      "platform_process_isolation_unavailable".to_string(),
    ))
  }
  #[cfg(target_os = "macos")]
  {
    let started = Instant::now();
    let _workspace_cache_lock = crate::cache::lock_workspace(workspace_root)?;
    reclaim_workspace_state(workspace_root)?;
    let lookup_key = match pre_context_lookup_key(workspace_root) {
      Ok(lookup_key) => lookup_key,
      Err(_) => {
        return Ok(PreContextCacheAttempt::Miss("lookup_inputs_unavailable".to_string()));
      }
    };
    let local_cas = match cas::LocalCas::open() {
      Ok(local_cas) => local_cas,
      Err(_) => {
        return Ok(PreContextCacheAttempt::Miss("cache_root_unavailable".to_string()));
      }
    };
    let candidates = local_cas.candidates(&lookup_key).map_err(|error| {
      RailError::with_help(
        format!("local action cache candidate verification failed: {error}"),
        "run with --no-cache for an explicit cold execution, or clean the validated local cache",
      )
    })?;
    if candidates.is_empty() {
      return Ok(PreContextCacheAttempt::Miss("action_not_found".to_string()));
    }
    let request = FastCacheRequestGuard::capture(workspace_root)?;
    if request.lookup_key != lookup_key {
      return Ok(PreContextCacheAttempt::Miss("lookup_input_changed".to_string()));
    }
    let mut first_miss = None;
    for candidate in candidates {
      match request.candidate_matches(&candidate.action_key, &candidate.validation)? {
        Ok(()) => {}
        Err(reason) => {
          record_cache_candidate_miss(&mut first_miss, reason);
          continue;
        }
      }
      let parent = create_state_directory(workspace_root, &["materializations"])?;
      prune_children(&parent, None, MATERIALIZATIONS_MAX_BYTES, usize::MAX)?;
      let temporary = tempfile::Builder::new()
        .prefix("result-")
        .tempdir_in(&parent)
        .map_err(|error| RailError::message(format!("failed to create declared output root: {error}")))?;
      let destination = temporary.path().join("output");
      let hit = match local_cas.restore(&candidate.action_key, &destination) {
        cas::CacheLookup::Hit(hit) => hit,
        cas::CacheLookup::Miss(miss) if miss.kind == cas::CacheMissKind::Miss => {
          record_cache_candidate_miss(&mut first_miss, miss.reason);
          continue;
        }
        cas::CacheLookup::Miss(miss) => {
          return Err(RailError::with_help(
            format!(
              "local action cache {}: {} (verified_objects={}, bytes_read={})",
              match miss.kind {
                cas::CacheMissKind::Miss => "miss",
                cas::CacheMissKind::Corrupt => "corrupt",
                cas::CacheMissKind::Incompatible => "incompatible",
              },
              miss.reason,
              miss.objects_verified,
              miss.bytes_read,
            ),
            "run with --no-cache for an explicit cold execution, or clean the validated local cache",
          ));
        }
      };
      match request.validate_after_restore(&candidate.validation)? {
        Ok(()) => {}
        Err(reason) => {
          record_cache_candidate_miss(&mut first_miss, reason);
          continue;
        }
      }
      if !destination.starts_with(temporary.path()) {
        return Err(RailError::message(
          "local action output escaped its private materialization root",
        ));
      }
      ensure_tree_bytes(
        &destination,
        MATERIALIZATIONS_MAX_BYTES,
        "restored hermetic materialization",
      )?;
      let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
      let materialized_root = destination
        .strip_prefix(&workspace_root)
        .map_err(|_| RailError::message("materialized local action output escaped the workspace"))?;
      let mut fetch = candidate.validation.fetch.clone();
      fetch.reused = true;
      let cache = LocalCacheSummary {
        version: 1,
        status: LocalCacheStatus::Hit,
        reason: "verified_action_result_restored_pre_context".to_string(),
        action_result: Some(hit.action_result),
        objects_verified: hit.objects_verified,
        bytes_read: hit.bytes_read,
        bytes_restored: hit.bytes_restored,
        objects_written: 0,
        bytes_written: 0,
        stored: false,
        cargo_check_executed: false,
        compiler_units_executed: false,
      };
      let report = HermeticExecutionReport {
        version: RESULT_VERSION,
        profile_version: HERMETIC_PROFILE_VERSION,
        action_id: "build".to_string(),
        action_class: "cargo_check".to_string(),
        support: HermeticSupport::Eligible,
        enforcement: EnforcementLevel::FilesystemAndNetwork,
        action_key: Some(candidate.action_key),
        result_digest: Some(hit.result_digest),
        fetch,
        output_manifest: Some(hit.output_manifest),
        materialized_root: Some(crate::utils::path_to_git_format(materialized_root)),
        cache,
        compiler_units: hit.compiler_units,
        elapsed_ms: started.elapsed().as_millis(),
        reasons: BTreeSet::new(),
      };
      let report_path = write_report(&workspace_root, &report)?;
      let persisted_parent = temporary.keep();
      if !destination.starts_with(&persisted_parent) {
        return Err(RailError::message(
          "persisted local action output escaped its declared root",
        ));
      }
      prune_children(&parent, Some(&persisted_parent), MATERIALIZATIONS_MAX_BYTES, usize::MAX)?;
      return Ok(PreContextCacheAttempt::Hit(Box::new(PreContextCacheHit {
        report,
        report_path,
        selected_packages: candidate.validation.selected_packages,
        argv: candidate.validation.argv,
      })));
    }
    Ok(PreContextCacheAttempt::Miss(
      first_miss.unwrap_or_else(|| "validated_candidate_not_found".to_string()),
    ))
  }
}

/// Execute one supported Rust action in a fresh root and publish its proof.
pub(crate) fn execute(
  ctx: &WorkspaceContext,
  action: &ExpandedAction,
  cache_enabled: bool,
  lookup_key: Option<&str>,
) -> RailResult<(HermeticExecutionReport, PathBuf)> {
  if action.kind() != ActionKind::Build || !cargo_check_action(action.argv()) {
    return Err(RailError::with_help(
      format!(
        "hermetic execution does not yet support action '{}' ({})",
        action.id(),
        action.kind().as_str()
      ),
      "use the built-in build action (cargo check); other action classes remain explicitly uncacheable",
    ));
  }

  let started = Instant::now();
  let snapshot = ctx.snapshot()?;
  ctx.validate_snapshot_unchanged()?;
  let mut reasons = classify_action(action, snapshot)?;
  ensure_execution_semantics_supported(&reasons)?;
  let inventory = ctx
    .hermetic_fetch_inventory()
    .cloned()
    .ok_or_else(|| RailError::message("hermetic execution context has no explicit fetch inventory"))?;
  inventory.validate_unchanged()?;
  ctx.validate_snapshot_unchanged()?;
  let layout = RunLayout::create(ctx)?;
  materialize_runtime_cargo_home(&layout, &inventory, snapshot)?;
  materialize_source(snapshot, &layout.workspace)?;
  ctx.validate_snapshot_unchanged()?;

  let cargo_environment = hermetic_cargo_environment(snapshot, &layout)?;
  let platform = PlatformBoundary::capture(snapshot, &layout, &inventory)?;
  if platform.enforcement != EnforcementLevel::FilesystemAndNetwork {
    reasons.insert("os_network_and_filesystem_enforcement_unavailable".to_string());
  }

  let preliminary_action_key = reasons
    .is_empty()
    .then(|| {
      hermetic_action_key(
        action,
        snapshot,
        &inventory.summary.result_digest,
        &platform.identity,
        &cargo_environment,
      )
    })
    .transpose()?;
  if let Some(lookup_key) = lookup_key
    && !canonical_identity(lookup_key, "local-lookup-v1-sha256-")
  {
    return Err(RailError::message(
      "local action lookup key has the wrong request domain",
    ));
  }
  let mut local_cas = None;
  let mut cache = if !cache_enabled {
    LocalCacheSummary::new(LocalCacheStatus::Disabled, "disabled_by_request")
  } else if lookup_key.is_none() {
    LocalCacheSummary::new(LocalCacheStatus::Uncacheable, "pre_context_request_not_graduated")
  } else if preliminary_action_key.is_some() {
    match cas::LocalCas::open() {
      Ok(cas) => {
        local_cas = Some(cas);
        LocalCacheSummary::new(LocalCacheStatus::Miss, "validated_candidate_not_found")
      }
      Err(_) => LocalCacheSummary::new(LocalCacheStatus::Disabled, "cache_root_unavailable"),
    }
  } else {
    LocalCacheSummary::new(
      LocalCacheStatus::Uncacheable,
      reasons.iter().next().map_or("action_key_unavailable", String::as_str),
    )
  };

  crate::instrumentation::record_hermetic_cargo_execution();
  let command = run_cargo_check(action, snapshot, &layout, &inventory, &platform, &cargo_environment)?;
  if !command.status.success() {
    forward_cargo_output(&command, true);
    let diagnostic = if command.stderr.is_empty() {
      String::from_utf8_lossy(&command.stdout).trim().to_string()
    } else {
      String::from_utf8_lossy(&command.stderr).trim().to_string()
    };
    let diagnostic = if diagnostic.is_empty() {
      String::new()
    } else {
      format!(": {diagnostic}")
    };
    return Err(RailError::with_help(
      format!("hermetic Cargo check failed with {}{diagnostic}", command.status),
      "inspect the denied input/output and declare it, or run without --hermetic while the action remains uncacheable",
    ));
  }
  forward_cargo_output(&command, false);
  ctx.validate_snapshot_unchanged()?;

  let mut invocations = load_raw(&layout.observations)?;
  crate::instrumentation::record_hermetic_compiler_units(invocations.len());
  normalize_dep_info_outputs(&mut invocations, snapshot, &layout, &inventory)?;
  let cargo_outputs = capture_cargo_artifact_outputs(&command, &layout)?;
  if cargo_outputs.files.is_empty() {
    reasons.insert("cargo_artifact_outputs_unavailable".to_string());
  }
  let mut observed_crate_names = invocations
    .iter()
    .filter_map(|invocation| invocation.crate_name.clone())
    .collect::<Vec<_>>();
  observed_crate_names.sort();
  if observed_crate_names.len() != invocations.len() || observed_crate_names != cargo_outputs.crate_names {
    reasons.insert("compiler_observation_coverage_incomplete".to_string());
  }
  let output_manifest = capture_compiler_outputs(
    &layout.root,
    &invocations,
    &cargo_outputs.files,
    &[snapshot.source_root(), &layout.root, &inventory.cargo_home],
    &mut reasons,
  )?;
  validate_invocations(&invocations, &mut reasons);

  let support = if reasons.is_empty() {
    HermeticSupport::Eligible
  } else if reasons.len() == 1 && reasons.contains("os_network_and_filesystem_enforcement_unavailable") {
    HermeticSupport::PlatformLimited
  } else {
    HermeticSupport::Uncacheable
  };
  let action_key = if support == HermeticSupport::Eligible {
    preliminary_action_key
  } else {
    None
  };
  let result_digest = action_key
    .as_ref()
    .map(|action_key| hermetic_result_digest(action_key, output_manifest.digest()));
  ctx.validate_snapshot_unchanged()?;
  inventory.validate_unchanged()?;
  platform.validate_unchanged(snapshot)?;
  output_manifest.validate_unchanged(&layout.root)?;
  cache.cargo_check_executed = true;
  cache.compiler_units_executed = !invocations.is_empty();
  if support != HermeticSupport::Eligible && cache.status == LocalCacheStatus::Miss {
    cache.status = LocalCacheStatus::Uncacheable;
    cache.reason = reasons
      .iter()
      .next()
      .cloned()
      .unwrap_or_else(|| "post_execution_evidence_incomplete".to_string());
  }
  if support == HermeticSupport::Eligible
    && cache.status == LocalCacheStatus::Miss
    && let (Some(cas), Some(lookup_key), Some(action_key), Some(result_digest)) = (
      local_cas.as_ref(),
      lookup_key,
      action_key.as_deref(),
      result_digest.as_deref(),
    )
  {
    let stored = build_fast_cache_validation(FastCacheValidationRequest {
      ctx,
      action,
      snapshot,
      lookup_key,
      action_key,
      platform: &platform,
      cargo_environment: &cargo_environment,
      inventory: &inventory,
    })
    .and_then(|validation| {
      cas.store(cas::StoreRequest {
        action_key,
        lookup_key,
        result_digest,
        manifest: &output_manifest,
        validation: &validation,
        compiler_units: invocations.len(),
        source_root: &layout.root,
      })
    });
    match stored {
      Ok(stored) => {
        cache.action_result = stored.action_result;
        cache.objects_written = stored.objects_written;
        cache.bytes_written = stored.bytes_written;
        cache.stored = true;
      }
      Err(error) => {
        cache.reason = format!("miss_store_failed:{error}");
      }
    }
  }
  let report = HermeticExecutionReport {
    version: RESULT_VERSION,
    profile_version: HERMETIC_PROFILE_VERSION,
    action_id: action.id().to_string(),
    action_class: "cargo_check".to_string(),
    support,
    enforcement: platform.enforcement,
    action_key,
    result_digest,
    fetch: inventory.summary,
    output_manifest: Some(output_manifest),
    materialized_root: None,
    cache,
    compiler_units: invocations.len(),
    elapsed_ms: started.elapsed().as_millis(),
    reasons,
  };
  let report_path = write_report(ctx.workspace_root(), &report)?;
  Ok((report, report_path))
}

fn cargo_check_action(argv: &[String]) -> bool {
  argv
    .iter()
    .take_while(|argument| argument.as_str() != "--")
    .any(|argument| argument == "check")
}

fn classify_action(action: &ExpandedAction, snapshot: &WorkspaceSnapshot) -> RailResult<BTreeSet<String>> {
  let mut reasons = BTreeSet::new();
  let allowed_base_reasons = BTreeSet::from([
    "ambient_environment",
    "ambient_host",
    "ambient_outputs",
    "build_script_observations_unavailable",
    "cargo_units_unmodeled",
    "cargo_secret_capability",
    "executable_identity_unavailable",
    "executable_runtime_inputs_unavailable",
    "external_source_digest_unavailable",
    "platform_runtime_identity_incomplete",
    "proc_macro_observations_unavailable",
    "recursive_wrapper_chain",
  ]);
  let Some(analysis) = action.action_key() else {
    reasons.insert("action_key_analysis_unavailable".to_string());
    return Ok(reasons);
  };
  for reason in analysis.reason_codes() {
    if !allowed_base_reasons.contains(reason) {
      reasons.insert(reason.to_string());
    }
  }
  if action.resolution_views().iter().any(|view| view.has_build_scripts()) {
    reasons.insert("build_scripts_not_hermetic".to_string());
  }
  if action.resolution_views().iter().any(|view| view.has_proc_macros()) {
    reasons.insert("proc_macros_not_hermetic".to_string());
  }
  if action.selected_targets().len() > 1 {
    reasons.insert("multiple_compilation_targets_not_graduated".to_string());
  }
  let target = hermetic_target(action, snapshot)?;
  if matches!(
    target.specification(),
    crate::cargo::TargetSpecificationIdentity::Custom(_)
  ) {
    reasons.insert("custom_target_not_graduated".to_string());
  } else if !target.is_host() {
    reasons.insert("cross_target_pair_not_graduated".to_string());
  }
  if target.linker().is_some() {
    reasons.insert("configured_linker_not_graduated".to_string());
  }
  if target.runner().is_some() {
    reasons.insert("configured_runner_not_graduated".to_string());
  }
  if rustflags_escape_hermetic_contract(target.rustflags()) {
    reasons.insert("rustflags_not_graduated".to_string());
  }
  classify_cargo_configuration(snapshot.cargo_config(), &mut reasons);
  classify_cargo_arguments(action.argv(), &mut reasons);
  Ok(reasons)
}

fn rustflags_escape_hermetic_contract(flags: &[String]) -> bool {
  let mut index = 0usize;
  while index < flags.len() {
    let flag = flags[index].as_str();
    if flag.starts_with('@')
      || flag.starts_with("-L")
      || flag.starts_with("-l")
      || flag.starts_with("-Z")
      || matches!(
        flag.split_once('=').map_or(flag, |(name, _)| name),
        "--emit" | "--extern" | "--out-dir" | "--sysroot" | "--target" | "-o"
      )
    {
      return true;
    }
    let codegen = if flag == "-C" {
      index += 1;
      flags.get(index).map(String::as_str)
    } else {
      flag.strip_prefix("-C")
    };
    if let Some(codegen) = codegen {
      let (name, value) = codegen
        .split_once('=')
        .map_or((codegen, ""), |(name, value)| (name, value));
      if matches!(
        name,
        "codegen-backend"
          | "incremental"
          | "link-arg"
          | "link-args"
          | "linker"
          | "linker-flavor"
          | "llvm-args"
          | "profile-generate"
          | "profile-use"
          | "remark"
          | "save-temps"
      ) || (name == "target-cpu" && value == "native")
      {
        return true;
      }
    }
    index += 1;
  }
  false
}

fn classify_cargo_configuration(cargo_config: &CargoConfigSnapshot, reasons: &mut BTreeSet<String>) {
  let Some(settings) = cargo_config.effective_file_settings().as_object() else {
    reasons.insert("cargo_configuration_unmodeled".to_string());
    return;
  };
  if settings
    .keys()
    .any(|key| matches!(key.as_str(), "patch" | "paths" | "registries" | "registry" | "resolver"))
    || settings
      .get("source")
      .is_some_and(|source| !remote_registry_source_configuration(source))
  {
    reasons.insert("cargo_resolution_configuration_not_materialized".to_string());
  }
  if settings.contains_key("profile") {
    reasons.insert("cargo_profile_configuration_not_materialized".to_string());
  }
  if settings.contains_key("host") || settings.contains_key("unstable") {
    reasons.insert("cargo_configuration_unmodeled".to_string());
  }
  if let Some(build) = settings.get("build").and_then(serde_json::Value::as_object)
    && build.keys().any(|key| {
      matches!(
        key.as_str(),
        "rustc" | "rustc-wrapper" | "rustc-workspace-wrapper" | "rustdoc"
      )
    })
  {
    reasons.insert("cargo_tool_override_not_graduated".to_string());
  }
  if let Some(environment) = settings.get("env").and_then(serde_json::Value::as_object)
    && environment
      .keys()
      .any(|name| hermetic_environment_name_is_controlled(name))
  {
    reasons.insert("cargo_environment_boundary_override".to_string());
  }
}

fn remote_registry_source_configuration(source: &serde_json::Value) -> bool {
  let Some(sources) = source.as_object() else {
    return false;
  };
  for (name, settings) in sources {
    let Some(settings) = settings.as_object() else {
      return false;
    };
    if settings
      .keys()
      .any(|key| !matches!(key.as_str(), "registry" | "replace-with"))
      || settings
        .get("registry")
        .is_some_and(|registry| !remote_registry_url(registry))
      || settings
        .get("replace-with")
        .is_some_and(|replacement| replacement.as_str().is_none_or(str::is_empty))
    {
      return false;
    }

    let mut current = name.as_str();
    let mut seen = BTreeSet::new();
    loop {
      if !seen.insert(current) {
        return false;
      }
      let Some(current_settings) = sources.get(current).and_then(serde_json::Value::as_object) else {
        return false;
      };
      match current_settings.get("replace-with").and_then(serde_json::Value::as_str) {
        Some(replacement) => current = replacement,
        None => {
          if !current_settings.get("registry").is_some_and(remote_registry_url) {
            return false;
          }
          break;
        }
      }
    }
  }
  true
}

fn remote_registry_url(value: &serde_json::Value) -> bool {
  value.as_str().is_some_and(|registry| {
    ["http://", "https://", "sparse+http://", "sparse+https://"]
      .iter()
      .any(|prefix| registry.starts_with(prefix))
  })
}

fn hermetic_environment_name_is_controlled(name: &str) -> bool {
  matches!(
    name,
    "CARGO_HOME"
      | "CARGO_INCREMENTAL"
      | "CARGO_NET_OFFLINE"
      | "CARGO_RAIL_COMPILER_CACHE_WRAPPER"
      | "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY"
      | "CARGO_RAIL_COMPILER_OBSERVATION_ONLY"
      | "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT"
      | "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION"
      | "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION"
      | "CARGO_RAIL_NATIVE_COMPILER_CACHE_STORE"
      | "CARGO_RAIL_INNER_RUSTDOC"
      | "CARGO_RAIL_INNER_WORKSPACE_WRAPPER"
      | "CARGO_RAIL_RUSTC_WRAPPER"
      | "CARGO_RAIL_RUSTDOC_WRAPPER"
      | "CARGO_TARGET_DIR"
      | "DYLD_INSERT_LIBRARIES"
      | "DYLD_LIBRARY_PATH"
      | "HOME"
      | "LANG"
      | "LD_LIBRARY_PATH"
      | "LD_PRELOAD"
      | "LIBPATH"
      | "LC_ALL"
      | "OPENSSL_CONF"
      | "PATH"
      | "RUSTC"
      | "RUSTC_CODEGEN_BACKEND"
      | "RUSTC_WRAPPER"
      | "RUSTC_WORKSPACE_WRAPPER"
      | "SOURCE_DATE_EPOCH"
      | "SHLIB_PATH"
      | "TEMP"
      | "TMP"
      | "TMPDIR"
      | "TZ"
  ) || name.starts_with("CARGO_BUILD_")
    || name == "CARGO_ENCODED_RUSTFLAGS"
    || name == "RUSTFLAGS"
}

fn classify_cargo_arguments(argv: &[String], reasons: &mut BTreeSet<String>) {
  for argument in argv.iter().skip(1) {
    if argument == "--" {
      reasons.insert("cargo_rustc_arguments_not_graduated".to_string());
      break;
    }
    let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
    let short_option = ["-C", "-Z", "-m"]
      .iter()
      .copied()
      .find(|prefix| argument.starts_with(prefix) && !argument.starts_with("--"));
    if matches!(
      option,
      "--artifact-dir"
        | "--build-dir"
        | "--future-incompat-report"
        | "--out-dir"
        | "--target-dir"
        | "--timings"
        | "--unit-graph"
    ) {
      reasons.insert("cargo_output_contract_override_not_graduated".to_string());
    }
    if matches!(option, "--lockfile-path" | "--manifest-path") || matches!(short_option, Some("-C" | "-m")) {
      reasons.insert("cargo_workspace_contract_override_not_graduated".to_string());
    }
    if short_option == Some("-Z") {
      reasons.insert("cargo_unstable_arguments_not_graduated".to_string());
    }
  }
}

fn ensure_execution_semantics_supported(reasons: &BTreeSet<String>) -> RailResult<()> {
  const BLOCKERS: &[&str] = &[
    "absolute_workspace_argument",
    "build_scripts_not_hermetic",
    "cargo_cli_configuration_unmodeled",
    "cargo_configuration_unmodeled",
    "cargo_environment_boundary_override",
    "cargo_output_contract_override_not_graduated",
    "cargo_profile_configuration_not_materialized",
    "cargo_resolution_configuration_not_materialized",
    "cargo_rustc_arguments_not_graduated",
    "cargo_tool_override_not_graduated",
    "cargo_unstable_arguments_not_graduated",
    "cargo_workspace_contract_override_not_graduated",
    "configured_linker_not_graduated",
    "configured_runner_not_graduated",
    "cross_target_pair_not_graduated",
    "custom_target_not_graduated",
    "multiple_compilation_targets_not_graduated",
    "proc_macros_not_hermetic",
    "rustflags_not_graduated",
    "target_identity_unavailable",
  ];
  let blockers = reasons
    .iter()
    .filter(|reason| BLOCKERS.contains(&reason.as_str()))
    .cloned()
    .collect::<Vec<_>>();
  if blockers.is_empty() {
    return Ok(());
  }
  Err(RailError::with_help(
    format!("hermetic action is explicitly uncacheable: {}", blockers.join(", ")),
    "remove or model the listed override, or run without --hermetic; cargo-rail will not execute altered semantics",
  ))
}

fn hermetic_action_key(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  inventory_digest: &str,
  platform_identity: &str,
  cargo_environment: &[(String, OsString, String)],
) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-hermetic-action\0");
  identity.frame(b"version", &HERMETIC_PROFILE_VERSION.to_le_bytes());
  identity.frame(b"action-id", action.id().as_bytes());
  identity.frame(b"kind", action.kind().as_str().as_bytes());
  for argument in action.argv().iter().skip(1) {
    identity.frame(b"cargo-argument", argument.as_bytes());
  }
  match action.working_directory() {
    ActionWorkingDirectory::Workspace => identity.frame(b"working-directory", b"repository:"),
    ActionWorkingDirectory::Repository(path) => {
      identity.frame(b"working-directory", format!("repository:{}", path.as_str()).as_bytes());
    }
  }
  for package in action.selected_packages() {
    identity.frame(b"selected-package", package.as_bytes());
  }
  for target in action.selected_targets() {
    identity.frame(b"selected-target", target.as_bytes());
  }
  identity.frame(b"all-features", &[u8::from(action.selected_features().all_features())]);
  identity.frame(
    b"default-features",
    &[u8::from(action.selected_features().default_features())],
  );
  for feature in action.selected_features().named() {
    identity.frame(b"feature", feature.as_bytes());
  }
  let analysis = action
    .action_key()
    .ok_or_else(|| RailError::message("hermetic action has no declared input analysis"))?;
  identity.frame(b"declared-input-root", analysis.declared_input_root().as_bytes());
  for resolution in action.resolution_views() {
    identity.frame(b"resolution", resolution.resolution_digest().as_bytes());
  }
  identity.frame(
    b"target-identity",
    &hermetic_target(action, snapshot)?.portable_snapshot_identity(snapshot.source_root())?,
  );
  identity.frame(b"inventory", inventory_digest.as_bytes());
  identity.frame(b"platform", platform_identity.as_bytes());
  identity.frame(b"compiler-observer", b"global-rustc-wrapper;observation-only");
  for (name, _, value) in cargo_environment {
    let mut binding = Vec::new();
    binding.extend_from_slice(&(name.len() as u64).to_le_bytes());
    binding.extend_from_slice(name.as_bytes());
    binding.extend_from_slice(&(value.len() as u64).to_le_bytes());
    binding.extend_from_slice(value.as_bytes());
    identity.frame(b"cargo-environment", &binding);
  }
  identity.frame(
    b"environment",
    b"clear;PATH=/rust-toolchain/bin;LANG=C;LC_ALL=C;TZ=UTC;SOURCE_DATE_EPOCH=1;OPENSSL_CONF=/home/openssl.cnf",
  );
  identity.frame(b"workspace", b"/workspace");
  identity.frame(b"cargo-home", b"/cargo-home");
  identity.frame(b"target", b"/target");
  identity.frame(b"build", b"/build");
  identity.frame(b"incremental", b"disabled");
  identity.frame(b"lock", snapshot.lockfile_fingerprint().as_bytes());
  Ok(format!(
    "hermetic-action-v{HERMETIC_PROFILE_VERSION}-sha256-{}",
    identity.finish()
  ))
}

fn hermetic_result_digest(action_key: &str, output_manifest: &str) -> String {
  let mut identity = FramedHasher::new(b"cargo-rail-hermetic-result\0");
  identity.frame(b"version", &RESULT_VERSION.to_le_bytes());
  identity.frame(b"action", action_key.as_bytes());
  identity.frame(b"outputs", output_manifest.as_bytes());
  format!("result-v{RESULT_VERSION}-sha256-{}", identity.finish())
}

fn write_report(workspace_root: &Path, report: &HermeticExecutionReport) -> RailResult<PathBuf> {
  let directory = create_state_directory(workspace_root, &["reports"])?;
  prune_children(&directory, None, REPORTS_MAX_BYTES, MAX_REPORTS)?;
  let nonce = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_nanos())
    .unwrap_or_default();
  let path = directory.join(format!("{}-{nonce}.json", report.action_id));
  let bytes = serde_json::to_vec_pretty(report)?;
  if bytes.len() as u64 > REPORTS_MAX_BYTES {
    return Err(RailError::message(format!(
      "hermetic execution report exceeds its {REPORTS_MAX_BYTES}-byte bound"
    )));
  }
  crate::utils::write_file_atomic(&path, &bytes)?;
  prune_children(&directory, Some(&path), REPORTS_MAX_BYTES, MAX_REPORTS)?;
  Ok(path)
}

fn forward_cargo_output(output: &Output, include_stdout: bool) {
  if include_stdout {
    let _ = std::io::stdout().write_all(&output.stdout);
  }
  let _ = std::io::stderr().write_all(&output.stderr);
}

struct PlatformBoundary {
  enforcement: EnforcementLevel,
  identity: String,
  sandbox_program: Option<PathBuf>,
  sandbox_policy: Option<String>,
  #[cfg(target_os = "macos")]
  read_boundary_digest: Option<String>,
}

impl PlatformBoundary {
  fn validate_unchanged(&self, snapshot: &WorkspaceSnapshot) -> RailResult<()> {
    #[cfg(not(target_os = "macos"))]
    let _ = snapshot;
    #[cfg(target_os = "macos")]
    if let Some(expected) = &self.read_boundary_digest
      && &macos_read_boundary_digest(snapshot)? != expected
    {
      return Err(RailError::with_help(
        "hermetic platform or toolchain inputs changed during execution",
        "retry after toolchain and platform updates have finished",
      ));
    }
    Ok(())
  }
}

impl PlatformBoundary {
  fn capture(snapshot: &WorkspaceSnapshot, layout: &RunLayout, inventory: &FetchInventory) -> RailResult<Self> {
    #[cfg(not(target_os = "macos"))]
    let _ = (snapshot, layout, inventory);
    let mut identity = FramedHasher::new(b"cargo-rail-hermetic-platform\0");
    identity.frame(b"family", std::env::consts::FAMILY.as_bytes());
    identity.frame(b"os", std::env::consts::OS.as_bytes());
    identity.frame(b"arch", std::env::consts::ARCH.as_bytes());

    #[cfg(target_os = "macos")]
    {
      let sandbox = PathBuf::from("/usr/bin/sandbox-exec");
      if sandbox.is_file() {
        let policy = macos_sandbox_policy(snapshot, layout, inventory)?;
        let read_boundary_digest = macos_read_boundary_digest(snapshot)?;
        let executable = ExecutableIdentity::capture(sandbox.as_os_str(), Path::new("/"), snapshot.source_root())?;
        identity.frame(b"sandbox", &executable.identity_bytes()?);
        identity.frame(b"policy-version", &MACOS_SANDBOX_POLICY_VERSION.to_le_bytes());
        identity.frame(b"read-boundary", read_boundary_digest.as_bytes());
        identity.frame(
          b"policy-semantics",
          b"deny-default;dyld-support;read=isolated-run,inventory,toolchain,executables,sealed-system;write=target,build,tmp,home,observations,package-cache;deny-network",
        );
        identity.frame(b"os-build", &command_stdout("/usr/bin/sw_vers", &["-buildVersion"])?);
        identity.frame(b"kernel", &command_stdout("/usr/bin/uname", &["-srv"])?);
        return Ok(Self {
          enforcement: EnforcementLevel::FilesystemAndNetwork,
          identity: format!("platform-v1-sha256-{}", identity.finish()),
          sandbox_program: Some(sandbox),
          sandbox_policy: Some(policy),
          read_boundary_digest: Some(read_boundary_digest),
        });
      }
    }

    Ok(Self {
      enforcement: EnforcementLevel::CargoOfflineOnly,
      identity: format!("platform-v1-sha256-{}", identity.finish()),
      sandbox_program: None,
      sandbox_policy: None,
      #[cfg(target_os = "macos")]
      read_boundary_digest: None,
    })
  }
}

#[cfg(target_os = "macos")]
fn command_stdout(program: &str, arguments: &[&str]) -> RailResult<Vec<u8>> {
  crate::instrumentation::record_hermetic_platform_probe();
  let output = Command::new(program).args(arguments).output().map_err(|error| {
    RailError::message(format!(
      "failed to capture hermetic platform identity from '{program}': {error}"
    ))
  })?;
  if !output.status.success() {
    return Err(RailError::message(format!(
      "hermetic platform identity command '{program}' failed"
    )));
  }
  Ok(output.stdout)
}

#[cfg(target_os = "macos")]
fn macos_read_boundary_digest(snapshot: &WorkspaceSnapshot) -> RailResult<String> {
  macos_read_boundary_digest_for(snapshot.toolchain().rustc_sysroot(), snapshot.toolchain().host_target())
}

#[cfg(target_os = "macos")]
fn macos_read_boundary_digest_for(sysroot: &Path, host_target: &str) -> RailResult<String> {
  let mut identity = FramedHasher::new(b"cargo-rail-macos-read-boundary\0");
  identity.frame(b"version", &MACOS_SANDBOX_POLICY_VERSION.to_le_bytes());
  for path in toolchain_read_files_for(sysroot)? {
    let relative = path.strip_prefix(sysroot).map_err(|_| {
      RailError::message(format!(
        "toolchain input '{}' is outside rustc sysroot '{}'",
        path.display(),
        sysroot.display()
      ))
    })?;
    frame_read_boundary_file(
      &mut identity,
      crate::utils::path_to_git_format(relative).as_bytes(),
      &path,
    )?;
  }
  for root in toolchain_read_roots_for(sysroot, host_target) {
    let relative = root.strip_prefix(sysroot).map_err(|_| {
      RailError::message(format!(
        "toolchain input root '{}' is outside rustc sysroot '{}'",
        root.display(),
        sysroot.display()
      ))
    })?;
    identity.frame(b"toolchain-root", crate::utils::path_to_git_format(relative).as_bytes());
    frame_read_boundary_tree(&mut identity, &root)?;
  }
  let current = std::env::current_exe()
    .map_err(|error| RailError::message(format!("failed to locate cargo-rail compiler observer: {error}")))?;
  frame_read_boundary_file(&mut identity, b"compiler-observer", &current)?;
  frame_read_boundary_file(&mut identity, b"sandbox-exec", Path::new("/usr/bin/sandbox-exec"))?;
  for path in MACOS_REQUIRED_HOST_FILES {
    frame_read_boundary_file(&mut identity, path.as_bytes(), Path::new(path))?;
  }
  Ok(format!("read-boundary-v1-sha256-{}", identity.finish()))
}

#[cfg(target_os = "macos")]
fn fast_platform_identity(sysroot: &Path, host_target: &str, source_root: &Path) -> RailResult<String> {
  let sandbox = PathBuf::from("/usr/bin/sandbox-exec");
  if !sandbox.is_file() {
    return Err(RailError::message(
      "macOS sandbox-exec is unavailable for process-free cache validation",
    ));
  }
  let mut identity = FramedHasher::new(b"cargo-rail-hermetic-platform\0");
  identity.frame(b"family", std::env::consts::FAMILY.as_bytes());
  identity.frame(b"os", std::env::consts::OS.as_bytes());
  identity.frame(b"arch", std::env::consts::ARCH.as_bytes());
  let executable = ExecutableIdentity::capture(sandbox.as_os_str(), Path::new("/"), source_root)?;
  identity.frame(b"sandbox", &executable.identity_bytes()?);
  identity.frame(b"policy-version", &MACOS_SANDBOX_POLICY_VERSION.to_le_bytes());
  identity.frame(
    b"read-boundary",
    macos_read_boundary_digest_for(sysroot, host_target)?.as_bytes(),
  );
  identity.frame(
    b"policy-semantics",
    b"deny-default;dyld-support;read=isolated-run,inventory,toolchain,executables,sealed-system;write=target,build,tmp,home,observations,package-cache;deny-network",
  );
  identity.frame(b"os-build", &command_stdout("/usr/bin/sw_vers", &["-buildVersion"])?);
  identity.frame(b"kernel", &command_stdout("/usr/bin/uname", &["-srv"])?);
  Ok(format!("platform-v1-sha256-{}", identity.finish()))
}

#[cfg(target_os = "macos")]
fn frame_read_boundary_file(identity: &mut FramedHasher, label: &[u8], path: &Path) -> RailResult<()> {
  let before = fs::symlink_metadata(path)?;
  if !before.is_file() || before.file_type().is_symlink() {
    return Err(RailError::message(format!(
      "hermetic read input '{}' is not a regular file",
      path.display()
    )));
  }
  let (digest, bytes, _) = digest_and_scan_file(path, &[])?;
  let after = fs::symlink_metadata(path)?;
  if read_boundary_metadata(&before) != read_boundary_metadata(&after) {
    return Err(RailError::with_help(
      format!("hermetic read input '{}' changed while it was hashed", path.display()),
      "retry after toolchain and platform updates have finished",
    ));
  }
  identity.frame(b"file-path", label);
  identity.frame(b"file-digest", digest.as_bytes());
  identity.frame(b"file-bytes", &bytes.to_le_bytes());
  identity.frame(b"file-mode", &output_mode(&before).to_le_bytes());
  Ok(())
}

#[cfg(target_os = "macos")]
fn frame_read_boundary_tree(identity: &mut FramedHasher, root: &Path) -> RailResult<()> {
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    let relative = path.strip_prefix(root).map_err(|_| {
      RailError::message(format!(
        "toolchain read input '{}' escaped root '{}'",
        path.display(),
        root.display()
      ))
    })?;
    let label = crate::utils::path_to_git_format(relative);
    let before = fs::symlink_metadata(&path)?;
    identity.frame(b"tree-path", label.as_bytes());
    identity.frame(b"tree-mode", &output_mode(&before).to_le_bytes());
    if before.file_type().is_symlink() {
      let target = fs::read_link(&path)?;
      let repository_path = crate::source::RepositoryPath::new(relative)?;
      let target = target
        .to_str()
        .ok_or_else(|| RailError::message(format!("toolchain symlink '{}' has a non-UTF-8 target", path.display())))?;
      if symlink_target_escapes(&repository_path, target) {
        return Err(RailError::with_help(
          format!("toolchain symlink '{}' escapes its declared read root", path.display()),
          "use an immutable self-contained Rust toolchain for hermetic execution",
        ));
      }
      identity.frame(b"tree-kind", b"symlink");
      identity.frame(b"tree-target", target.as_bytes());
    } else if before.is_file() {
      identity.frame(b"tree-kind", b"file");
      frame_read_boundary_file(identity, label.as_bytes(), &path)?;
    } else if before.is_dir() {
      identity.frame(b"tree-kind", b"directory");
      let mut children = fs::read_dir(&path)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
      children.sort();
      children.reverse();
      pending.extend(children);
    } else {
      return Err(RailError::with_help(
        format!("toolchain read input '{}' has an unsupported file type", path.display()),
        "use an immutable toolchain containing only files, directories, and bounded symlinks",
      ));
    }
    let after = fs::symlink_metadata(&path)?;
    if read_boundary_metadata(&before) != read_boundary_metadata(&after) {
      return Err(RailError::with_help(
        format!(
          "toolchain read input '{}' changed while it was captured",
          path.display()
        ),
        "retry after toolchain updates have finished",
      ));
    }
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn read_boundary_metadata(metadata: &fs::Metadata) -> (u64, u64, u32, u64, u64, i64, i64, i64, i64) {
  use std::os::unix::fs::MetadataExt as _;

  (
    metadata.dev(),
    metadata.ino(),
    metadata.mode(),
    metadata.nlink(),
    metadata.size(),
    metadata.mtime(),
    metadata.mtime_nsec(),
    metadata.ctime(),
    metadata.ctime_nsec(),
  )
}

#[cfg(target_os = "macos")]
fn macos_sandbox_policy(
  snapshot: &WorkspaceSnapshot,
  layout: &RunLayout,
  inventory: &FetchInventory,
) -> RailResult<String> {
  let current = std::env::current_exe()
    .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))?;
  let (inventory_roots, inventory_files) = immutable_inventory_paths(&inventory.cargo_home)?;
  let mut read_roots = vec![layout.root.clone()];
  read_roots.extend(inventory_roots);
  read_roots.extend(toolchain_read_roots(snapshot));
  let mut policy = String::from(
    "(version 1)\n(deny default)\n(import \"dyld-support.sb\")\n(allow process*)\n(allow signal)\n(allow sysctl-read)\n(allow mach*)\n(allow ipc-posix-shm)\n",
  );
  for root in read_roots {
    let canonical = fs::canonicalize(&root).unwrap_or(root);
    policy.push_str(&format!(
      "(allow file-read* file-test-existence (subpath {}))\n",
      sandbox_string(&canonical)?
    ));
    policy.push_str(&format!(
      "(allow file-read-metadata file-test-existence (path-ancestors {}))\n",
      sandbox_string(&canonical)?
    ));
  }
  let mut read_files = toolchain_read_files(snapshot)?;
  read_files.extend(inventory_files);
  read_files.sort();
  read_files.dedup();
  for path in read_files
    .iter()
    .map(PathBuf::as_path)
    .chain(std::iter::once(current.as_path()))
    .chain(MACOS_REQUIRED_HOST_FILES.iter().map(Path::new))
  {
    let canonical = fs::canonicalize(path)?;
    policy.push_str(&format!(
      "(allow file-read* file-test-existence (literal {}))\n",
      sandbox_string(&canonical)?
    ));
    policy.push_str(&format!(
      "(allow file-read-metadata file-test-existence (path-ancestors {}))\n",
      sandbox_string(&canonical)?
    ));
  }
  for path in [Path::new("/dev/null"), Path::new("/dev/zero"), Path::new("/dev/fd")] {
    policy.push_str(&format!(
      "(allow file-read* file-test-existence (subpath {}))\n",
      sandbox_string(path)?
    ));
  }
  for root in [
    layout.target.as_path(),
    layout.build.as_path(),
    layout.temporary.as_path(),
    layout.home.as_path(),
    layout.observations.as_path(),
  ] {
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    policy.push_str(&format!(
      "(allow file-write* (subpath {}))\n",
      sandbox_string(&canonical)?
    ));
  }
  policy.push_str("(deny network*)\n");
  Ok(policy)
}

#[cfg(target_os = "macos")]
fn sandbox_string(path: &Path) -> RailResult<String> {
  let value = path
    .to_str()
    .ok_or_else(|| RailError::message(format!("sandbox path '{}' is not valid UTF-8", path.display())))?;
  let mut escaped = String::with_capacity(value.len() + 2);
  escaped.push('"');
  for character in value.chars() {
    match character {
      '"' => escaped.push_str("\\\""),
      '\\' => escaped.push_str("\\\\"),
      '\n' | '\r' | '\0' => return Err(RailError::message("sandbox path contains a control character")),
      _ => escaped.push(character),
    }
  }
  escaped.push('"');
  Ok(escaped)
}

fn toolchain_program(sysroot: &Path, name: &str) -> PathBuf {
  #[cfg(windows)]
  let name = format!("{name}.exe");
  sysroot.join("bin").join(name)
}

#[cfg(target_os = "macos")]
fn toolchain_read_roots(snapshot: &WorkspaceSnapshot) -> Vec<PathBuf> {
  toolchain_read_roots_for(snapshot.toolchain().rustc_sysroot(), snapshot.toolchain().host_target())
}

#[cfg(target_os = "macos")]
fn toolchain_read_roots_for(sysroot: &Path, host_target: &str) -> Vec<PathBuf> {
  let mut roots = Vec::new();
  let target = sysroot.join("lib/rustlib").join(host_target);
  for relative in ["lib", "codegen-backends"] {
    let root = target.join(relative);
    if root.is_dir() {
      roots.push(root);
    }
  }
  roots.sort();
  roots.dedup();
  roots
}

#[cfg(target_os = "macos")]
fn toolchain_read_files(snapshot: &WorkspaceSnapshot) -> RailResult<Vec<PathBuf>> {
  toolchain_read_files_for(snapshot.toolchain().rustc_sysroot())
}

#[cfg(target_os = "macos")]
fn toolchain_read_files_for(sysroot: &Path) -> RailResult<Vec<PathBuf>> {
  let mut files = vec![toolchain_program(sysroot, "cargo"), toolchain_program(sysroot, "rustc")];
  for entry in fs::read_dir(sysroot.join("lib"))? {
    let path = entry?.path();
    let is_rustc_driver = path
      .file_name()
      .and_then(OsStr::to_str)
      .is_some_and(|name| name.starts_with("librustc_driver-") && name.ends_with(".dylib"));
    if is_rustc_driver && fs::symlink_metadata(&path)?.file_type().is_file() {
      files.push(path);
    }
  }
  files.sort();
  files.dedup();
  Ok(files)
}

struct FramedHasher {
  hasher: Sha256,
  input_bytes: usize,
}

impl FramedHasher {
  fn new(domain: &[u8]) -> Self {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    Self {
      hasher,
      input_bytes: domain.len(),
    }
  }

  fn frame(&mut self, tag: &[u8], value: &[u8]) {
    self.hasher.update((tag.len() as u64).to_le_bytes());
    self.hasher.update(tag);
    self.hasher.update((value.len() as u64).to_le_bytes());
    self.hasher.update(value);
    self.input_bytes = self
      .input_bytes
      .saturating_add(16)
      .saturating_add(tag.len())
      .saturating_add(value.len());
  }

  fn finish(self) -> ContentDigest {
    crate::instrumentation::record_hash(self.input_bytes);
    ContentDigest::from_sha256_bytes(self.hasher.finalize().into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn package() -> InventoryPackage {
    InventoryPackage {
      identity: "external@1.0.0#source-sha256-source".to_string(),
      checksum: Some("checksum".to_string()),
      source_tree_digest: "sha256:tree".to_string(),
      source_entries: 3,
    }
  }

  #[test]
  fn remote_registry_source_configuration_rejects_local_and_ambiguous_sources() {
    let supported = serde_json::json!({
      "crates-io": { "replace-with": "mirror" },
      "mirror": { "registry": "sparse+https://registry.example/index/" }
    });
    assert!(remote_registry_source_configuration(&supported));
    for unsupported in [
      serde_json::json!({ "vendor": { "directory": "vendor" } }),
      serde_json::json!({ "local": { "local-registry": "registry" } }),
      serde_json::json!({ "crates-io": { "replace-with": "missing" } }),
      serde_json::json!({
        "left": { "replace-with": "right" },
        "right": { "replace-with": "left" }
      }),
      serde_json::json!({ "mirror": { "registry": "file:///tmp/index" } }),
    ] {
      assert!(!remote_registry_source_configuration(&unsupported));
    }
  }

  #[test]
  fn fetch_result_binds_external_package_content() {
    let baseline = fetch_result_digest(&[package()]).expect("fetch result should hash");
    for changed in [
      InventoryPackage {
        identity: "external@1.0.0#source-sha256-other".to_string(),
        ..package()
      },
      InventoryPackage {
        checksum: Some("other".to_string()),
        ..package()
      },
      InventoryPackage {
        source_tree_digest: "sha256:other".to_string(),
        ..package()
      },
      InventoryPackage {
        source_entries: 4,
        ..package()
      },
    ] {
      assert_ne!(
        fetch_result_digest(&[changed]).expect("changed fetch result should hash"),
        baseline
      );
    }
    assert_ne!(
      fetch_result_digest(&[package(), package()]).expect("second package should hash"),
      baseline
    );
  }

  #[test]
  fn exact_inventory_tree_binds_bytes_modes_and_bounded_symlinks() {
    let root = tempfile::tempdir().expect("inventory root should exist");
    fs::write(root.path().join("source.rs"), "alpha\n").expect("source should be written");
    fs::write(root.path().join("ignored"), "first\n").expect("ignored file should be written");
    let ignored = [root.path().join("ignored")];
    let baseline = capture_exact_tree(root.path(), &ignored).expect("tree should capture");
    fs::write(root.path().join("ignored"), "other\n").expect("ignored file should change");
    assert_eq!(
      capture_exact_tree(root.path(), &ignored)
        .expect("ignored mutation should capture")
        .digest,
      baseline.digest
    );
    fs::write(root.path().join("source.rs"), "omega\n").expect("same-size source should change");
    assert_ne!(
      capture_exact_tree(root.path(), &ignored)
        .expect("source mutation should capture")
        .digest,
      baseline.digest
    );

    #[cfg(unix)]
    {
      use std::os::unix::fs::{PermissionsExt as _, symlink};

      let file = root.path().join("source.rs");
      let mut permissions = fs::metadata(&file).expect("source metadata").permissions();
      permissions.set_mode(0o755);
      fs::set_permissions(&file, permissions).expect("source mode should change");
      let executable = capture_exact_tree(root.path(), &ignored).expect("executable tree should capture");
      permissions = fs::metadata(&file).expect("source metadata").permissions();
      permissions.set_mode(0o644);
      fs::set_permissions(&file, permissions).expect("source mode should reset");
      assert_ne!(
        capture_exact_tree(root.path(), &ignored)
          .expect("non-executable tree should capture")
          .digest,
        executable.digest
      );

      symlink("source.rs", root.path().join("inside")).expect("bounded symlink should be created");
      assert!(capture_exact_tree(root.path(), &ignored).is_ok());
      symlink("../outside", root.path().join("outside")).expect("escaping symlink should be created");
      let error = capture_exact_tree(root.path(), &ignored).expect_err("escaping symlink must fail");
      assert!(error.to_string().contains("escapes the immutable store"));
    }
  }

  #[test]
  fn output_manifest_digest_binds_path_kind_mode_and_content() {
    let baseline = vec![
      OutputEntry {
        path: "target/debug".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/debug/lib.rmeta".to_string(),
        kind: OutputEntryKind::File {
          digest: "sha256:content".to_string(),
          mode: 0o644,
          bytes: 7,
        },
      },
    ];
    let digest = output_manifest_digest(&baseline).expect("output manifest should hash");
    let mutations = [
      OutputEntry {
        path: "target/release/lib.rmeta".to_string(),
        kind: baseline[1].kind.clone(),
      },
      OutputEntry {
        path: baseline[1].path.clone(),
        kind: OutputEntryKind::File {
          digest: "sha256:other".to_string(),
          mode: 0o644,
          bytes: 7,
        },
      },
      OutputEntry {
        path: baseline[1].path.clone(),
        kind: OutputEntryKind::File {
          digest: "sha256:content".to_string(),
          mode: 0o755,
          bytes: 7,
        },
      },
      OutputEntry {
        path: baseline[1].path.clone(),
        kind: OutputEntryKind::File {
          digest: "sha256:content".to_string(),
          mode: 0o644,
          bytes: 8,
        },
      },
      OutputEntry {
        path: baseline[1].path.clone(),
        kind: OutputEntryKind::Symlink {
          target: "other".to_string(),
        },
      },
    ];
    for mutation in mutations {
      let mut entries = baseline.clone();
      entries[1] = mutation;
      assert_ne!(output_manifest_digest(&entries).expect("mutation should hash"), digest);
    }
  }

  #[test]
  fn output_manifest_revalidation_rejects_same_size_mutation() {
    let root = tempfile::tempdir().expect("temporary output root should exist");
    let path = root.path().join("target/debug/lib.rmeta");
    fs::create_dir_all(path.parent().expect("output should have a parent")).expect("output parent should be created");
    fs::write(&path, b"alpha").expect("output should be written");
    let output = FileObservation::capture(&path, root.path(), root.path()).expect("output should capture");
    let mut reasons = BTreeSet::new();
    let manifest =
      capture_compiler_outputs(root.path(), &[], &[output], &[], &mut reasons).expect("output manifest should capture");
    assert!(reasons.is_empty());
    manifest
      .validate_unchanged(root.path())
      .expect("unchanged output should revalidate");

    fs::write(&path, b"omega").expect("same-size output mutation should be written");
    let error = manifest
      .validate_unchanged(root.path())
      .expect_err("same-size output mutation must fail revalidation");
    assert!(error.to_string().contains("changed before publication"));
  }

  #[test]
  fn byte_replacement_is_exact_and_non_overlapping() {
    assert_eq!(
      replace_bytes(b"aa/root/bb/root", b"/root", b"/workspace"),
      b"aa/workspace/bb/workspace"
    );
    assert_eq!(replace_bytes(b"aaaa", b"aa", b"x"), b"xx");
    assert_eq!(replace_bytes(b"unchanged", b"", b"x"), b"unchanged");
    assert_eq!(replace_bytes(b"short", b"longer", b"x"), b"short");
  }

  #[test]
  fn output_scan_finds_a_physical_root_across_read_buffers() {
    let directory = tempfile::tempdir().expect("temporary output root should exist");
    let path = directory.path().join("artifact");
    let pattern = b"physical-root";
    let mut bytes = vec![b'x'; IO_BUFFER_BYTES - 4];
    bytes.extend_from_slice(pattern);
    fs::write(&path, &bytes).expect("artifact should be written");
    let (_, length, found) = digest_and_scan_file(&path, &[pattern.to_vec()]).expect("artifact should scan");
    assert_eq!(length, bytes.len() as u64);
    assert!(found);
  }

  #[test]
  fn relative_symlink_escape_detection_is_lexical_and_root_bounded() {
    let root = crate::source::RepositoryPath::new(Path::new("link")).expect("root link path");
    let nested = crate::source::RepositoryPath::new(Path::new("a/b/link")).expect("nested link path");
    assert!(symlink_target_escapes(&root, "../outside"));
    assert!(symlink_target_escapes(&nested, "../../../outside"));
    assert!(!symlink_target_escapes(&nested, "../../inside"));
    assert!(!symlink_target_escapes(&nested, "../sibling"));
    assert!(symlink_target_escapes(&nested, "/absolute"));
  }

  #[test]
  fn cargo_mutable_bookkeeping_is_not_part_of_the_immutable_inventory() {
    let root = Path::new("/cargo-home");
    assert_eq!(
      cargo_mutable_cache_paths(root),
      [".global-cache", ".package-cache", ".package-cache-mutate"]
        .map(|name| root.join(name))
        .to_vec()
    );
  }

  #[test]
  fn fetch_processes_pin_the_exact_toolchain_without_rustup_state() {
    let sysroot = Path::new("/toolchain");
    let mut command = Command::new("cargo");
    command.env("RUSTUP_HOME", "/ambient/rustup");
    command.env("RUSTUP_TOOLCHAIN", "ambient");
    configure_fetch_environment(
      &mut command,
      sysroot,
      Path::new("/cargo-home"),
      Path::new("/home"),
      true,
    );
    let environment = command
      .get_envs()
      .map(|(name, value)| (name.to_os_string(), value.map(OsStr::to_os_string)))
      .collect::<BTreeMap<_, _>>();
    assert_eq!(
      environment.get(OsStr::new("RUSTC")).and_then(Option::as_deref),
      Some(toolchain_program(sysroot, "rustc").as_os_str())
    );
    assert_eq!(
      environment.get(OsStr::new("RUSTDOC")).and_then(Option::as_deref),
      Some(toolchain_program(sysroot, "rustdoc").as_os_str())
    );
    assert!(!environment.contains_key(OsStr::new("RUSTUP_HOME")));
    assert!(!environment.contains_key(OsStr::new("RUSTUP_TOOLCHAIN")));
    assert_eq!(
      environment
        .get(OsStr::new("CARGO_NET_OFFLINE"))
        .and_then(Option::as_deref),
      Some(OsStr::new("true"))
    );
  }

  #[test]
  fn rustflags_that_escape_declared_tool_or_output_roots_fail_closed() {
    assert!(!rustflags_escape_hermetic_contract(&[
      "--cfg".to_string(),
      "feature=\"stable\"".to_string(),
      "-Copt-level=2".to_string(),
    ]));
    for flags in [
      vec!["@flags.rsp"],
      vec!["--sysroot=/other"],
      vec!["-Lnative=/other"],
      vec!["-Clinker=/other/cc"],
      vec!["-C", "incremental=/other"],
      vec!["-Ctarget-cpu=native"],
      vec!["-Zunstable-options"],
    ] {
      assert!(rustflags_escape_hermetic_contract(
        &flags.into_iter().map(str::to_string).collect::<Vec<_>>()
      ));
    }
  }

  #[test]
  fn private_observation_environment_cannot_override_the_hermetic_wrapper() {
    for name in [
      "CARGO_RAIL_COMPILER_CACHE_WRAPPER",
      "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY",
      "CARGO_RAIL_COMPILER_OBSERVATION_ONLY",
      "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT",
      "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION",
      "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION",
      "CARGO_RAIL_NATIVE_COMPILER_CACHE_STORE",
      "CARGO_RAIL_INNER_RUSTDOC",
      "CARGO_RAIL_INNER_WORKSPACE_WRAPPER",
      "CARGO_RAIL_RUSTC_WRAPPER",
      "CARGO_RAIL_RUSTDOC_WRAPPER",
    ] {
      assert!(hermetic_environment_name_is_controlled(name));
    }
  }

  #[test]
  fn workspace_pruning_keeps_only_bounded_protected_state() {
    let root = tempfile::tempdir().expect("workspace state root");
    let older = root.path().join("a");
    let retained = root.path().join("b");
    fs::create_dir(&older).expect("older state");
    fs::create_dir(&retained).expect("retained state");
    fs::write(older.join("payload"), [1; 8]).expect("older payload");
    fs::write(retained.join("payload"), [2; 8]).expect("retained payload");

    prune_children(root.path(), Some(&retained), 8, 1).expect("bounded pruning");

    assert!(!older.exists());
    assert!(retained.is_dir());
  }

  #[test]
  fn fetch_inventory_retention_removes_superseded_and_crash_state() {
    let root = tempfile::tempdir().expect("inventory root");
    let retained = root.path().join("current");
    fs::create_dir(&retained).expect("current inventory");
    fs::write(retained.join("manifest.json"), b"{}").expect("current manifest");
    fs::create_dir(root.path().join("superseded")).expect("superseded inventory");
    fs::create_dir(root.path().join("fetch-crash")).expect("crash staging");

    retain_fetch_inventory(root.path(), &retained).expect("inventory retention");

    assert_eq!(fs::read_dir(root.path()).expect("retained inventory root").count(), 1);
    assert!(retained.is_dir());
  }
}
