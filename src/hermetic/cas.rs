//! Immutable machine-local storage for verified hermetic action outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{FastCacheValidation, OutputEntry, OutputEntryKind, OutputManifest};
use crate::error::{RailError, RailResult};

const CAS_VERSION: u32 = 1;
const OWNER_MARKER: &[u8] = b"cargo-rail-local-cas\nschema=1\n";
const CACHE_BASE_ENV: &str = "CARGO_RAIL_CACHE_DIR";
const CACHE_MAX_BYTES_ENV: &str = "CARGO_RAIL_CACHE_MAX_BYTES";
const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_RESULT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_OBJECT_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LOOKUP_BYTES: u64 = MAX_RESULT_BYTES + MAX_OBJECT_METADATA_BYTES + 1;
const MAX_TREE_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_NAME_BYTES: usize = 255;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_CANDIDATE_PINS: usize = 4096;
const IO_BUFFER_BYTES: usize = 64 * 1024;
const STALE_LEASE_SECONDS: u64 = 24 * 60 * 60;

const BLOB_PREFIX: &str = "blob-v1-sha256-";
const TREE_PREFIX: &str = "tree-v1-sha256-";
const MANIFEST_PREFIX: &str = "output-manifest-v1-sha256-";
const ACTION_RESULT_PREFIX: &str = "action-result-v1-sha256-";
const ACTION_KEY_PREFIX: &str = "hermetic-action-v1-sha256-";
const VALIDATION_PREFIX: &str = "validation-v1-sha256-";
const LOOKUP_PREFIX: &str = "local-lookup-v1-sha256-";

/// A verified cache lookup restored into an isolated output root.
pub(super) struct CacheHit {
  pub(super) action_result: String,
  pub(super) result_digest: String,
  pub(super) output_manifest: OutputManifest,
  pub(super) compiler_units: usize,
  pub(super) objects_verified: u64,
  pub(super) bytes_read: u64,
  pub(super) bytes_restored: u64,
}

/// A fail-closed lookup outcome that permits ordinary cold execution.
pub(super) struct CacheMiss {
  pub(super) kind: CacheMissKind,
  pub(super) reason: String,
  pub(super) objects_verified: u64,
  pub(super) bytes_read: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CacheMissKind {
  Miss,
  Corrupt,
  Incompatible,
}

pub(super) enum CacheLookup {
  Hit(CacheHit),
  Miss(CacheMiss),
}

/// A fully verified action-result bundle that may be checked against current raw inputs.
pub(super) struct CacheCandidate {
  pub(super) action_key: String,
  pub(super) validation: FastCacheValidation,
}

#[derive(Debug, Default)]
pub(super) struct StoreStats {
  pub(super) action_result: Option<String>,
  pub(super) objects_written: u64,
  pub(super) bytes_written: u64,
}

pub(super) struct StoreRequest<'a> {
  pub(super) action_key: &'a str,
  pub(super) lookup_key: &'a str,
  pub(super) result_digest: &'a str,
  pub(super) manifest: &'a OutputManifest,
  pub(super) validation: &'a FastCacheValidation,
  pub(super) compiler_units: usize,
  pub(super) source_root: &'a Path,
}

/// One validated local CAS rooted outside any physical checkout.
#[derive(Debug)]
pub(super) struct LocalCas {
  root: PathBuf,
  max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeObject {
  version: u32,
  entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct TreeEntry {
  name: String,
  #[serde(flatten)]
  kind: TreeEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TreeEntryKind {
  File {
    blob: String,
    content_digest: String,
    bytes: u64,
    mode: u32,
  },
  Directory {
    tree: String,
    mode: u32,
  },
  Symlink {
    target: String,
    directory: bool,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionResultObject {
  version: u32,
  action_key: String,
  lookup_key: String,
  result_digest: String,
  output_manifest: String,
  output_tree: String,
  validation: String,
  compiler_units: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPin {
  version: u32,
  action_key: String,
  action_result: String,
  lookup_key: String,
  created_unix_nanos: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRecord {
  version: u32,
  action_result: String,
  created_unix_seconds: u64,
}

#[derive(Default)]
struct ReadStats {
  objects: u64,
  bytes: u64,
  restored: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultKind {
  Miss,
  Corrupt,
  Incompatible,
}

#[derive(Debug)]
struct Fault {
  kind: FaultKind,
  reason: String,
}

impl Fault {
  fn corrupt(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Corrupt,
      reason: reason.into(),
    }
  }

  fn miss(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Miss,
      reason: reason.into(),
    }
  }

  fn incompatible(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Incompatible,
      reason: reason.into(),
    }
  }
}

struct LeaseGuard {
  path: PathBuf,
}

impl Drop for LeaseGuard {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.path);
  }
}

#[derive(Debug)]
struct PreparedBlob {
  source: PathBuf,
  content_digest: String,
  bytes: u64,
}

#[derive(Debug)]
struct PreparedTree {
  root: String,
  trees: BTreeMap<String, Vec<u8>>,
  blobs: BTreeMap<String, PreparedBlob>,
}

struct BundlePublication<'a> {
  action_result: &'a str,
  object: &'a ActionResultObject,
  object_bytes: &'a [u8],
  manifest: &'a OutputManifest,
  manifest_bytes: &'a [u8],
  validation: &'a FastCacheValidation,
  validation_bytes: &'a [u8],
  prepared: &'a PreparedTree,
}

#[derive(Default)]
struct BuildDirectory {
  children: BTreeMap<String, BuildNode>,
}

enum BuildNode {
  File {
    source: PathBuf,
    content_digest: String,
    bytes: u64,
    mode: u32,
  },
  Directory {
    mode: u32,
    contents: BuildDirectory,
  },
  Symlink {
    target: String,
    directory: bool,
  },
}

impl LocalCas {
  pub(super) fn root(&self) -> &Path {
    &self.root
  }

  pub(super) fn open() -> RailResult<Self> {
    let base = cache_base()?;
    fs::create_dir_all(&base).map_err(|error| {
      RailError::message(format!(
        "failed to create local cache base '{}': {error}",
        base.display()
      ))
    })?;
    let base = fs::canonicalize(&base).map_err(|error| {
      RailError::message(format!(
        "failed to resolve local cache base '{}': {error}",
        base.display()
      ))
    })?;
    validate_real_directory(&base, "local cache base")?;
    let cargo_rail = create_real_directory(&base, "cargo-rail")?;
    let root = create_real_directory(&cargo_rail, "local-cas-v1")?;
    ensure_owner_marker(&root)?;
    create_real_directory(&root, "staging")?;
    for name in ["results", "pins", "leases"] {
      create_real_directory(&root, name)?;
    }
    validate_root_entries(&root)?;
    Ok(Self {
      root,
      max_bytes: cache_max_bytes()?,
    })
  }

  #[cfg(test)]
  fn open_at(base: &Path, max_bytes: u64) -> RailResult<Self> {
    fs::create_dir_all(base)?;
    let base = fs::canonicalize(base)?;
    let cargo_rail = create_real_directory(&base, "cargo-rail")?;
    let root = create_real_directory(&cargo_rail, "local-cas-v1")?;
    ensure_owner_marker(&root)?;
    create_real_directory(&root, "staging")?;
    for name in ["results", "pins", "leases"] {
      create_real_directory(&root, name)?;
    }
    Ok(Self { root, max_bytes })
  }

  pub(super) fn restore(&self, action_key: &str, destination: &Path) -> CacheLookup {
    let mut stats = ReadStats::default();
    match self.restore_inner(action_key, destination, &mut stats) {
      Ok(hit) => CacheLookup::Hit(CacheHit {
        action_result: hit.action_result,
        result_digest: hit.object.result_digest,
        output_manifest: hit.manifest,
        compiler_units: hit.object.compiler_units,
        objects_verified: stats.objects,
        bytes_read: stats.bytes,
        bytes_restored: stats.restored,
      }),
      Err(fault) => CacheLookup::Miss(CacheMiss {
        kind: match fault.kind {
          FaultKind::Miss => CacheMissKind::Miss,
          FaultKind::Corrupt => CacheMissKind::Corrupt,
          FaultKind::Incompatible => CacheMissKind::Incompatible,
        },
        reason: fault.reason,
        objects_verified: stats.objects,
        bytes_read: stats.bytes,
      }),
    }
  }

  pub(super) fn candidates(&self, lookup_key: &str) -> RailResult<Vec<CacheCandidate>> {
    validate_lookup_key(lookup_key)?;
    let pins_directory = self.root.join("pins");
    let mut entries = fs::read_dir(&pins_directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_CANDIDATE_PINS {
      return Err(RailError::message(format!(
        "local CAS has more than {MAX_CANDIDATE_PINS} action pins; refusing an unbounded pre-context scan"
      )));
    }
    let mut candidates = Vec::new();
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || metadata.file_type().is_symlink() || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS pin '{}' is not a bounded regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS pin has a non-UTF-8 name"))?;
      let key_hex = file_name
        .strip_suffix(".json")
        .ok_or_else(|| RailError::message(format!("local CAS pin '{file_name}' has an invalid name")))?;
      let mut stats = ReadStats::default();
      let pin: ActionPin = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if pin.version != CAS_VERSION {
        return Err(RailError::message("local CAS candidate pin has an incompatible schema"));
      }
      if validated_id_hex(&pin.action_key, ACTION_KEY_PREFIX)? != key_hex {
        return Err(RailError::message(
          "local CAS candidate pin filename does not match its action key",
        ));
      }
      validate_lookup_key(&pin.lookup_key)?;
      validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)?;
      if pin.lookup_key != lookup_key {
        continue;
      }
      let verified = self
        .load_verified_result(&pin.action_key, &pin.action_result, &mut stats)
        .map_err(fault_to_error)?;
      if verified.object.lookup_key != pin.lookup_key {
        return Err(RailError::message(
          "local CAS candidate pin does not match its verified action result",
        ));
      }
      verified.validation.validate_object()?;
      candidates.push(CacheCandidate {
        action_key: pin.action_key,
        validation: verified.validation,
      });
    }
    Ok(candidates)
  }

  pub(super) fn store(&self, request: StoreRequest<'_>) -> RailResult<StoreStats> {
    let StoreRequest {
      action_key,
      lookup_key,
      result_digest,
      manifest,
      validation,
      compiler_units,
      source_root,
    } = request;
    validate_action_key(action_key)?;
    validate_lookup_key(lookup_key)?;
    validation.validate_object()?;
    if validation.action_key != action_key || validation.lookup_key != lookup_key {
      return Err(RailError::message(
        "local cache validation manifest does not match the stored action",
      ));
    }
    validate_manifest(manifest).map_err(fault_to_error)?;
    manifest.validate_unchanged(source_root)?;
    let prepared = prepare_tree(manifest, source_root).map_err(fault_to_error)?;
    let manifest_bytes = canonical_json(manifest)?;
    let manifest_id = manifest.digest.clone();
    let validation_bytes = canonical_json(validation)?;
    let validation_id = validation_id(&validation_bytes);
    let object = ActionResultObject {
      version: CAS_VERSION,
      action_key: action_key.to_string(),
      lookup_key: lookup_key.to_string(),
      result_digest: result_digest.to_string(),
      output_manifest: manifest_id,
      output_tree: prepared.root.clone(),
      validation: validation_id,
      compiler_units,
    };
    let object_bytes = canonical_json(&object)?;
    let action_result = action_result_id(&object)?;
    let result_hex = validated_id_hex(&action_result, ACTION_RESULT_PREFIX)?;
    let incoming = match fs::symlink_metadata(self.root.join("results").join(result_hex)) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => 0,
      Ok(_) => {
        return Err(RailError::message(
          "local CAS action-result path is not a real directory",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => estimate_result_bytes(
        &prepared,
        manifest_bytes.len(),
        validation_bytes.len(),
        object_bytes.len(),
      )?,
      Err(error) => return Err(error.into()),
    };
    if incoming > MAX_RESULT_BYTES || incoming > self.max_bytes {
      return Err(RailError::message(format!(
        "verified action result is {incoming} bytes, above the local CAS limit"
      )));
    }
    self
      .ensure_capacity(incoming, Some(&action_result))
      .map_err(|error| RailError::message(format!("local CAS pre-publication capacity check failed: {error}")))?;
    let _lease = self
      .create_lease(&action_result)
      .map_err(|error| RailError::message(format!("local CAS lease creation failed: {error}")))?;
    let mut stats = self
      .publish_bundle(BundlePublication {
        action_result: &action_result,
        object: &object,
        object_bytes: &object_bytes,
        manifest,
        manifest_bytes: &manifest_bytes,
        validation,
        validation_bytes: &validation_bytes,
        prepared: &prepared,
      })
      .map_err(|error| RailError::message(format!("local CAS bundle publication failed: {error}")))?;
    self
      .publish_pin(action_key, lookup_key, &action_result)
      .map_err(|error| RailError::message(format!("local CAS pin publication failed: {error}")))?;
    self
      .ensure_capacity(0, Some(&action_result))
      .map_err(|error| RailError::message(format!("local CAS post-publication capacity check failed: {error}")))?;
    stats.action_result = Some(action_result);
    Ok(stats)
  }

  fn restore_inner(
    &self,
    action_key: &str,
    destination: &Path,
    stats: &mut ReadStats,
  ) -> Result<VerifiedResult, Fault> {
    let key_hex = validated_id_hex(action_key, ACTION_KEY_PREFIX).map_err(|error| Fault::corrupt(error.to_string()))?;
    let pin_path = self.root.join("pins").join(format!("{key_hex}.json"));
    let pin_metadata = match fs::symlink_metadata(&pin_path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        return Err(Fault::miss("action_not_found"));
      }
      Err(error) => return Err(Fault::corrupt(format!("pin_unreadable: {error}"))),
    };
    if !pin_metadata.is_file() || pin_metadata.file_type().is_symlink() || !has_single_link(&pin_metadata) {
      return Err(Fault::corrupt("pin_not_regular_file"));
    }
    let pin: ActionPin = read_canonical_json(&pin_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    if pin.version != CAS_VERSION {
      return Err(Fault::incompatible("pin_schema_version"));
    }
    if pin.action_key != action_key {
      return Err(Fault::corrupt("pin_action_key_mismatch"));
    }
    validate_lookup_key(&pin.lookup_key).map_err(|_| Fault::corrupt("pin_lookup_identity"))?;
    validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)
      .map_err(|_| Fault::corrupt("pin_action_result_identity"))?;
    let _lease = self
      .create_lease(&pin.action_result)
      .map_err(|error| Fault::corrupt(format!("lease_unavailable: {error}")))?;
    let verified = self.load_verified_result(action_key, &pin.action_result, stats)?;
    if verified.object.lookup_key != pin.lookup_key {
      return Err(Fault::corrupt("pin_lookup_binding_mismatch"));
    }
    self.materialize(&verified, destination, stats)?;
    Ok(verified)
  }
}

struct VerifiedResult {
  action_result: String,
  object: ActionResultObject,
  manifest: OutputManifest,
  validation: FastCacheValidation,
  trees: BTreeMap<String, TreeObject>,
  bundle: PathBuf,
}

fn cache_base() -> RailResult<PathBuf> {
  if let Some(base) = std::env::var_os(CACHE_BASE_ENV) {
    if base.is_empty() {
      return Err(RailError::message(format!("{CACHE_BASE_ENV} must not be empty")));
    }
    return Ok(PathBuf::from(base));
  }
  if let Some(cargo_home) = std::env::var_os("CARGO_HOME")
    && !cargo_home.is_empty()
  {
    return Ok(PathBuf::from(cargo_home));
  }
  let home = std::env::var_os("HOME")
    .filter(|home| !home.is_empty())
    .ok_or_else(|| RailError::message("local CAS needs CARGO_RAIL_CACHE_DIR, CARGO_HOME, or HOME"))?;
  Ok(PathBuf::from(home).join(".cargo"))
}

fn cache_max_bytes() -> RailResult<u64> {
  let Some(value) = std::env::var_os(CACHE_MAX_BYTES_ENV) else {
    return Ok(DEFAULT_CACHE_MAX_BYTES);
  };
  let value = value
    .to_str()
    .ok_or_else(|| RailError::message(format!("{CACHE_MAX_BYTES_ENV} is not valid UTF-8")))?;
  let parsed = value
    .parse::<u64>()
    .map_err(|error| RailError::message(format!("invalid {CACHE_MAX_BYTES_ENV} value '{value}': {error}")))?;
  if parsed == 0 {
    return Err(RailError::message(format!("{CACHE_MAX_BYTES_ENV} must be positive")));
  }
  Ok(parsed)
}

fn create_real_directory(parent: &Path, name: &str) -> RailResult<PathBuf> {
  let path = parent.join(name);
  match fs::create_dir(&path) {
    Ok(()) => {}
    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to create local CAS directory '{}': {error}",
        path.display()
      )));
    }
  }
  validate_real_directory(&path, "local CAS path")?;
  make_directory_private(&path)?;
  Ok(path)
}

