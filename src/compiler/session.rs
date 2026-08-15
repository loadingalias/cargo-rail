//! Compiler-run capabilities shared across wrapper processes.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::compiler::facts::{
  COMPILER_FACT_PROTOCOL_VERSION, CompilerFactCoverage, CompilerFactDomain, CompilerFactInvocation,
  CompilerFactPackage, CompilerFactProducerAuthority, CompilerFactRole, CompilerFactRunAuthority,
  CompilerFactTargetKind, CompilerFactUnit,
};
use crate::compiler::observation::{ObservationPath, RawCompilerInvocation};
use crate::compiler::scheduler::CompilerFactFamily;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;
use crate::workspace::WorkspaceSnapshot;

pub(crate) const FACT_SESSION_ENV: &str = "CARGO_RAIL_COMPILER_FACT_SESSION";
const FACT_SESSION_VERSION: u32 = 4;
const MAX_FACT_SESSION_BYTES: u64 = 4 * 1024 * 1024;

/// Authenticated authority to publish facts for one explicitly launched compiler run.
pub(crate) struct CompilerFactSession {
  observation_directory: PathBuf,
  source_root: PathBuf,
  fact_families: BTreeSet<CompilerFactFamily>,
  typed: Option<CompilerFactTypedSession>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerFactSessionRecord {
  version: u32,
  observation_directory_identity: String,
  source_root_identity: String,
  fact_families: BTreeSet<CompilerFactFamily>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  typed: Option<CompilerFactTypedSession>,
}

/// Exact driver, compiler, view, and Cargo-target authority for typed facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactTypedSession {
  pub(crate) run_authority: CompilerFactRunAuthority,
  pub(crate) producer_authority: CompilerFactProducerAuthority,
  pub(crate) driver_program: String,
  pub(crate) rustc_program: String,
  pub(crate) compiler_library_directory: String,
  pub(crate) host_platform: String,
  pub(crate) target_platform: String,
  pub(crate) doctest: bool,
  pub(crate) doctest_sysroot: Option<String>,
  pub(crate) generated_roots: Vec<String>,
  pub(crate) required_coverage: BTreeSet<CompilerFactCoverage>,
  pub(crate) targets: Vec<CompilerFactTargetAuthority>,
}

/// One captured workspace target the wrapper may authorize for the driver.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactTargetAuthority {
  pub(crate) package: CompilerFactPackage,
  pub(crate) manifest_directory: String,
  pub(crate) cargo_target: String,
  pub(crate) crate_name: String,
  pub(crate) target_kind: CompilerFactTargetKind,
  pub(crate) source: String,
  pub(crate) doctest: bool,
}

impl CompilerFactSession {
  #[cfg(test)]
  pub(crate) fn write(
    observation_directory: &Path,
    source_root: &Path,
    fact_families: &BTreeSet<CompilerFactFamily>,
  ) -> RailResult<PathBuf> {
    Self::write_with_typed(observation_directory, source_root, fact_families, None)
  }

  pub(crate) fn write_with_typed(
    observation_directory: &Path,
    source_root: &Path,
    fact_families: &BTreeSet<CompilerFactFamily>,
    typed: Option<CompilerFactTypedSession>,
  ) -> RailResult<PathBuf> {
    if fact_families.is_empty() {
      return Err(RailError::message(
        "compiler fact capability requires at least one fact family",
      ));
    }
    if fact_families.contains(&CompilerFactFamily::TypedRustItems) != typed.is_some() {
      return Err(RailError::message(
        "compiler fact capability typed authority does not match its requested fact families",
      ));
    }
    if let Some(typed) = &typed {
      typed.validate()?;
    }
    let observation_directory = crate::utils::canonicalize_existing(observation_directory)?;
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let record = CompilerFactSessionRecord {
      version: FACT_SESSION_VERSION,
      observation_directory_identity: path_identity(&observation_directory),
      source_root_identity: path_identity(&source_root),
      fact_families: fact_families.clone(),
      typed,
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
      || record.fact_families.is_empty()
      || record.fact_families.contains(&CompilerFactFamily::TypedRustItems) != record.typed.is_some()
    {
      return Err(RailError::message(
        "compiler fact capability does not authorize this compiler run",
      ));
    }
    if let Some(typed) = &record.typed {
      typed.validate()?;
    }
    Ok(Self {
      observation_directory: observation_directory.to_path_buf(),
      source_root: source_root.to_path_buf(),
      fact_families: record.fact_families,
      typed: record.typed,
    })
  }

  pub(crate) fn observation_directory(&self) -> &Path {
    &self.observation_directory
  }

  pub(crate) fn source_root(&self) -> &Path {
    &self.source_root
  }

  pub(crate) fn fact_families(&self) -> &BTreeSet<CompilerFactFamily> {
    &self.fact_families
  }

  pub(crate) fn typed(&self) -> Option<&CompilerFactTypedSession> {
    self.typed.as_ref()
  }
}

impl CompilerFactTypedSession {
  fn validate(&self) -> RailResult<()> {
    if self.driver_program.is_empty()
      || self.rustc_program.is_empty()
      || self.compiler_library_directory.is_empty()
      || self.host_platform.is_empty()
      || self.target_platform.is_empty()
      || self.required_coverage.is_empty()
      || self.targets.is_empty()
      || self.targets.windows(2).any(|pair| pair[0] >= pair[1])
      || self.generated_roots.is_empty()
      || self.generated_roots.windows(2).any(|pair| pair[0] >= pair[1])
      || self.doctest != self.doctest_sysroot.is_some()
    {
      return Err(RailError::message(
        "typed compiler fact session authority is incomplete",
      ));
    }
    if !Path::new(&self.driver_program).is_absolute()
      || !Path::new(&self.rustc_program).is_absolute()
      || !Path::new(&self.compiler_library_directory).is_absolute()
      || self.generated_roots.iter().any(|root| !Path::new(root).is_absolute())
    {
      return Err(RailError::message(
        "typed compiler fact executable and library authorities must be absolute",
      ));
    }
    if self
      .doctest_sysroot
      .as_deref()
      .is_some_and(|path| !Path::new(path).is_absolute())
    {
      return Err(RailError::message(
        "typed compiler fact doctest sysroot authority must be absolute",
      ));
    }
    Ok(())
  }

