//! Compiler-run capabilities shared across wrapper processes.

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

pub(crate) const FACT_SESSION_ENV: &str = "CARGO_RAIL_COMPILER_FACT_SESSION";
const FACT_SESSION_VERSION: u32 = 1;
const MAX_FACT_SESSION_BYTES: u64 = 4 * 1024;

/// Authenticated authority to publish facts for one explicitly launched compiler run.
pub(crate) struct CompilerFactSession {
  observation_directory: PathBuf,
  source_root: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerFactSessionRecord {
  version: u32,
  observation_directory_identity: String,
  source_root_identity: String,
}

impl CompilerFactSession {
  pub(crate) fn write(observation_directory: &Path, source_root: &Path) -> RailResult<PathBuf> {
    let observation_directory = crate::utils::canonicalize_existing(observation_directory)?;
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let record = CompilerFactSessionRecord {
      version: FACT_SESSION_VERSION,
      observation_directory_identity: path_identity(&observation_directory),
      source_root_identity: path_identity(&source_root),
    };
    let encoded = serde_json::to_vec(&record)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".cargo-rail-fact-").suffix(".cap");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;

      builder.permissions(fs::Permissions::from_mode(0o600));
    }
    let mut capability = builder.tempfile_in(&observation_directory)?;
    capability.write_all(&encoded)?;
    let (_file, path) = capability
      .keep()
      .map_err(|error| RailError::message(format!("failed to retain compiler fact capability: {}", error.error)))?;
    Ok(path)
  }

  pub(crate) fn load(capability: &Path, observation_directory: &Path, source_root: &Path) -> RailResult<Self> {
    let metadata = fs::symlink_metadata(capability)?;
    if !metadata.is_file()
      || crate::utils::is_symlink_or_reparse(&metadata)
      || metadata.len() > MAX_FACT_SESSION_BYTES
      || !private_mode(&metadata)
    {
      return Err(RailError::message(
        "compiler fact capability is not a bounded private regular file",
      ));
    }
    let mut file = File::open(capability)?;
    if !crate::utils::private_file_matches_path(&file, capability, metadata.len())? {
      return Err(RailError::message(
        "compiler fact capability changed before it was opened",
      ));
    }
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
      .take(MAX_FACT_SESSION_BYTES.saturating_add(1))
      .read_to_end(&mut encoded)?;
    if encoded.len() as u64 != metadata.len() {
      return Err(RailError::message("compiler fact capability changed while it was read"));
    }

    let canonical_capability = crate::utils::canonicalize_existing(capability)?;
    let canonical_observation_directory = crate::utils::canonicalize_existing(observation_directory)?;
    let canonical_source_root = crate::utils::canonicalize_existing(source_root)?;
    if canonical_capability.parent() != Some(canonical_observation_directory.as_path()) {
      return Err(RailError::message(
        "compiler fact capability is outside its observation directory",
      ));
    }
    let record: CompilerFactSessionRecord = serde_json::from_slice(&encoded)?;
    if record.version != FACT_SESSION_VERSION
      || record.observation_directory_identity != path_identity(&canonical_observation_directory)
      || record.source_root_identity != path_identity(&canonical_source_root)
    {
      return Err(RailError::message(
        "compiler fact capability does not authorize this compiler run",
      ));
    }
    Ok(Self {
      observation_directory: observation_directory.to_path_buf(),
      source_root: source_root.to_path_buf(),
    })
  }

  pub(crate) fn observation_directory(&self) -> &Path {
    &self.observation_directory
  }

  pub(crate) fn source_root(&self) -> &Path {
    &self.source_root
  }
}

fn path_identity(path: &Path) -> String {
  let mut hasher = Sha256::new();
  hasher.update(b"cargo-rail-compiler-fact-path-v1\0");
  hasher.update((path.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
  hasher.update(path.as_os_str().as_encoded_bytes());
  let digest = ContentDigest::from_sha256_bytes(hasher.finalize().into());
  format!("sha256:{digest}")
}

#[cfg(unix)]
fn private_mode(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o077 == 0
}

#[cfg(not(unix))]
fn private_mode(_metadata: &fs::Metadata) -> bool {
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fact_capability_binds_both_authority_paths() {
    let root = tempfile::tempdir().expect("source root");
    let observations = tempfile::tempdir().expect("observation directory");
    let capability = CompilerFactSession::write(observations.path(), root.path()).expect("fact capability");
    let session = CompilerFactSession::load(&capability, observations.path(), root.path()).expect("valid capability");
    assert_eq!(session.observation_directory(), observations.path());
    assert_eq!(session.source_root(), root.path());

    let other_root = tempfile::tempdir().expect("different source root");
    assert!(CompilerFactSession::load(&capability, observations.path(), other_root.path()).is_err());
  }
}