#[cfg(unix)]
fn make_directory_private(path: &Path) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  Ok(())
}

#[cfg(not(unix))]
fn make_directory_private(_path: &Path) -> RailResult<()> {
  Ok(())
}

fn validate_real_directory(path: &Path, description: &str) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)
    .map_err(|error| RailError::message(format!("failed to inspect {description} '{}': {error}", path.display())))?;
  if metadata.is_dir() && !metadata.file_type().is_symlink() {
    Ok(())
  } else {
    Err(RailError::with_help(
      format!("{description} '{}' is not a real directory", path.display()),
      "remove the hostile path; cargo-rail will not follow cache symlinks",
    ))
  }
}

fn ensure_owner_marker(root: &Path) -> RailResult<()> {
  let marker = root.join("OWNER");
  match fs::symlink_metadata(&marker) {
    Ok(_) => return ensure_owner_marker_existing(root),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => return Err(error.into()),
  }
  if fs::read_dir(root)?.next().transpose()?.is_some() {
    // Another first writer may have published the marker after our initial
    // check. Accept only that exact ownership transition; never adopt an
    // unrelated nonempty directory.
    if fs::symlink_metadata(&marker).is_ok() {
      return ensure_owner_marker_existing(root);
    }
    return Err(RailError::with_help(
      format!(
        "local CAS root '{}' is nonempty but has no cargo-rail ownership marker",
        root.display()
      ),
      "choose an empty cache directory or remove the hostile pre-positioned path",
    ));
  }
  let parent = root
    .parent()
    .ok_or_else(|| RailError::message("local CAS root has no parent directory"))?;
  let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
  temporary.write_all(OWNER_MARKER)?;
  temporary.as_file().sync_all()?;
  match temporary.persist_noclobber(&marker) {
    Ok(_) => {
      sync_directory(root)?;
    }
    Err(_)
      if fs::symlink_metadata(&marker)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink()) => {}
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to create local CAS ownership marker '{}': {}",
        marker.display(),
        error.error
      )));
    }
  }
  ensure_owner_marker_existing(root)
}

fn validate_root_entries(root: &Path) -> RailResult<()> {
  let allowed = BTreeSet::from(["OWNER", "leases", "pins", "results", "staging"]);
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| RailError::message("local CAS root contains a non-UTF-8 entry"))?;
    if !allowed.contains(name.as_str()) {
      return Err(RailError::with_help(
        format!("local CAS root '{}' contains unexpected entry '{name}'", root.display()),
        "remove the hostile entry or choose a different CARGO_RAIL_CACHE_DIR",
      ));
    }
  }
  Ok(())
}

fn validate_action_key(action_key: &str) -> RailResult<()> {
  validated_id_hex(action_key, ACTION_KEY_PREFIX).map(|_| ())
}

fn validate_lookup_key(lookup_key: &str) -> RailResult<()> {
  validated_id_hex(lookup_key, LOOKUP_PREFIX).map(|_| ())
}

pub(super) fn validate_fast_identity(action_key: &str, lookup_key: &str) -> RailResult<()> {
  validate_action_key(action_key)?;
  validate_lookup_key(lookup_key)
}

fn validated_id_hex<'a>(identity: &'a str, prefix: &str) -> RailResult<&'a str> {
  let hex = identity
    .strip_prefix(prefix)
    .ok_or_else(|| RailError::message(format!("identity '{identity}' has the wrong domain or version")))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(RailError::message(format!(
      "identity '{identity}' is not canonical SHA-256"
    )));
  }
  Ok(hex)
}

fn canonical_json<T: Serialize>(value: &T) -> RailResult<Vec<u8>> {
  serde_json::to_vec(value).map_err(Into::into)
}