  /// Capture the selected targets from the already-loaded canonical Cargo metadata.
  pub(crate) fn targets_from_snapshot(
    snapshot: &WorkspaceSnapshot,
    selected_packages: &BTreeSet<String>,
  ) -> RailResult<Vec<CompilerFactTargetAuthority>> {
    let source_root = crate::utils::canonicalize_existing(snapshot.source_root())?;
    let mut found = BTreeSet::new();
    let mut targets = Vec::new();
    for package in snapshot.base_resolution().metadata().workspace_packages() {
      if !selected_packages.contains(package.name.as_str()) {
        continue;
      }
      found.insert(package.name.to_string());
      let manifest = crate::utils::canonicalize_existing(package.manifest_path.as_std_path())?;
      let manifest_directory = repository_path(
        manifest
          .parent()
          .ok_or_else(|| RailError::message("workspace package manifest has no parent directory"))?,
        &source_root,
      )?;
      let package_identity = CompilerFactPackage {
        name: package.name.to_string(),
        version: package.version.to_string(),
        source: package.source.as_ref().map(ToString::to_string),
      };
      for target in &package.targets {
        let source = crate::utils::canonicalize_existing(target.src_path.as_std_path())?;
        targets.push(CompilerFactTargetAuthority {
          package: package_identity.clone(),
          manifest_directory: manifest_directory.clone(),
          cargo_target: target.name.to_string(),
          crate_name: target.name.replace('-', "_"),
          target_kind: target_kind(&target.kind),
          source: repository_path(&source, &source_root)?,
          doctest: target.doctest,
        });
      }
    }
    if &found != selected_packages {
      return Err(RailError::message(
        "typed compiler fact targets do not cover the exact scheduled package set",
      ));
    }
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
      return Err(RailError::message(
        "typed compiler fact package set has no Cargo targets",
      ));
    }
    Ok(targets)
  }