fn framed_identity(domain: &[u8], frames: &[(&[u8], &[u8])]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(domain);
  for (tag, value) in frames {
    hasher.update((tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
  }
  crate::instrumentation::record_hash_operation();
  let input_bytes = domain.len()
    + frames
      .iter()
      .map(|(tag, value)| 16usize.saturating_add(tag.len()).saturating_add(value.len()))
      .sum::<usize>();
  crate::instrumentation::record_hash_input_bytes(input_bytes);
  sha256_hex(hasher)
}

fn blob_id(content_digest: &str, bytes: u64) -> Result<String, Fault> {
  let content = content_digest
    .strip_prefix("sha256:")
    .ok_or_else(|| Fault::corrupt("file_content_digest_domain"))?;
  if content.len() != 64
    || !content
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(Fault::corrupt("file_content_digest_encoding"));
  }
  Ok(format!(
    "{BLOB_PREFIX}{}",
    framed_identity(
      b"cargo-rail-cas-blob\0",
      &[
        (b"version", CAS_VERSION.to_le_bytes().as_slice()),
        (b"bytes", bytes.to_le_bytes().as_slice()),
        (b"content-sha256", content.as_bytes()),
      ],
    )
  ))
}

fn tree_id(tree: &TreeObject) -> RailResult<String> {
  let version = tree.version.to_le_bytes();
  let entries = tree
    .entries
    .iter()
    .map(canonical_json)
    .collect::<RailResult<Vec<_>>>()?;
  let mut frames = Vec::<(&[u8], &[u8])>::with_capacity(entries.len() + 1);
  frames.push((b"version", &version));
  frames.extend(entries.iter().map(|entry| (b"entry".as_slice(), entry.as_slice())));
  Ok(format!(
    "{TREE_PREFIX}{}",
    framed_identity(b"cargo-rail-cas-tree\0", &frames)
  ))
}

fn action_result_id(object: &ActionResultObject) -> RailResult<String> {
  let version = object.version.to_le_bytes();
  let compiler_units = (object.compiler_units as u64).to_le_bytes();
  Ok(format!(
    "{ACTION_RESULT_PREFIX}{}",
    framed_identity(
      b"cargo-rail-cas-action-result\0",
      &[
        (b"version", &version),
        (b"action-key", object.action_key.as_bytes()),
        (b"lookup-key", object.lookup_key.as_bytes()),
        (b"result-digest", object.result_digest.as_bytes()),
        (b"output-manifest", object.output_manifest.as_bytes()),
        (b"output-tree", object.output_tree.as_bytes()),
        (b"validation", object.validation.as_bytes()),
        (b"compiler-units", &compiler_units),
      ],
    )
  ))
}

fn validation_id(bytes: &[u8]) -> String {
  let version = CAS_VERSION.to_le_bytes();
  format!(
    "{VALIDATION_PREFIX}{}",
    framed_identity(
      b"cargo-rail-cas-validation\0",
      &[(b"version", &version), (b"manifest", bytes)],
    )
  )
}

fn fault_to_error(fault: Fault) -> RailError {
  RailError::message(format!(
    "local CAS {}: {}",
    match fault.kind {
      FaultKind::Miss => "miss",
      FaultKind::Corrupt => "corruption",
      FaultKind::Incompatible => "incompatibility",
    },
    fault.reason
  ))
}

fn validate_manifest(manifest: &OutputManifest) -> Result<(), Fault> {
  if manifest.version != super::OUTPUT_MANIFEST_VERSION {
    return Err(Fault::incompatible("output_manifest_schema_version"));
  }
  if manifest.entries.len() > MAX_ENTRIES {
    return Err(Fault::corrupt("output_manifest_entry_limit"));
  }
  let expected_digest = super::output_manifest_digest(&manifest.entries)
    .map_err(|error| Fault::corrupt(format!("output_manifest_identity: {error}")))?;
  if expected_digest != manifest.digest {
    return Err(Fault::corrupt("output_manifest_digest_mismatch"));
  }
  validated_id_hex(&manifest.digest, MANIFEST_PREFIX)
    .map_err(|_| Fault::corrupt("output_manifest_identity_encoding"))?;

  let mut files = 0usize;
  let mut directories = 0usize;
  let mut symlinks = 0usize;
  let mut bytes = 0u64;
  let mut previous = None::<&str>;
  let mut kinds = BTreeMap::<String, &'static str>::new();
  for entry in &manifest.entries {
    validate_logical_path(&entry.path)?;
    if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
      return Err(Fault::corrupt("output_manifest_paths_not_strictly_sorted"));
    }
    previous = Some(&entry.path);
    if !entry.path.starts_with("target/")
      && entry.path != "target"
      && !entry.path.starts_with("build/")
      && entry.path != "build"
    {
      return Err(Fault::corrupt("output_manifest_path_outside_declared_roots"));
    }
    let kind = match &entry.kind {
      OutputEntryKind::Directory { mode } => {
        validate_mode(*mode, true)?;
        directories += 1;
        "directory"
      }
      OutputEntryKind::File {
        digest,
        mode,
        bytes: length,
      } => {
        validate_content_digest(digest)?;
        validate_mode(*mode, false)?;
        files += 1;
        bytes = bytes
          .checked_add(*length)
          .ok_or_else(|| Fault::corrupt("output_manifest_byte_overflow"))?;
        "file"
      }
      OutputEntryKind::Symlink { target } => {
        validate_symlink(&entry.path, target)?;
        symlinks += 1;
        "symlink"
      }
    };
    kinds.insert(entry.path.clone(), kind);
  }
  for entry in &manifest.entries {
    if let Some(parent) = Path::new(&entry.path).parent()
      && !parent.as_os_str().is_empty()
    {
      let parent = crate::utils::path_to_git_format(parent);
      if kinds.get(&parent) != Some(&"directory") {
        return Err(Fault::corrupt("output_manifest_parent_not_directory"));
      }
    }
  }
  if files != manifest.files
    || directories != manifest.directories
    || symlinks != manifest.symlinks
    || bytes != manifest.bytes
  {
    return Err(Fault::corrupt("output_manifest_summary_mismatch"));
  }
  if manifest.bytes > MAX_RESULT_BYTES {
    return Err(Fault::corrupt("output_manifest_byte_limit"));
  }
  Ok(())
}

fn validate_logical_path(path: &str) -> Result<(), Fault> {
  if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
    return Err(Fault::corrupt("unsafe_output_path"));
  }
  let parsed = crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_output_path"))?;
  if parsed.as_str() != path {
    return Err(Fault::corrupt("noncanonical_output_path"));
  }
  for component in path.split('/') {
    validate_name(component)?;
  }
  Ok(())
}

fn validate_name(name: &str) -> Result<(), Fault> {
  if name.is_empty()
    || !name.is_ascii()
    || name.len() > MAX_NAME_BYTES
    || name == "."
    || name == ".."
    || name.contains(['/', '\\', '\0', ':'])
    || name.ends_with(['.', ' '])
    || name.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
  {
    return Err(Fault::corrupt("unsafe_output_name"));
  }
  let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
  let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
    || stem
      .strip_prefix("COM")
      .or_else(|| stem.strip_prefix("LPT"))
      .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
  if reserved {
    return Err(Fault::corrupt("platform_reserved_output_name"));
  }
  Ok(())
}

fn validate_content_digest(digest: &str) -> Result<(), Fault> {
  let hex = digest
    .strip_prefix("sha256:")
    .ok_or_else(|| Fault::corrupt("file_content_digest_domain"))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(Fault::corrupt("file_content_digest_encoding"));
  }
  Ok(())
}

fn validate_mode(mode: u32, directory: bool) -> Result<(), Fault> {
  let allowed = if directory {
    mode == 0o755
  } else {
    mode == 0o644 || mode == 0o755
  };
  if allowed {
    Ok(())
  } else {
    Err(Fault::incompatible("unsupported_output_mode"))
  }
}

fn validate_symlink(path: &str, target: &str) -> Result<(), Fault> {
  if target.is_empty() || target.contains(['\0', '\\']) || target.len() > MAX_PATH_BYTES {
    return Err(Fault::corrupt("unsafe_symlink_target"));
  }
  let path = crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_symlink_path"))?;
  if super::symlink_target_escapes(&path, target) {
    return Err(Fault::corrupt("symlink_target_escape"));
  }
  Ok(())
}

fn prepare_tree(manifest: &OutputManifest, source_root: &Path) -> Result<PreparedTree, Fault> {
  let mut root = BuildDirectory::default();
  let mut directories = manifest
    .entries
    .iter()
    .filter_map(|entry| match entry.kind {
      OutputEntryKind::Directory { mode } => Some((entry.path.as_str(), mode)),
      _ => None,
    })
    .collect::<Vec<_>>();
  directories.sort_by_key(|(path, _)| path.split('/').count());
  for (path, mode) in directories {
    let components = path.split('/').collect::<Vec<_>>();
    insert_directory(&mut root, &components, mode)?;
  }
  for entry in &manifest.entries {
    let components = entry.path.split('/').collect::<Vec<_>>();
    match &entry.kind {
      OutputEntryKind::Directory { .. } => {}
      OutputEntryKind::File { digest, mode, bytes } => {
        insert_leaf(
          &mut root,
          &components,
          BuildNode::File {
            source: source_root.join(&entry.path),
            content_digest: digest.clone(),
            bytes: *bytes,
            mode: *mode,
          },
        )?;
      }
      OutputEntryKind::Symlink { target } => {
        let source = source_root.join(&entry.path);
        let directory = fs::metadata(&source).is_ok_and(|metadata| metadata.is_dir());
        insert_leaf(
          &mut root,
          &components,
          BuildNode::Symlink {
            target: target.clone(),
            directory,
          },
        )?;
      }
    }
  }
  let mut trees = BTreeMap::new();
  let mut blobs = BTreeMap::new();
  let root = finalize_tree(root, &mut trees, &mut blobs, 0)?;
  Ok(PreparedTree { root, trees, blobs })
}

fn insert_directory(directory: &mut BuildDirectory, components: &[&str], mode: u32) -> Result<(), Fault> {
  let (name, parents) = components
    .split_last()
    .ok_or_else(|| Fault::corrupt("empty_output_path"))?;
  let parent = directory_at_mut(directory, parents)?;
  match parent.children.get(*name) {
    Some(BuildNode::Directory { mode: existing, .. }) if *existing == mode => Ok(()),
    Some(_) => Err(Fault::corrupt("duplicate_or_colliding_output_path")),
    None => {
      parent.children.insert(
        (*name).to_string(),
        BuildNode::Directory {
          mode,
          contents: BuildDirectory::default(),
        },
      );
      Ok(())
    }
  }
}

fn insert_leaf(directory: &mut BuildDirectory, components: &[&str], node: BuildNode) -> Result<(), Fault> {
  let (name, parents) = components
    .split_last()
    .ok_or_else(|| Fault::corrupt("empty_output_path"))?;
  let parent = directory_at_mut(directory, parents)?;
  if parent.children.insert((*name).to_string(), node).is_some() {
    return Err(Fault::corrupt("duplicate_or_colliding_output_path"));
  }
  Ok(())
}

fn directory_at_mut<'a>(
  mut directory: &'a mut BuildDirectory,
  components: &[&str],
) -> Result<&'a mut BuildDirectory, Fault> {
  for component in components {
    directory = match directory.children.get_mut(*component) {
      Some(BuildNode::Directory { contents, .. }) => contents,
      _ => return Err(Fault::corrupt("missing_output_parent_directory")),
    };
  }
  Ok(directory)
}

fn finalize_tree(
  directory: BuildDirectory,
  trees: &mut BTreeMap<String, Vec<u8>>,
  blobs: &mut BTreeMap<String, PreparedBlob>,
  depth: usize,
) -> Result<String, Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("output_tree_depth_limit"));
  }
  validate_platform_collisions(directory.children.keys().map(String::as_str))?;
  let mut entries = Vec::with_capacity(directory.children.len());
  for (name, node) in directory.children {
    let kind = match node {
      BuildNode::File {
        source,
        content_digest,
        bytes,
        mode,
      } => {
        let blob = blob_id(&content_digest, bytes)?;
        if let Some(existing) = blobs.get(&blob) {
          if existing.content_digest != content_digest || existing.bytes != bytes {
            return Err(Fault::corrupt("blob_identity_collision"));
          }
        } else {
          blobs.insert(
            blob.clone(),
            PreparedBlob {
              source,
              content_digest: content_digest.clone(),
              bytes,
            },
          );
        }
        TreeEntryKind::File {
          blob,
          content_digest,
          bytes,
          mode,
        }
      }
      BuildNode::Directory { mode, contents } => TreeEntryKind::Directory {
        tree: finalize_tree(contents, trees, blobs, depth + 1)?,
        mode,
      },
      BuildNode::Symlink { target, directory } => TreeEntryKind::Symlink { target, directory },
    };
    entries.push(TreeEntry { name, kind });
  }
  let object = TreeObject {
    version: CAS_VERSION,
    entries,
  };
  let identity = tree_id(&object).map_err(|error| Fault::corrupt(format!("tree_identity: {error}")))?;
  let bytes = canonical_json(&object).map_err(|error| Fault::corrupt(format!("tree_encoding: {error}")))?;
  if let Some(existing) = trees.insert(identity.clone(), bytes.clone())
    && existing != bytes
  {
    return Err(Fault::corrupt("tree_identity_collision"));
  }
  Ok(identity)
}

fn validate_platform_collisions<'a>(names: impl Iterator<Item = &'a str>) -> Result<(), Fault> {
  let mut portable = BTreeSet::new();
  for name in names {
    validate_name(name)?;
    let folded = name.to_ascii_lowercase();
    if !portable.insert(folded) {
      return Err(Fault::corrupt("platform_colliding_output_names"));
    }
  }
  Ok(())
}

fn estimate_result_bytes(
  prepared: &PreparedTree,
  manifest: usize,
  validation: usize,
  action_result: usize,
) -> RailResult<u64> {
  let blob_bytes = prepared.blobs.values().try_fold(0u64, |total, blob| {
    total
      .checked_add(blob.bytes)
      .ok_or_else(|| RailError::message("local CAS result size overflow"))
  })?;
  let tree_bytes = prepared.trees.values().try_fold(0u64, |total, tree| {
    total
      .checked_add(tree.len() as u64)
      .ok_or_else(|| RailError::message("local CAS result size overflow"))
  })?;
  blob_bytes
    .checked_add(tree_bytes)
    .and_then(|total| total.checked_add(manifest as u64))
    .and_then(|total| total.checked_add(validation as u64))
    .and_then(|total| total.checked_add(action_result as u64))
    .ok_or_else(|| RailError::message("local CAS result size overflow"))
}

impl LocalCas {
  fn load_verified_result(
    &self,
    action_key: &str,
    action_result: &str,
    stats: &mut ReadStats,
  ) -> Result<VerifiedResult, Fault> {
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_identity_encoding"))?;
    let bundle = self.root.join("results").join(result_hex);
    validate_bundle_directory(&bundle)?;
    validate_bundle_root_entries(&bundle)?;

    let object_path = bundle.join("action-result.json");
    let object: ActionResultObject = read_canonical_json(&object_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    if object.version != CAS_VERSION {
      return Err(Fault::incompatible("action_result_schema_version"));
    }
    if object.action_key != action_key {
      return Err(Fault::corrupt("action_result_action_key_mismatch"));
    }
    validate_lookup_key(&object.lookup_key).map_err(|_| Fault::corrupt("action_result_lookup_identity"))?;
    if action_result_id(&object).map_err(|error| Fault::corrupt(format!("action_result_identity: {error}")))?
      != action_result
    {
      return Err(Fault::corrupt("action_result_digest_mismatch"));
    }
    validated_id_hex(&object.output_manifest, MANIFEST_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_manifest_identity"))?;
    validated_id_hex(&object.output_tree, TREE_PREFIX).map_err(|_| Fault::corrupt("action_result_tree_identity"))?;
    let validation_hex = validated_id_hex(&object.validation, VALIDATION_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_validation_identity"))?;

    let manifest_path = bundle.join("manifests").join(format!(
      "{}.json",
      validated_id_hex(&object.output_manifest, MANIFEST_PREFIX)
        .map_err(|_| { Fault::corrupt("action_result_manifest_identity") })?
    ));
    let manifest: OutputManifest = read_canonical_json(&manifest_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    validate_manifest(&manifest)?;
    if manifest.digest != object.output_manifest {
      return Err(Fault::corrupt("action_result_manifest_mismatch"));
    }
    if super::hermetic_result_digest(action_key, manifest.digest()) != object.result_digest {
      return Err(Fault::corrupt("action_result_result_digest_mismatch"));
    }

    let validation_path = bundle.join("validations").join(format!("{validation_hex}.json"));
    let validation: FastCacheValidation = read_canonical_json(&validation_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    let validation_bytes =
      canonical_json(&validation).map_err(|error| Fault::corrupt(format!("validation_encoding: {error}")))?;
    if validation_id(&validation_bytes) != object.validation {
      return Err(Fault::corrupt("validation_digest_mismatch"));
    }
    if validation.action_key != object.action_key || validation.lookup_key != object.lookup_key {
      return Err(Fault::corrupt("validation_action_binding_mismatch"));
    }
    validation
      .validate_object()
      .map_err(|error| Fault::corrupt(format!("validation_object: {error}")))?;

    let mut trees = BTreeMap::new();
    let mut loading = BTreeSet::new();
    let mut tree_entries = 0usize;
    load_tree_recursive(
      &bundle,
      &object.output_tree,
      0,
      &mut tree_entries,
      &mut loading,
      &mut trees,
      stats,
    )?;
    let mut flattened = BTreeMap::new();
    let mut blobs = BTreeSet::new();
    flatten_tree(&object.output_tree, "", &trees, &mut flattened, &mut blobs, 0)?;
    let flattened = flattened
      .into_iter()
      .map(|(path, kind)| OutputEntry { path, kind })
      .collect::<Vec<_>>();
    if flattened != manifest.entries {
      return Err(Fault::corrupt("output_tree_manifest_mismatch"));
    }
    validate_object_directory(
      &bundle.join("trees"),
      &trees.keys().cloned().collect(),
      TREE_PREFIX,
      "json",
    )?;
    validate_object_directory(&bundle.join("blobs"), &blobs, BLOB_PREFIX, "blob")?;
    validate_object_directory(
      &bundle.join("manifests"),
      &BTreeSet::from([object.output_manifest.clone()]),
      MANIFEST_PREFIX,
      "json",
    )?;
    validate_object_directory(
      &bundle.join("validations"),
      &BTreeSet::from([object.validation.clone()]),
      VALIDATION_PREFIX,
      "json",
    )?;
    for blob in &blobs {
      let path = bundle.join("blobs").join(format!(
        "{}.blob",
        validated_id_hex(blob, BLOB_PREFIX).map_err(|_| Fault::corrupt("blob_identity"))?
      ));
      let metadata = fs::symlink_metadata(&path).map_err(|error| Fault::corrupt(format!("blob_missing: {error}")))?;
      if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !has_single_link(&metadata)
        || metadata.len() > MAX_RESULT_BYTES
      {
        return Err(Fault::corrupt("blob_not_bounded_regular_file"));
      }
    }
    Ok(VerifiedResult {
      action_result: action_result.to_string(),
      object,
      manifest,
      validation,
      trees,
      bundle,
    })
  }

  fn materialize(&self, verified: &VerifiedResult, destination: &Path, stats: &mut ReadStats) -> Result<(), Fault> {
    let parent = destination
      .parent()
      .ok_or_else(|| Fault::corrupt("materialization_root_has_no_parent"))?;
    validate_real_directory_fault(parent, "materialization parent")?;
    match fs::symlink_metadata(destination) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Ok(_) => return Err(Fault::corrupt("materialization_destination_prepositioned")),
      Err(error) => {
        return Err(Fault::corrupt(format!(
          "materialization_destination_unreadable: {error}"
        )));
      }
    }
    let temporary = tempfile::Builder::new()
      .prefix("restore-")
      .tempdir_in(parent)
      .map_err(|error| Fault::corrupt(format!("materialization_staging_unavailable: {error}")))?;
    let payload = temporary.path().join("output");
    fs::create_dir(&payload).map_err(|error| Fault::corrupt(format!("materialization_root_create: {error}")))?;
    materialize_tree(
      &verified.bundle,
      &verified.object.output_tree,
      &verified.trees,
      &payload,
      stats,
      0,
    )?;
    verified
      .manifest
      .validate_unchanged(&payload)
      .map_err(|error| Fault::corrupt(format!("materialized_manifest_validation: {error}")))?;
    sync_output_tree(&payload).map_err(|error| Fault::corrupt(format!("materialized_tree_sync: {error}")))?;
    fs::rename(&payload, destination)
      .map_err(|error| Fault::corrupt(format!("materialization_atomic_publish: {error}")))?;
    sync_directory(parent).map_err(|error| Fault::corrupt(format!("materialization_parent_sync: {error}")))?;
    Ok(())
  }
}

fn read_canonical_json<T>(path: &Path, limit: u64, stats: &mut ReadStats) -> Result<T, Fault>
where
  T: for<'de> Deserialize<'de> + Serialize,
{
  let bytes = read_bounded_file(path, limit, stats)?;
  let decoded =
    serde_json::from_slice::<T>(&bytes).map_err(|error| Fault::corrupt(format!("object_decode: {error}")))?;
  let canonical = canonical_json(&decoded).map_err(|error| Fault::corrupt(format!("object_encode: {error}")))?;
  if canonical != bytes {
    return Err(Fault::corrupt("object_encoding_not_canonical"));
  }
  Ok(decoded)
}

fn read_bounded_file(path: &Path, limit: u64, stats: &mut ReadStats) -> Result<Vec<u8>, Fault> {
  let metadata = fs::symlink_metadata(path).map_err(|error| Fault::corrupt(format!("object_missing: {error}")))?;
  if !metadata.is_file() || metadata.file_type().is_symlink() || !has_single_link(&metadata) {
    return Err(Fault::corrupt("object_not_regular_file"));
  }
  if metadata.len() > limit {
    return Err(Fault::corrupt("object_size_limit"));
  }
  let total = stats
    .bytes
    .checked_add(metadata.len())
    .ok_or_else(|| Fault::corrupt("result_byte_overflow"))?;
  if total > MAX_LOOKUP_BYTES {
    return Err(Fault::corrupt("result_byte_limit"));
  }
  let mut file = File::open(path).map_err(|error| Fault::corrupt(format!("object_open: {error}")))?;
  let opened = file
    .metadata()
    .map_err(|error| Fault::corrupt(format!("object_metadata: {error}")))?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != metadata.len() {
    return Err(Fault::corrupt("object_changed_before_read"));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  (&mut file)
    .take(metadata.len().saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(|error| Fault::corrupt(format!("object_read: {error}")))?;
  if bytes.len() as u64 != metadata.len() {
    return Err(Fault::corrupt("object_truncated_during_read"));
  }
  stats.bytes = total;
  crate::instrumentation::record_cas_read(bytes.len() as u64);
  Ok(bytes)
}

fn validate_bundle_directory(path: &Path) -> Result<(), Fault> {
  validate_real_directory_fault(path, "action result bundle")
}

fn validate_real_directory_fault(path: &Path, description: &str) -> Result<(), Fault> {
  let metadata =
    fs::symlink_metadata(path).map_err(|error| Fault::corrupt(format!("{description}_missing: {error}")))?;
  if metadata.is_dir() && !metadata.file_type().is_symlink() {
    Ok(())
  } else {
    Err(Fault::corrupt(format!("{description}_not_real_directory")))
  }
}

fn validate_bundle_root_entries(bundle: &Path) -> Result<(), Fault> {
  let expected = ["action-result.json", "blobs", "manifests", "trees", "validations"]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
  let mut actual = BTreeSet::new();
  for entry in fs::read_dir(bundle).map_err(|error| Fault::corrupt(format!("bundle_read: {error}")))? {
    let entry = entry.map_err(|error| Fault::corrupt(format!("bundle_entry: {error}")))?;
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| Fault::corrupt("bundle_non_utf8_entry"))?;
    actual.insert(name);
    if actual.len() > expected.len() {
      return Err(Fault::corrupt("bundle_entries_mismatch"));
    }
  }
  if actual != expected {
    return Err(Fault::corrupt("bundle_entries_mismatch"));
  }
  for directory in ["blobs", "manifests", "trees", "validations"] {
    validate_real_directory_fault(&bundle.join(directory), "bundle object directory")?;
  }
  Ok(())
}

fn load_tree_recursive(
  bundle: &Path,
  identity: &str,
  depth: usize,
  total_entries: &mut usize,
  loading: &mut BTreeSet<String>,
  loaded: &mut BTreeMap<String, TreeObject>,
  stats: &mut ReadStats,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  if loaded.contains_key(identity) {
    return Ok(());
  }
  if !loading.insert(identity.to_string()) {
    return Err(Fault::corrupt("tree_cycle"));
  }
  let hex = validated_id_hex(identity, TREE_PREFIX).map_err(|_| Fault::corrupt("tree_identity_encoding"))?;
  let path = bundle.join("trees").join(format!("{hex}.json"));
  let tree: TreeObject = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, stats)?;
  stats.objects = stats.objects.saturating_add(1);
  if tree.version != CAS_VERSION {
    return Err(Fault::incompatible("tree_schema_version"));
  }
  if tree.entries.len() > MAX_ENTRIES {
    return Err(Fault::corrupt("tree_entry_limit"));
  }
  *total_entries = total_entries
    .checked_add(tree.entries.len())
    .ok_or_else(|| Fault::corrupt("tree_entry_overflow"))?;
  if *total_entries > MAX_ENTRIES {
    return Err(Fault::corrupt("tree_entry_limit"));
  }
  let actual = tree_id(&tree).map_err(|error| Fault::corrupt(format!("tree_identity: {error}")))?;
  if actual != identity {
    return Err(Fault::corrupt("tree_digest_mismatch"));
  }
  let mut previous = None::<&str>;
  validate_platform_collisions(tree.entries.iter().map(|entry| entry.name.as_str()))?;
  for entry in &tree.entries {
    if previous.is_some_and(|previous| previous >= entry.name.as_str()) {
      return Err(Fault::corrupt("tree_entries_not_strictly_sorted"));
    }
    previous = Some(&entry.name);
    match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        validate_mode(*mode, false)?;
        validate_content_digest(content_digest)?;
        if blob_id(content_digest, *bytes)? != *blob {
          return Err(Fault::corrupt("blob_reference_mismatch"));
        }
      }
      TreeEntryKind::Directory { tree, mode } => {
        validate_mode(*mode, true)?;
        load_tree_recursive(bundle, tree, depth + 1, total_entries, loading, loaded, stats)?;
      }
      TreeEntryKind::Symlink { target, .. } => {
        if target.is_empty() || target.contains(['\0', '\\']) || target.len() > MAX_PATH_BYTES {
          return Err(Fault::corrupt("unsafe_symlink_target"));
        }
      }
    }
  }
  loading.remove(identity);
  loaded.insert(identity.to_string(), tree);
  Ok(())
}

fn flatten_tree(
  identity: &str,
  prefix: &str,
  trees: &BTreeMap<String, TreeObject>,
  output: &mut BTreeMap<String, OutputEntryKind>,
  blobs: &mut BTreeSet<String>,
  depth: usize,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  let tree = trees
    .get(identity)
    .ok_or_else(|| Fault::corrupt("tree_reference_missing"))?;
  for entry in &tree.entries {
    let path = if prefix.is_empty() {
      entry.name.clone()
    } else {
      format!("{prefix}/{}", entry.name)
    };
    validate_logical_path(&path)?;
    let kind = match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        blobs.insert(blob.clone());
        OutputEntryKind::File {
          digest: content_digest.clone(),
          mode: *mode,
          bytes: *bytes,
        }
      }
      TreeEntryKind::Directory { tree, mode } => {
        flatten_tree(tree, &path, trees, output, blobs, depth + 1)?;
        OutputEntryKind::Directory { mode: *mode }
      }
      TreeEntryKind::Symlink { target, .. } => {
        validate_symlink(&path, target)?;
        OutputEntryKind::Symlink { target: target.clone() }
      }
    };
    if output.insert(path, kind).is_some() {
      return Err(Fault::corrupt("duplicate_tree_path"));
    }
    if output.len() > MAX_ENTRIES {
      return Err(Fault::corrupt("tree_entry_limit"));
    }
  }
  Ok(())
}