  /// Authorize one exact workspace rustc invocation, or bypass an unrequested workspace dependency.
  pub(crate) fn authorize_invocation(
    &self,
    raw: &RawCompilerInvocation,
    observation_directory: &Path,
    source_root: &Path,
    doctest_builder: bool,
  ) -> RailResult<Option<CompilerFactInvocation>> {
    if self.doctest != doctest_builder {
      return Ok(None);
    }
    let package_name = std::env::var("CARGO_PKG_NAME").ok();
    let selected_package = package_name
      .as_ref()
      .is_some_and(|name| self.targets.iter().any(|target| target.package.name == *name));
    if !selected_package {
      return Ok(None);
    }
    let package_name = package_name.ok_or_else(|| RailError::message("Cargo omitted the selected package name"))?;
    let package_version = std::env::var("CARGO_PKG_VERSION")
      .map_err(|_| RailError::message("Cargo omitted the selected package version"))?;
    let manifest_directory = std::env::var_os("CARGO_MANIFEST_DIR")
      .map(PathBuf::from)
      .ok_or_else(|| RailError::message("Cargo omitted the selected package manifest directory"))?;
    let manifest_directory = crate::utils::canonicalize_existing(&manifest_directory)?;
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let manifest_directory = repository_path(&manifest_directory, &source_root)?;
    let crate_name = raw
      .crate_name
      .as_deref()
      .or_else(|| {
        (doctest_builder && raw.compiler_arguments.iter().any(|argument| argument == "-")).then_some("rust_out")
      })
      .ok_or_else(|| RailError::message("selected typed compiler invocation has no crate name"))?;
    let declared_sources = raw
      .declared_inputs
      .iter()
      .filter_map(|input| match &input.path {
        ObservationPath::Repository(path) => Some(path.as_str()),
        ObservationPath::Host(_) => None,
      })
      .collect::<BTreeSet<_>>();
    let mut candidates = self.targets.iter().filter(|target| {
      target.package.name == package_name
        && target.package.version == package_version
        && target.manifest_directory == manifest_directory
        && if doctest_builder {
          target.doctest && target.target_kind == CompilerFactTargetKind::Library
        } else {
          target.crate_name == crate_name && declared_sources.contains(target.source.as_str())
        }
    });
    let target = candidates
      .next()
      .ok_or_else(|| RailError::message("selected compiler invocation does not match a captured Cargo target"))?;
    if candidates.next().is_some() {
      return Err(RailError::message(
        "selected compiler invocation matches more than one captured Cargo target",
      ));
    }
    let domain = if doctest_builder {
      CompilerFactDomain::Doctest
    } else {
      match target.target_kind {
        CompilerFactTargetKind::BuildScript => CompilerFactDomain::BuildScript,
        CompilerFactTargetKind::ProcMacro => CompilerFactDomain::ProcMacro,
        CompilerFactTargetKind::Test | CompilerFactTargetKind::Example | CompilerFactTargetKind::Benchmark => {
          CompilerFactDomain::NonProduction
        }
        _ if raw.test_mode => CompilerFactDomain::NonProduction,
        _ => CompilerFactDomain::Production,
      }
    };
    let role = if matches!(domain, CompilerFactDomain::BuildScript | CompilerFactDomain::ProcMacro) {
      CompilerFactRole::Host
    } else {
      CompilerFactRole::Target
    };
    let platform = if role == CompilerFactRole::Host {
      self.host_platform.clone()
    } else {
      self.target_platform.clone()
    };
    let mut features = raw
      .cfg
      .iter()
      .filter_map(|cfg| cfg.strip_prefix("feature=\"")?.strip_suffix('"'))
      .map(str::to_string)
      .collect::<Vec<_>>();
    features.sort();
    features.dedup();
    let unit = CompilerFactUnit {
      identity: String::new(),
      invocation_identity: raw.compiler_fact_invocation_identity()?,
      package: target.package.clone(),
      cargo_target: target.cargo_target.clone(),
      crate_name: crate_name.to_string(),
      target_kind: target.target_kind.clone(),
      domain,
      role,
      platform,
      features,
      cfg: raw.cfg.iter().cloned().collect(),
    }
    .bind_identity()?;
    let mut generated_roots = self.generated_roots.clone();
    if let Some(out_dir) = std::env::var_os("OUT_DIR") {
      let out_dir = crate::utils::canonicalize_existing(Path::new(&out_dir))?
        .to_str()
        .ok_or_else(|| RailError::message("compiler fact OUT_DIR is not valid UTF-8"))?
        .to_string();
      generated_roots.push(out_dir);
      generated_roots.sort();
      generated_roots.dedup();
    }
    Ok(Some(CompilerFactInvocation {
      version: COMPILER_FACT_PROTOCOL_VERSION,
      observation_directory: observation_directory.to_string_lossy().into_owned(),
      source_root: source_root.to_string_lossy().into_owned(),
      generated_roots,
      run_authority: self.run_authority.clone(),
      producer_authority: self.producer_authority.clone(),
      unit,
      required_coverage: self.required_coverage.clone(),
    }))
  }
}

fn repository_path(path: &Path, source_root: &Path) -> RailResult<String> {
  let relative = path
    .strip_prefix(source_root)
    .map_err(|_| RailError::message("typed compiler fact target is outside the captured source root"))?;
  Ok(crate::utils::path_to_git_format(relative))
}

fn target_kind(kinds: &[cargo_metadata::TargetKind]) -> CompilerFactTargetKind {
  use cargo_metadata::TargetKind;

  if kinds.contains(&TargetKind::CustomBuild) {
    CompilerFactTargetKind::BuildScript
  } else if kinds.contains(&TargetKind::ProcMacro) {
    CompilerFactTargetKind::ProcMacro
  } else if kinds.contains(&TargetKind::Test) {
    CompilerFactTargetKind::Test
  } else if kinds.contains(&TargetKind::Example) {
    CompilerFactTargetKind::Example
  } else if kinds.contains(&TargetKind::Bench) {
    CompilerFactTargetKind::Benchmark
  } else if kinds.contains(&TargetKind::Bin) {
    CompilerFactTargetKind::Binary
  } else if kinds.iter().any(|kind| {
    matches!(
      kind,
      TargetKind::Lib | TargetKind::RLib | TargetKind::DyLib | TargetKind::CDyLib | TargetKind::StaticLib
    )
  }) {
    CompilerFactTargetKind::Library
  } else {
    CompilerFactTargetKind::Other(kinds.iter().map(ToString::to_string).collect::<Vec<_>>().join(","))
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
    let families = BTreeSet::from([CompilerFactFamily::StableDiagnostics]);
    let capability = CompilerFactSession::write(observations.path(), root.path(), &families).expect("fact capability");
    let session = CompilerFactSession::load(&capability, observations.path(), root.path()).expect("valid capability");
    assert_eq!(session.observation_directory(), observations.path());
    assert_eq!(session.source_root(), root.path());
    assert_eq!(session.fact_families(), &families);

    let other_root = tempfile::tempdir().expect("different source root");
    assert!(CompilerFactSession::load(&capability, observations.path(), other_root.path()).is_err());
  }
}