fn validate_object_directory(
  directory: &Path,
  expected: &BTreeSet<String>,
  prefix: &str,
  extension: &str,
) -> Result<(), Fault> {
  let expected = expected
    .iter()
    .map(|identity| {
      validated_id_hex(identity, prefix)
        .map(|hex| format!("{hex}.{extension}"))
        .map_err(|_| Fault::corrupt("object_identity_encoding"))
    })
    .collect::<Result<BTreeSet<_>, _>>()?;
  let mut actual = BTreeSet::new();
  for entry in fs::read_dir(directory).map_err(|error| Fault::corrupt(format!("object_directory_read: {error}")))? {
    let entry = entry.map_err(|error| Fault::corrupt(format!("object_directory_entry: {error}")))?;
    let metadata =
      fs::symlink_metadata(entry.path()).map_err(|error| Fault::corrupt(format!("object_metadata: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || !has_single_link(&metadata) {
      return Err(Fault::corrupt("object_directory_contains_non_file"));
    }
    actual.insert(
      entry
        .file_name()
        .into_string()
        .map_err(|_| Fault::corrupt("object_directory_non_utf8_name"))?,
    );
    if actual.len() > MAX_ENTRIES {
      return Err(Fault::corrupt("object_directory_entry_limit"));
    }
  }
  if actual != expected {
    return Err(Fault::corrupt("object_directory_membership_mismatch"));
  }
  Ok(())
}

fn materialize_tree(
  bundle: &Path,
  identity: &str,
  trees: &BTreeMap<String, TreeObject>,
  destination: &Path,
  stats: &mut ReadStats,
  depth: usize,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  let tree = trees
    .get(identity)
    .ok_or_else(|| Fault::corrupt("tree_reference_missing"))?;
  validate_real_directory_fault(destination, "materialization directory")?;
  for entry in &tree.entries {
    let path = destination.join(&entry.name);
    match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        materialize_blob(bundle, blob, content_digest, *bytes, *mode, &path, stats)?;
      }
      TreeEntryKind::Directory { tree, mode } => {
        fs::create_dir(&path).map_err(|error| Fault::corrupt(format!("directory_materialization: {error}")))?;
        materialize_tree(bundle, tree, trees, &path, stats, depth + 1)?;
        set_exact_mode(&path, *mode)?;
      }
      TreeEntryKind::Symlink { target, directory } => {
        let target_path = Path::new(target);
        create_materialized_symlink(target_path, &path, *directory)?;
      }
    }
  }
  Ok(())
}

fn materialize_blob(
  bundle: &Path,
  identity: &str,
  content_digest: &str,
  expected_bytes: u64,
  mode: u32,
  destination: &Path,
  stats: &mut ReadStats,
) -> Result<(), Fault> {
  let hex = validated_id_hex(identity, BLOB_PREFIX).map_err(|_| Fault::corrupt("blob_identity_encoding"))?;
  let source = bundle.join("blobs").join(format!("{hex}.blob"));
  let metadata = fs::symlink_metadata(&source).map_err(|error| Fault::corrupt(format!("blob_missing: {error}")))?;
  if !metadata.is_file()
    || metadata.file_type().is_symlink()
    || !has_single_link(&metadata)
    || metadata.len() != expected_bytes
  {
    return Err(Fault::corrupt("blob_metadata_mismatch"));
  }
  let maximum_read = expected_bytes.saturating_add(1);
  if stats
    .bytes
    .checked_add(maximum_read)
    .is_none_or(|total| total > MAX_LOOKUP_BYTES)
  {
    return Err(Fault::corrupt("result_byte_limit"));
  }
  let mut input = File::open(&source).map_err(|error| Fault::corrupt(format!("blob_open: {error}")))?;
  let opened = input
    .metadata()
    .map_err(|error| Fault::corrupt(format!("blob_opened_metadata: {error}")))?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != expected_bytes {
    return Err(Fault::corrupt("blob_changed_before_read"));
  }
  let mut output = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(destination)
    .map_err(|error| Fault::corrupt(format!("blob_destination_create: {error}")))?;
  let mut digest = Sha256::new();
  let mut copied = 0u64;
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let remaining = maximum_read.saturating_sub(copied);
    if remaining == 0 {
      break;
    }
    let read_capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
    let read = input
      .read(&mut buffer[..read_capacity])
      .map_err(|error| Fault::corrupt(format!("blob_read: {error}")))?;
    if read == 0 {
      break;
    }
    output
      .write_all(&buffer[..read])
      .map_err(|error| Fault::corrupt(format!("blob_write: {error}")))?;
    digest.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  output
    .sync_all()
    .map_err(|error| Fault::corrupt(format!("blob_sync: {error}")))?;
  let actual_digest = format!("sha256:{}", sha256_hex(digest));
  stats.bytes = stats.bytes.saturating_add(copied);
  crate::instrumentation::record_cas_read(copied);
  if copied != expected_bytes || actual_digest != content_digest || blob_id(&actual_digest, copied)? != identity {
    return Err(Fault::corrupt("blob_digest_mismatch"));
  }
  stats.objects = stats.objects.saturating_add(1);
  stats.restored = stats.restored.saturating_add(copied);
  crate::instrumentation::record_cas_restore(copied);
  crate::instrumentation::record_hash(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  set_exact_mode(destination, mode)
}

#[cfg(unix)]
fn set_exact_mode(path: &Path, mode: u32) -> Result<(), Fault> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::set_permissions(path, fs::Permissions::from_mode(mode))
    .map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
}

#[cfg(not(unix))]
fn set_exact_mode(path: &Path, mode: u32) -> Result<(), Fault> {
  let mut permissions = fs::metadata(path)
    .map_err(|error| Fault::corrupt(format!("output_mode_metadata: {error}")))?
    .permissions();
  permissions.set_readonly(mode & 0o200 == 0);
  fs::set_permissions(path, permissions).map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
}

#[cfg(unix)]
fn create_materialized_symlink(target: &Path, destination: &Path, _directory: bool) -> Result<(), Fault> {
  std::os::unix::fs::symlink(target, destination)
    .map_err(|error| Fault::corrupt(format!("symlink_materialization: {error}")))
}

#[cfg(windows)]
fn create_materialized_symlink(target: &Path, destination: &Path, directory: bool) -> Result<(), Fault> {
  let result = if directory {
    std::os::windows::fs::symlink_dir(target, destination)
  } else {
    std::os::windows::fs::symlink_file(target, destination)
  };
  result.map_err(|error| Fault::corrupt(format!("symlink_materialization: {error}")))
}

#[cfg(not(any(unix, windows)))]
fn create_materialized_symlink(_target: &Path, _destination: &Path, _directory: bool) -> Result<(), Fault> {
  Err(Fault::incompatible("symlink_materialization_unsupported"))
}

fn sync_output_tree(root: &Path) -> RailResult<()> {
  let mut directories = Vec::new();
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
      continue;
    }
    if metadata.is_dir() {
      directories.push(path.clone());
      for entry in fs::read_dir(path)? {
        pending.push(entry?.path());
      }
    }
  }
  directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
  for directory in directories {
    sync_directory(&directory)?;
  }
  Ok(())
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

impl LocalCas {
  fn publish_bundle(&self, publication: BundlePublication<'_>) -> RailResult<StoreStats> {
    let BundlePublication {
      action_result,
      object,
      object_bytes,
      manifest,
      manifest_bytes,
      validation,
      validation_bytes,
      prepared,
    } = publication;
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let destination = self.root.join("results").join(result_hex);
    match fs::symlink_metadata(&destination) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
        let mut read = ReadStats::default();
        self
          .load_verified_result(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        return Ok(StoreStats::default());
      }
      Ok(_) => {
        return Err(RailError::with_help(
          format!("local CAS result '{}' is not a real directory", destination.display()),
          "run `cargo rail clean --cache`; cargo-rail will not replace a hostile object",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }

    let staging = self.root.join("staging");
    let temporary = tempfile::Builder::new().prefix("result-").tempdir_in(&staging)?;
    let payload = temporary.path().join("payload");
    fs::create_dir(&payload)?;
    make_directory_private(&payload)?;
    for name in ["blobs", "manifests", "trees", "validations"] {
      let directory = payload.join(name);
      fs::create_dir(&directory)?;
      make_directory_private(&directory)?;
    }
    let mut stats = StoreStats::default();
    write_new_synced(&payload.join("action-result.json"), object_bytes)?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(object_bytes.len() as u64);

    let manifest_hex = validated_id_hex(manifest.digest(), MANIFEST_PREFIX)?;
    write_new_synced(
      &payload.join("manifests").join(format!("{manifest_hex}.json")),
      manifest_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(manifest_bytes.len() as u64);

    let validation_identity = validation_id(validation_bytes);
    if validation_identity != object.validation || canonical_json(validation)? != validation_bytes {
      return Err(RailError::message(
        "local CAS validation object changed before publication",
      ));
    }
    let validation_hex = validated_id_hex(&validation_identity, VALIDATION_PREFIX)?;
    write_new_synced(
      &payload.join("validations").join(format!("{validation_hex}.json")),
      validation_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(validation_bytes.len() as u64);

    for (identity, bytes) in &prepared.trees {
      let hex = validated_id_hex(identity, TREE_PREFIX)?;
      write_new_synced(&payload.join("trees").join(format!("{hex}.json")), bytes)?;
      stats.objects_written = stats.objects_written.saturating_add(1);
      stats.bytes_written = stats.bytes_written.saturating_add(bytes.len() as u64);
    }
    for (identity, blob) in &prepared.blobs {
      let hex = validated_id_hex(identity, BLOB_PREFIX)?;
      let written = copy_blob_verified(blob, identity, &payload.join("blobs").join(format!("{hex}.blob")))?;
      stats.objects_written = stats.objects_written.saturating_add(1);
      stats.bytes_written = stats.bytes_written.saturating_add(written);
    }
    sync_directory(&payload.join("blobs"))?;
    sync_directory(&payload.join("manifests"))?;
    sync_directory(&payload.join("trees"))?;
    sync_directory(&payload.join("validations"))?;
    sync_directory(&payload)?;

    match fs::rename(&payload, &destination) {
      Ok(()) => {
        sync_directory(&self.root.join("results"))?;
        crate::instrumentation::record_cas_write(stats.bytes_written, stats.objects_written);
        Ok(stats)
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink()) =>
      {
        let mut read = ReadStats::default();
        self
          .load_verified_result(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        Ok(StoreStats::default())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to atomically publish local CAS result '{}': {error}",
        destination.display()
      ))),
    }
  }

  fn publish_pin(&self, action_key: &str, lookup_key: &str, action_result: &str) -> RailResult<()> {
    let pin = ActionPin {
      version: CAS_VERSION,
      action_key: action_key.to_string(),
      action_result: action_result.to_string(),
      lookup_key: lookup_key.to_string(),
      created_unix_nanos: unix_nanos(),
    };
    let bytes = canonical_json(&pin)?;
    let key_hex = validated_id_hex(action_key, ACTION_KEY_PREFIX)?;
    let destination = self.root.join("pins").join(format!("{key_hex}.json"));
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&destination) {
      Ok(_) => {
        sync_directory(&self.root.join("pins"))?;
        Ok(())
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink()) =>
      {
        let mut stats = ReadStats::default();
        let existing: ActionPin =
          read_canonical_json(&destination, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
        if existing.version != CAS_VERSION {
          return Err(RailError::message(
            "existing local CAS action pin has an incompatible schema",
          ));
        }
        if existing.action_key != action_key
          || existing.action_result != action_result
          || existing.lookup_key != lookup_key
        {
          return Err(RailError::with_help(
            format!("action key '{action_key}' produced two different verified results"),
            "the action is nondeterministic or the cache is corrupt; run `cargo rail clean --cache` before retrying",
          ));
        }
        Ok(())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to atomically publish local CAS pin '{}': {}",
        destination.display(),
        error.error
      ))),
    }
  }

  fn create_lease(&self, action_result: &str) -> RailResult<LeaseGuard> {
    validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let record = LeaseRecord {
      version: CAS_VERSION,
      action_result: action_result.to_string(),
      created_unix_seconds: unix_seconds(),
    };
    let bytes = canonical_json(&record)?;
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    let random = temporary
      .path()
      .file_name()
      .and_then(OsStr::to_str)
      .ok_or_else(|| RailError::message("temporary lease name is not valid UTF-8"))?;
    let destination = self.root.join("leases").join(format!("{random}.json"));
    temporary.persist_noclobber(&destination).map_err(|error| {
      RailError::message(format!(
        "failed to atomically publish local CAS lease '{}': {}",
        destination.display(),
        error.error
      ))
    })?;
    sync_directory(&self.root.join("leases"))?;
    Ok(LeaseGuard { path: destination })
  }
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  Ok(())
}

fn copy_blob_verified(blob: &PreparedBlob, identity: &str, destination: &Path) -> RailResult<u64> {
  let metadata = fs::symlink_metadata(&blob.source)?;
  if !metadata.is_file()
    || metadata.file_type().is_symlink()
    || !has_single_link(&metadata)
    || metadata.len() != blob.bytes
  {
    return Err(RailError::message(format!(
      "declared output '{}' changed before local CAS publication",
      blob.source.display()
    )));
  }
  let mut input = File::open(&blob.source)?;
  let opened = input.metadata()?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != blob.bytes {
    return Err(RailError::message(format!(
      "declared output '{}' changed before local CAS publication",
      blob.source.display()
    )));
  }
  let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
  let mut hasher = Sha256::new();
  let mut copied = 0u64;
  let maximum_read = blob.bytes.saturating_add(1);
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let remaining = maximum_read.saturating_sub(copied);
    if remaining == 0 {
      break;
    }
    let read_capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
    let read = input.read(&mut buffer[..read_capacity])?;
    if read == 0 {
      break;
    }
    output.write_all(&buffer[..read])?;
    hasher.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  output.sync_all()?;
  let digest = format!("sha256:{}", sha256_hex(hasher));
  if copied != blob.bytes
    || digest != blob.content_digest
    || blob_id(&digest, copied).map_err(fault_to_error)? != identity
  {
    return Err(RailError::message(format!(
      "declared output '{}' changed while the local CAS copied it",
      blob.source.display()
    )));
  }
  crate::instrumentation::record_hash(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  Ok(copied)
}

fn unix_seconds() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_secs())
}

fn sha256_hex(hasher: Sha256) -> String {
  crate::source::ContentDigest::from_sha256_bytes(hasher.finalize().into()).to_string()
}

fn unix_nanos() -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_nanos())
}

struct GcPin {
  path: PathBuf,
  key: String,
  result: String,
  created: u128,
  bytes: u64,
}

impl LocalCas {
  fn ensure_capacity(&self, incoming: u64, protected_result: Option<&str>) -> RailResult<()> {
    let current = checked_tree_bytes(&self.root)?;
    if current.saturating_add(incoming) <= self.max_bytes {
      return Ok(());
    }
    let target = self.max_bytes.saturating_sub(incoming);
    self.garbage_collect(target, protected_result)?;
    let current = checked_tree_bytes(&self.root)?;
    if current.saturating_add(incoming) > self.max_bytes {
      return Err(RailError::with_help(
        format!(
          "local CAS needs {incoming} bytes but its {}-byte bound cannot be satisfied",
          self.max_bytes
        ),
        format!("raise {CACHE_MAX_BYTES_ENV} or run `cargo rail clean --cache`"),
      ));
    }
    Ok(())
  }

  fn garbage_collect(&self, target_bytes: u64, protected_result: Option<&str>) -> RailResult<()> {
    let now = unix_seconds();
    let mut leased = BTreeSet::new();
    let leases = self.root.join("leases");
    let mut lease_paths = fs::read_dir(&leases)?.collect::<Result<Vec<_>, _>>()?;
    lease_paths.sort_by_key(|entry| entry.file_name());
    for entry in lease_paths {
      let path = entry.path();
      let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      if !metadata.is_file() || metadata.file_type().is_symlink() || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS lease '{}' is not a regular file",
          path.display()
        )));
      }
      let mut stats = ReadStats::default();
      let lease: LeaseRecord =
        read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if lease.version != CAS_VERSION {
        return Err(RailError::message("local CAS lease has an incompatible schema"));
      }
      validated_id_hex(&lease.action_result, ACTION_RESULT_PREFIX)?;
      if now.saturating_sub(lease.created_unix_seconds) >= STALE_LEASE_SECONDS {
        fs::remove_file(path)?;
      } else {
        leased.insert(lease.action_result);
      }
    }
    if let Some(protected) = protected_result {
      leased.insert(protected.to_string());
    }

    let pins_directory = self.root.join("pins");
    let mut pins = Vec::new();
    let mut pin_entries = fs::read_dir(&pins_directory)?.collect::<Result<Vec<_>, _>>()?;
    pin_entries.sort_by_key(|entry| entry.file_name());
    for entry in pin_entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || metadata.file_type().is_symlink() || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS pin '{}' is not a regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS pin has a non-UTF-8 name"))?;
      let key_hex = file_name
        .strip_suffix(".json")
        .ok_or_else(|| RailError::message(format!("local CAS pin '{file_name}' has an invalid name")))?;
      let mut stats = ReadStats::default();
      let pin: ActionPin = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if pin.version != CAS_VERSION {
        return Err(RailError::message("local CAS pin has an incompatible schema"));
      }
      if validated_id_hex(&pin.action_key, ACTION_KEY_PREFIX)? != key_hex {
        return Err(RailError::message(
          "local CAS pin filename does not match its action key",
        ));
      }
      validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)?;
      validate_lookup_key(&pin.lookup_key)?;
      pins.push(GcPin {
        path,
        key: pin.action_key,
        result: pin.action_result,
        created: pin.created_unix_nanos,
        bytes: metadata.len(),
      });
    }
    pins.sort_by(|left, right| (left.created, &left.key).cmp(&(right.created, &right.key)));

    let results_directory = self.root.join("results");
    let mut result_sizes = BTreeMap::<String, u64>::new();
    let mut result_entries = fs::read_dir(&results_directory)?.collect::<Result<Vec<_>, _>>()?;
    result_entries.sort_by_key(|entry| entry.file_name());
    for entry in result_entries {
      let name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS result has a non-UTF-8 name"))?;
      if name.len() != 64
        || !name
          .bytes()
          .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
      {
        return Err(RailError::message(format!(
          "local CAS result directory '{name}' is not canonical"
        )));
      }
      validate_real_directory(&entry.path(), "local CAS result")?;
      result_sizes.insert(
        format!("{ACTION_RESULT_PREFIX}{name}"),
        checked_tree_bytes(&entry.path())?,
      );
    }

    let mut references = BTreeMap::<String, usize>::new();
    for pin in &pins {
      *references.entry(pin.result.clone()).or_default() += 1;
    }
    for (result, size) in result_sizes.clone() {
      if !references.contains_key(&result) && !leased.contains(&result) {
        let path = result_path(&self.root, &result)?;
        safe_remove_tree(&path)?;
        result_sizes.remove(&result);
        let _ = size;
      }
    }

    let mut current = checked_tree_bytes(&self.root)?;
    for pin in pins {
      if current <= target_bytes {
        break;
      }
      if leased.contains(&pin.result) {
        continue;
      }
      fs::remove_file(&pin.path)?;
      current = current.saturating_sub(pin.bytes);
      if let Some(count) = references.get_mut(&pin.result) {
        *count = count.saturating_sub(1);
        if *count == 0 {
          references.remove(&pin.result);
          if let Some(size) = result_sizes.remove(&pin.result) {
            safe_remove_tree(&result_path(&self.root, &pin.result)?)?;
            current = current.saturating_sub(size);
          }
        }
      }
    }
    sync_directory(&pins_directory)?;
    sync_directory(&results_directory)?;
    Ok(())
  }
}

fn result_path(root: &Path, identity: &str) -> RailResult<PathBuf> {
  Ok(
    root
      .join("results")
      .join(validated_id_hex(identity, ACTION_RESULT_PREFIX)?),
  )
}

fn checked_tree_bytes(root: &Path) -> RailResult<u64> {
  let mut bytes = 0u64;
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    let metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
      return Err(RailError::with_help(
        format!("local CAS contains a symlink at '{}'", path.display()),
        "run `cargo rail clean --cache`; cargo-rail will not follow cache symlinks",
      ));
    }
    if metadata.is_dir() {
      let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      let mut children = Vec::new();
      for entry in entries {
        match entry {
          Ok(entry) => children.push(entry),
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
          Err(error) => return Err(error.into()),
        }
      }
      children.sort_by_key(|entry| entry.file_name());
      pending.extend(children.into_iter().map(|entry| entry.path()));
    } else if metadata.is_file() {
      bytes = bytes
        .checked_add(metadata.len())
        .ok_or_else(|| RailError::message("local CAS size overflow"))?;
    } else {
      return Err(RailError::message(format!(
        "local CAS contains unsupported file type '{}'",
        path.display()
      )));
    }
  }
  Ok(bytes)
}

fn safe_remove_tree(root: &Path) -> RailResult<()> {
  let metadata = fs::symlink_metadata(root)?;
  if !metadata.is_dir() || metadata.file_type().is_symlink() {
    return Err(RailError::message(format!(
      "refusing to recursively remove non-directory local CAS path '{}'",
      root.display()
    )));
  }
  let mut directories = Vec::new();
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
      directories.push(path.clone());
      let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
      entries.sort_by_key(|entry| entry.file_name());
      pending.extend(entries.into_iter().map(|entry| entry.path()));
    } else {
      fs::remove_file(path)?;
    }
  }
  directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
  for directory in directories {
    fs::remove_dir(directory)?;
  }
  Ok(())
}

pub(super) fn existing_root_at(root: &Path) -> RailResult<Option<PathBuf>> {
  if !root.is_absolute()
    || root.file_name() != Some(OsStr::new("local-cas-v1"))
    || root.parent().and_then(Path::file_name) != Some(OsStr::new("cargo-rail"))
  {
    return Err(RailError::message(format!(
      "local CAS reference '{}' is not a cargo-rail-owned cache path",
      root.display()
    )));
  }
  match fs::symlink_metadata(root) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
    Ok(_) => validate_real_directory(root, "local CAS root")?,
  }
  let canonical = fs::canonicalize(root)?;
  if canonical != root {
    return Err(RailError::message(format!(
      "local CAS reference '{}' is not canonical",
      root.display()
    )));
  }
  ensure_owner_marker_existing(root)?;
  validate_root_entries(root)?;
  for name in ["results", "pins", "leases", "staging"] {
    validate_real_directory(&root.join(name), "local CAS domain")?;
  }
  Ok(Some(root.to_path_buf()))
}

pub(super) fn remove_owned_root_at(root: &Path) -> RailResult<Option<PathBuf>> {
  let Some(root) = existing_root_at(root)? else {
    return Ok(None);
  };
  safe_remove_tree(&root)?;
  Ok(Some(root))
}

fn ensure_owner_marker_existing(root: &Path) -> RailResult<()> {
  let marker = root.join("OWNER");
  let metadata = fs::symlink_metadata(&marker).map_err(|error| {
    RailError::message(format!(
      "local CAS root '{}' has no ownership marker: {error}",
      root.display()
    ))
  })?;
  if !metadata.is_file()
    || metadata.file_type().is_symlink()
    || !has_single_link(&metadata)
    || metadata.len() != OWNER_MARKER.len() as u64
    || fs::read(&marker)? != OWNER_MARKER
  {
    return Err(RailError::with_help(
      format!("local CAS root '{}' has an invalid ownership marker", root.display()),
      "cargo-rail will not recursively remove an unowned cache root",
    ));
  }
  Ok(())
}

#[cfg(unix)]
fn has_single_link(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::MetadataExt as _;

  metadata.nlink() == 1
}

#[cfg(not(unix))]
fn has_single_link(_metadata: &fs::Metadata) -> bool {
  true
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Barrier};

  use super::*;
  use crate::source::ContentDigest;

  fn action_key(value: u8) -> String {
    format!("{ACTION_KEY_PREFIX}{value:064x}")
  }

  fn write_fixture(root: &Path, bytes: &[u8]) -> OutputManifest {
    let target = root.join("target");
    let deps = target.join("deps");
    fs::create_dir_all(&deps).expect("output directories should be created");
    fs::write(deps.join("artifact.rmeta"), bytes).expect("output bytes should be written");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("target mode");
      fs::set_permissions(&deps, fs::Permissions::from_mode(0o755)).expect("deps mode");
      fs::set_permissions(deps.join("artifact.rmeta"), fs::Permissions::from_mode(0o644)).expect("file mode");
    }
    manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/deps".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/deps/artifact.rmeta".to_string(),
        kind: OutputEntryKind::File {
          digest: format!("sha256:{}", ContentDigest::sha256(bytes)),
          mode: 0o644,
          bytes: bytes.len() as u64,
        },
      },
    ])
  }

  fn manifest(mut entries: Vec<OutputEntry>) -> OutputManifest {
    entries.sort();
    let files = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::File { .. }))
      .count();
    let directories = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::Directory { .. }))
      .count();
    let symlinks = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::Symlink { .. }))
      .count();
    let bytes = entries
      .iter()
      .filter_map(|entry| match entry.kind {
        OutputEntryKind::File { bytes, .. } => Some(bytes),
        _ => None,
      })
      .sum();
    let digest = super::super::output_manifest_digest(&entries).expect("manifest should hash");
    OutputManifest {
      version: super::super::OUTPUT_MANIFEST_VERSION,
      digest,
      entries,
      files,
      directories,
      symlinks,
      bytes,
    }
  }

  fn store_fixture(cas: &LocalCas, output: &Path, key: &str, manifest: &OutputManifest) -> StoreStats {
    let result = super::super::hermetic_result_digest(key, manifest.digest());
    let lookup = super::super::test_pre_context_lookup_key();
    let validation = super::super::FastCacheValidation::fixture(key, &lookup);
    cas
      .store(StoreRequest {
        action_key: key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output,
      })
      .expect("fixture should enter the CAS")
  }

  #[test]
  fn verified_bundle_round_trips_exact_bytes_and_modes() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"exact compiler bytes");
    #[cfg(unix)]
    let manifest = {
      use std::os::unix::fs::PermissionsExt as _;
      let mut manifest = manifest;
      fs::set_permissions(
        output.path().join("target/deps/artifact.rmeta"),
        fs::Permissions::from_mode(0o755),
      )
      .expect("executable output mode");
      let entry = manifest
        .entries
        .iter_mut()
        .find(|entry| entry.path.ends_with("artifact.rmeta"))
        .expect("file manifest entry");
      let OutputEntryKind::File { mode, .. } = &mut entry.kind else {
        panic!("artifact should be a file");
      };
      *mode = 0o755;
      manifest.digest = super::super::output_manifest_digest(&manifest.entries).expect("updated manifest identity");
      manifest
    };
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(1);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    assert!(stored.objects_written >= 5);
    let destination = restore_parent.path().join("clean-output");
    let CacheLookup::Hit(hit) = cas.restore(&key, &destination) else {
      panic!("verified result should hit");
    };
    assert_eq!(hit.bytes_restored, b"exact compiler bytes".len() as u64);
    assert_eq!(
      fs::read(destination.join("target/deps/artifact.rmeta")).expect("restored bytes"),
      b"exact compiler bytes"
    );
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(
        fs::metadata(destination.join("target/deps/artifact.rmeta"))
          .expect("restored metadata")
          .permissions()
          .mode()
          & 0o777,
        0o755
      );
    }
    hit
      .output_manifest
      .validate_unchanged(&destination)
      .expect("restored manifest must revalidate");
  }

  #[cfg(unix)]
  #[test]
  fn verified_bundle_round_trips_bounded_symlinks() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    fs::create_dir(output.path().join("target")).expect("target directory");
    fs::write(output.path().join("target/real"), b"bytes").expect("real output");
    symlink("real", output.path().join("target/link")).expect("bounded output symlink");
    let manifest = manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/link".to_string(),
        kind: OutputEntryKind::Symlink {
          target: "real".to_string(),
        },
      },
      OutputEntry {
        path: "target/real".to_string(),
        kind: OutputEntryKind::File {
          digest: format!("sha256:{}", ContentDigest::sha256(b"bytes")),
          mode: 0o644,
          bytes: 5,
        },
      },
    ]);
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(9);
    store_fixture(&cas, output.path(), &key, &manifest);
    let destination = restore_parent.path().join("clean-output");
    let CacheLookup::Hit(_) = cas.restore(&key, &destination) else {
      panic!("symlink result should hit");
    };
    assert_eq!(
      fs::read_link(destination.join("target/link")).expect("restored link"),
      Path::new("real")
    );
    assert_eq!(
      fs::read(destination.join("target/link")).expect("linked bytes"),
      b"bytes"
    );
  }

  #[test]
  fn same_size_blob_tampering_is_a_corrupt_miss() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"alpha");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(2);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    let result = stored.action_result.expect("action result identity");
    let bundle = result_path(cas.root(), &result).expect("bundle path");
    let blob = fs::read_dir(bundle.join("blobs"))
      .expect("blob directory")
      .next()
      .expect("one blob")
      .expect("blob entry")
      .path();
    fs::write(blob, b"omega").expect("same-size tamper");
    let CacheLookup::Miss(miss) = cas.restore(&key, &restore_parent.path().join("restore")) else {
      panic!("tampered blob must not hit");
    };
    assert_eq!(miss.kind, CacheMissKind::Corrupt);
    assert!(miss.reason.contains("blob_digest_mismatch"), "{}", miss.reason);
  }

  #[cfg(unix)]
  #[test]
  fn hard_linked_outputs_are_not_published() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let outside = tempfile::tempdir().expect("outside root");
    let manifest = write_fixture(output.path(), b"linked");
    fs::hard_link(
      output.path().join("target/deps/artifact.rmeta"),
      outside.path().join("alias"),
    )
    .expect("hard link fixture");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(10);
    let result = super::super::hermetic_result_digest(&key, manifest.digest());
    let lookup = super::super::test_pre_context_lookup_key();
    let validation = super::super::FastCacheValidation::fixture(&key, &lookup);
    let error = cas
      .store(StoreRequest {
        action_key: &key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest: &manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output.path(),
      })
      .expect_err("hard-linked output must not enter the CAS");
    assert!(
      error.to_string().contains("changed before local CAS publication"),
      "{error}"
    );
    assert_eq!(
      fs::read(outside.path().join("alias")).expect("outside alias"),
      b"linked"
    );
  }

  #[test]
  fn missing_and_incompatible_objects_fail_closed() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(3);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    let result = stored.action_result.expect("action result identity");
    let bundle = result_path(cas.root(), &result).expect("bundle path");
    let blob = fs::read_dir(bundle.join("blobs"))
      .expect("blob directory")
      .next()
      .expect("one blob")
      .expect("blob entry")
      .path();
    fs::remove_file(blob).expect("blob removal");
    let CacheLookup::Miss(missing) = cas.restore(&key, &restore_parent.path().join("missing")) else {
      panic!("missing blob must not hit");
    };
    assert_eq!(missing.kind, CacheMissKind::Corrupt);

    let key = action_key(4);
    store_fixture(&cas, output.path(), &key, &manifest);
    let pin_path = cas.root().join("pins").join(format!(
      "{}.json",
      validated_id_hex(&key, ACTION_KEY_PREFIX).expect("key hex")
    ));
    let mut pin: ActionPin = serde_json::from_slice(&fs::read(&pin_path).expect("pin bytes")).expect("pin JSON");
    pin.version += 1;
    fs::write(&pin_path, canonical_json(&pin).expect("canonical pin")).expect("incompatible pin");
    let CacheLookup::Miss(incompatible) = cas.restore(&key, &restore_parent.path().join("incompatible")) else {
      panic!("incompatible pin must not hit");
    };
    assert_eq!(incompatible.kind, CacheMissKind::Incompatible);
  }

  #[test]
  fn truncated_and_malicious_metadata_objects_fail_closed() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");

    let truncated_key = action_key(11);
    let truncated = store_fixture(&cas, output.path(), &truncated_key, &manifest)
      .action_result
      .expect("action result identity");
    let truncated_bundle = result_path(cas.root(), &truncated).expect("bundle path");
    fs::write(truncated_bundle.join("action-result.json"), b"{").expect("truncate metadata object");
    let CacheLookup::Miss(truncated) = cas.restore(&truncated_key, &restore_parent.path().join("truncated")) else {
      panic!("truncated metadata must not hit");
    };
    assert_eq!(truncated.kind, CacheMissKind::Corrupt);
    assert!(truncated.reason.contains("object_decode"), "{}", truncated.reason);

    let malicious_key = action_key(12);
    let malicious_result = store_fixture(&cas, output.path(), &malicious_key, &manifest)
      .action_result
      .expect("action result identity");
    let old_bundle = result_path(cas.root(), &malicious_result).expect("old bundle path");
    let mut object: ActionResultObject =
      serde_json::from_slice(&fs::read(old_bundle.join("action-result.json")).expect("result object"))
        .expect("result JSON");
    let malicious_tree = TreeObject {
      version: CAS_VERSION,
      entries: vec![TreeEntry {
        name: "..".to_string(),
        kind: TreeEntryKind::Symlink {
          target: "../../outside".to_string(),
          directory: false,
        },
      }],
    };
    let malicious_tree_id = tree_id(&malicious_tree).expect("malicious tree identity");
    let malicious_tree_hex = validated_id_hex(&malicious_tree_id, TREE_PREFIX).expect("tree hex");
    fs::write(
      old_bundle.join("trees").join(format!("{malicious_tree_hex}.json")),
      canonical_json(&malicious_tree).expect("canonical tree"),
    )
    .expect("malicious tree object");
    object.output_tree = malicious_tree_id;
    let forged_result = action_result_id(&object).expect("forged result identity");
    fs::write(
      old_bundle.join("action-result.json"),
      canonical_json(&object).expect("canonical action result"),
    )
    .expect("forged action result");
    let forged_bundle = result_path(cas.root(), &forged_result).expect("forged bundle path");
    fs::rename(&old_bundle, &forged_bundle).expect("rename forged bundle");
    let pin_path = cas.root().join("pins").join(format!(
      "{}.json",
      validated_id_hex(&malicious_key, ACTION_KEY_PREFIX).expect("key hex")
    ));
    let mut pin: ActionPin = serde_json::from_slice(&fs::read(&pin_path).expect("pin")).expect("pin JSON");
    pin.action_result = forged_result;
    fs::write(&pin_path, canonical_json(&pin).expect("canonical pin")).expect("forged pin");
    let CacheLookup::Miss(malicious) = cas.restore(&malicious_key, &restore_parent.path().join("malicious")) else {
      panic!("malicious tree must not hit");
    };
    assert_eq!(malicious.kind, CacheMissKind::Corrupt);
    assert_eq!(malicious.reason, "unsafe_output_name");
    assert!(!restore_parent.path().join("outside").exists());
  }

  #[test]
  fn unsafe_paths_collisions_and_symlinks_never_enter_a_tree() {
    let file = |path: &str| OutputEntry {
      path: path.to_string(),
      kind: OutputEntryKind::File {
        digest: format!("sha256:{}", ContentDigest::sha256(b"x")),
        mode: 0o644,
        bytes: 1,
      },
    };
    let traversal = manifest(vec![file("target/../outside")]);
    assert_eq!(
      validate_manifest(&traversal)
        .expect_err("parent traversal must fail")
        .reason,
      "unsafe_output_path"
    );
    let collision = manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      file("target/A"),
      file("target/a"),
    ]);
    let root = tempfile::tempdir().expect("output root");
    fs::create_dir(root.path().join("target")).expect("target directory");
    fs::write(root.path().join("target/A"), b"x").expect("A");
    fs::write(root.path().join("target/a"), b"x").expect("a");
    validate_manifest(&collision).expect("flat manifest is structurally valid");
    assert_eq!(
      prepare_tree(&collision, root.path())
        .expect_err("portable name collision must fail")
        .reason,
      "platform_colliding_output_names"
    );
    let escaping_link = manifest(vec![OutputEntry {
      path: "target/link".to_string(),
      kind: OutputEntryKind::Symlink {
        target: "../../outside".to_string(),
      },
    }]);
    assert_eq!(
      validate_manifest(&escaping_link)
        .expect_err("escaping link must fail")
        .reason,
      "symlink_target_escape"
    );
  }

  #[test]
  fn hostile_prepositioned_materialization_root_is_preserved() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(5);
    store_fixture(&cas, output.path(), &key, &manifest);
    let destination = restore_parent.path().join("occupied");
    fs::create_dir(&destination).expect("occupied destination");
    fs::write(destination.join("keep"), b"keep").expect("sentinel");
    let CacheLookup::Miss(miss) = cas.restore(&key, &destination) else {
      panic!("pre-positioned destination must not hit");
    };
    assert_eq!(miss.kind, CacheMissKind::Corrupt);
    assert_eq!(fs::read(destination.join("keep")).expect("sentinel"), b"keep");
  }

  #[test]
  fn concurrent_writers_converge_on_one_complete_bundle() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = Arc::new(write_fixture(output.path(), b"concurrent"));
    let key = action_key(6);
    LocalCas::open_at(cache.path(), 1024 * 1024).expect("initialize CAS");
    let barrier = Arc::new(Barrier::new(2));
    let cache_path = cache.path();
    let output_path = output.path();
    std::thread::scope(|scope| {
      let mut handles = Vec::new();
      for _ in 0..2 {
        let manifest = Arc::clone(&manifest);
        let barrier = Arc::clone(&barrier);
        let key = key.clone();
        handles.push(scope.spawn(move || {
          let cas = LocalCas::open_at(cache_path, 1024 * 1024).expect("writer CAS");
          barrier.wait();
          store_fixture(&cas, output_path, &key, &manifest);
        }));
      }
      for handle in handles {
        handle.join().expect("writer should converge");
      }
    });
    assert_eq!(
      fs::read_dir(cache.path().join("cargo-rail/local-cas-v1/pins"))
        .unwrap()
        .count(),
      1
    );
    assert_eq!(
      fs::read_dir(cache.path().join("cargo-rail/local-cas-v1/results"))
        .unwrap()
        .count(),
      1
    );
  }

  #[test]
  fn interrupted_staging_state_never_authorizes_reuse() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"complete");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let orphan = cas.root().join("staging/result-interrupted/payload");
    fs::create_dir_all(orphan.join("blobs")).expect("orphaned staging directories");
    fs::write(orphan.join("action-result.json"), b"{").expect("partial staged object");

    let absent_key = action_key(13);
    let CacheLookup::Miss(absent) = cas.restore(&absent_key, &restore_parent.path().join("absent")) else {
      panic!("unpublished staging state must not hit");
    };
    assert_eq!(absent.kind, CacheMissKind::Miss);
    assert_eq!(absent.reason, "action_not_found");

    let published_key = action_key(14);
    store_fixture(&cas, output.path(), &published_key, &manifest);
    let CacheLookup::Hit(_) = cas.restore(&published_key, &restore_parent.path().join("published")) else {
      panic!("an unrelated complete publication must remain reusable");
    };
    assert!(orphan.exists(), "only atomic pin publication may authorize a result");
  }

  #[test]
  fn aggregate_object_limits_fail_before_authorization() {
    let bundle = tempfile::tempdir().expect("bundle root");
    fs::create_dir(bundle.path().join("trees")).expect("tree object directory");
    let tree = TreeObject {
      version: CAS_VERSION,
      entries: vec![TreeEntry {
        name: "target".to_string(),
        kind: TreeEntryKind::Symlink {
          target: "inside".to_string(),
          directory: false,
        },
      }],
    };
    let identity = tree_id(&tree).expect("tree identity");
    let hex = validated_id_hex(&identity, TREE_PREFIX).expect("tree hex");
    fs::write(
      bundle.path().join("trees").join(format!("{hex}.json")),
      canonical_json(&tree).expect("canonical tree"),
    )
    .expect("tree object");
    let mut total_entries = MAX_ENTRIES;
    let mut loading = BTreeSet::new();
    let mut loaded = BTreeMap::new();
    let mut stats = ReadStats::default();
    let error = load_tree_recursive(
      bundle.path(),
      &identity,
      0,
      &mut total_entries,
      &mut loading,
      &mut loaded,
      &mut stats,
    )
    .expect_err("aggregate entry limit must fail");
    assert_eq!(error.reason, "tree_entry_limit");

    let object = bundle.path().join("bounded.json");
    fs::write(&object, b"{}").expect("bounded object");
    let mut exhausted = ReadStats {
      bytes: MAX_LOOKUP_BYTES,
      ..ReadStats::default()
    };
    let error = read_bounded_file(&object, MAX_OBJECT_METADATA_BYTES, &mut exhausted)
      .expect_err("aggregate byte limit must fail");
    assert_eq!(error.reason, "result_byte_limit");
  }

  #[cfg(unix)]
  #[test]
  fn owned_cleanup_unlinks_nested_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let outside = tempfile::tempdir().expect("outside root");
    fs::write(outside.path().join("keep"), b"outside").expect("outside sentinel");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    symlink(outside.path(), cas.root().join("staging/hostile-link")).expect("hostile nested link");
    let removed = remove_owned_root_at(cas.root()).expect("owned cleanup should succeed");
    assert!(removed.is_some());
    assert_eq!(
      fs::read(outside.path().join("keep")).expect("outside sentinel"),
      b"outside"
    );
  }

  #[test]
  fn cache_open_refuses_to_adopt_a_nonempty_unowned_root() {
    let cache = tempfile::tempdir().expect("cache base");
    let root = cache.path().join("cargo-rail/local-cas-v1");
    fs::create_dir_all(&root).expect("hostile root");
    let sentinel = root.join("user-data");
    fs::write(&sentinel, b"preserve me").expect("hostile sentinel");

    let error = LocalCas::open_at(cache.path(), 1024 * 1024).expect_err("unowned root must not be adopted");
    assert!(error.to_string().contains("nonempty"), "{error}");
    assert!(!root.join("OWNER").exists());
    assert_eq!(fs::read(&sentinel).expect("sentinel must survive"), b"preserve me");
    assert!(remove_owned_root_at(&root).is_err());
    assert_eq!(
      fs::read(&sentinel).expect("cleanup refusal must preserve sentinel"),
      b"preserve me"
    );
  }

  #[test]
  fn cleanup_refuses_an_invalid_owner_marker() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    fs::write(cas.root().join("OWNER"), b"forged\n").expect("tamper owner marker");
    let error = remove_owned_root_at(cas.root()).expect_err("invalid marker must block recursive cleanup");
    assert!(error.to_string().contains("ownership marker"), "{error}");
    assert!(cas.root().exists());
  }

  #[test]
  fn deterministic_gc_removes_unpinned_bundles() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_fixture(&cas, output.path(), &action_key(7), &manifest);
    store_fixture(&cas, output.path(), &action_key(8), &manifest);
    cas.garbage_collect(0, None).expect("GC should be deterministic");
    assert_eq!(fs::read_dir(cas.root().join("pins")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(cas.root().join("results")).unwrap().count(), 0);
  }
}
