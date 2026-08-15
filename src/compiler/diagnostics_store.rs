//! Persistent store for compiler diagnostics cache entries.

use crate::compiler::facts::{
  CompilerFactCoverage, CompilerFactObject, CompilerFactProducerAuthority, FRAGMENT_OBJECT_IDENTITY_PREFIX,
};
use crate::compiler::model::{
  COLLECTOR_VERSION, COMPILER_DIAG_CACHE_VERSION, CompilerDiagCacheFile, CompilerDiagEntry, CompilerDiagKey,
  DiagnosticsCompleteness, MAX_COMPILER_DIAG_CACHE_ENTRIES, TargetEvidence,
};
use crate::error::{RailError, RailResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CACHE_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
pub(crate) const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CACHE_ENTRY_BYTES: usize = 8 * 1024 * 1024;

pub(crate) const EVIDENCE_CANDIDATE_KEY_PREFIX: &str = "compiler-evidence-candidate-v1-sha256-";
pub(crate) const EVIDENCE_ACTION_KEY_PREFIX: &str = "compiler-evidence-action-v1-sha256-";
pub(crate) const EVIDENCE_OBJECT_PREFIX: &str = "compiler-evidence-v1-sha256-";
const EVIDENCE_RESULT_PREFIX: &str = "compiler-evidence-result-v1-sha256-";
const EVIDENCE_VALIDATION_VERSION: u32 = 1;
const EVIDENCE_OBJECT_VERSION: u32 = 1;
const FACT_EVIDENCE_VALIDATION_VERSION: u32 = 1;
const FACT_EVIDENCE_OBJECT_VERSION: u32 = 1;

/// Fully observed authority for one compiler-evidence result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum CompilerEvidenceValidation {
  Diagnostics(CompilerDiagnosticsEvidenceValidation),
  CompilerFacts(CompilerFactEvidenceValidation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerDiagnosticsEvidenceValidation {
  version: u32,
  action_key: String,
  candidate_key: String,
  collector_version: u32,
  key: CompilerDiagKey,
  observations: Vec<crate::compiler::observation::CompilationObservationManifest>,
}

/// One root-independent scheduled compiler-fact acquisition identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactCacheKey {
  version: u32,
  view_identity: String,
  cargo_packages: Vec<CompilerDiagKey>,
  typed_packages: BTreeSet<String>,
  producer_authority: CompilerFactProducerAuthority,
  required_coverage: BTreeSet<CompilerFactCoverage>,
}

/// One immutable object named by a complete compiler-fact set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactObjectReference {
  pub(crate) object_identity: String,
  pub(crate) unit_identity: String,
  pub(crate) package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CompilerFactEvidenceValidationKind {
  Object {
    producer_authority: CompilerFactProducerAuthority,
    required_coverage: BTreeSet<CompilerFactCoverage>,
    reference: CompilerFactObjectReference,
  },
  Set {
    cache_key: CompilerFactCacheKey,
    objects: Vec<CompilerFactObjectReference>,
  },
}

/// CAS validation authority for either one fact object or one complete set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactEvidenceValidation {
  fact_validation_version: u32,
  action_key: String,
  candidate_key: String,
  validation: CompilerFactEvidenceValidationKind,
}

impl CompilerEvidenceValidation {
  fn from_entry(entry: &CompilerDiagEntry) -> RailResult<Self> {
    let key = canonical_evidence_key(&entry.key);
    let candidate_key = evidence_candidate_key(&key)?;
    let action_key = evidence_action_key(&key, entry.collector_version, &entry.observations)?;
    Ok(Self::Diagnostics(CompilerDiagnosticsEvidenceValidation {
      version: EVIDENCE_VALIDATION_VERSION,
      action_key,
      candidate_key,
      collector_version: entry.collector_version,
      key,
      observations: entry.observations.clone(),
    }))
  }

  pub(crate) fn action_key(&self) -> &str {
    match self {
      Self::Diagnostics(validation) => &validation.action_key,
      Self::CompilerFacts(validation) => &validation.action_key,
    }
  }

  pub(crate) fn candidate_key(&self) -> &str {
    match self {
      Self::Diagnostics(validation) => &validation.candidate_key,
      Self::CompilerFacts(validation) => &validation.candidate_key,
    }
  }

  fn diagnostics(&self) -> Option<&CompilerDiagnosticsEvidenceValidation> {
    match self {
      Self::Diagnostics(validation) => Some(validation),
      Self::CompilerFacts(_) => None,
    }
  }

  pub(crate) fn compiler_facts(&self) -> Option<&CompilerFactEvidenceValidation> {
    match self {
      Self::Diagnostics(_) => None,
      Self::CompilerFacts(validation) => Some(validation),
    }
  }

  pub(crate) fn validate_object(&self) -> RailResult<()> {
    match self {
      Self::Diagnostics(validation) => validation.validate_object(),
      Self::CompilerFacts(validation) => validation.validate_object(),
    }
  }

  pub(crate) fn result_digest(&self, evidence: &str) -> String {
    framed_sha256(
      EVIDENCE_RESULT_PREFIX,
      b"cargo-rail-compiler-evidence-result\0",
      &[
        (b"action-key", self.action_key().as_bytes()),
        (b"evidence", evidence.as_bytes()),
      ],
    )
  }
}

impl CompilerDiagnosticsEvidenceValidation {
  fn collector_version(&self) -> u32 {
    self.collector_version
  }

  fn observations(&self) -> &[crate::compiler::observation::CompilationObservationManifest] {
    &self.observations
  }

  fn matches(&self, key: &CompilerDiagKey) -> bool {
    semantic_key_bytes(&self.key).is_ok_and(|stored| semantic_key_bytes(key).is_ok_and(|current| stored == current))
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.version != EVIDENCE_VALIDATION_VERSION {
      return Err(RailError::message(
        "compiler evidence validation has an incompatible schema",
      ));
    }
    if evidence_candidate_key(&self.key)? != self.candidate_key
      || evidence_action_key(&self.key, self.collector_version, &self.observations)? != self.action_key
    {
      return Err(RailError::message(
        "compiler evidence validation identity does not match its observed inputs",
      ));
    }
    Ok(())
  }
}

impl CompilerFactCacheKey {
  pub(crate) fn new(
    view_identity: String,
    cargo_packages: Vec<CompilerDiagKey>,
    typed_packages: BTreeSet<String>,
    producer_authority: CompilerFactProducerAuthority,
    required_coverage: BTreeSet<CompilerFactCoverage>,
  ) -> RailResult<Self> {
    let mut cargo_packages = cargo_packages
      .into_iter()
      .map(|key| canonical_evidence_key(&key))
      .collect::<Vec<_>>();
    cargo_packages.sort_by(|left, right| left.package_name.cmp(&right.package_name));
    let key = Self {
      version: FACT_EVIDENCE_VALIDATION_VERSION,
      view_identity,
      cargo_packages,
      typed_packages,
      producer_authority,
      required_coverage,
    };
    key.validate()?;
    Ok(key)
  }

  fn validate(&self) -> RailResult<()> {
    if self.version != FACT_EVIDENCE_VALIDATION_VERSION {
      return Err(RailError::message("compiler fact cache key has an incompatible schema"));
    }
    validate_identity(&self.view_identity, crate::compiler::facts::VIEW_IDENTITY_PREFIX)?;
    self.producer_authority.validate()?;
    if self.cargo_packages.is_empty() || self.typed_packages.is_empty() || self.required_coverage.is_empty() {
      return Err(RailError::message("compiler fact cache key is incomplete"));
    }
    let mut previous = None;
    for package in &self.cargo_packages {
      if package.package_name.is_empty()
        || package.package_id.repr != package.package_name
        || previous.is_some_and(|name: &str| name >= package.package_name.as_str())
      {
        return Err(RailError::message(
          "compiler fact cache packages are not canonical, unique, and strictly sorted",
        ));
      }
      previous = Some(package.package_name.as_str());
    }
    if self
      .typed_packages
      .iter()
      .any(|package| !self.cargo_packages.iter().any(|key| &key.package_name == package))
    {
      return Err(RailError::message(
        "compiler fact cache typed packages are outside the Cargo package set",
      ));
    }
    Ok(())
  }

  fn canonical_bytes(&self) -> RailResult<Vec<u8>> {
    self.validate()?;
    serde_json::to_vec(self).map_err(Into::into)
  }

  pub(crate) fn producer_authority(&self) -> &CompilerFactProducerAuthority {
    &self.producer_authority
  }

  pub(crate) fn required_coverage(&self) -> &BTreeSet<CompilerFactCoverage> {
    &self.required_coverage
  }

  pub(crate) fn typed_packages(&self) -> &BTreeSet<String> {
    &self.typed_packages
  }
}

impl CompilerFactObjectReference {
  pub(crate) fn new(object_identity: String, unit_identity: String, package: String) -> RailResult<Self> {
    let reference = Self {
      object_identity,
      unit_identity,
      package,
    };
    reference.validate()?;
    Ok(reference)
  }

  fn validate(&self) -> RailResult<()> {
    validate_identity(&self.object_identity, FRAGMENT_OBJECT_IDENTITY_PREFIX)?;
    validate_identity(&self.unit_identity, crate::compiler::facts::UNIT_IDENTITY_PREFIX)?;
    if self.package.is_empty() || self.package.len() > 1024 || self.package.contains(['\0', '\n', '\r']) {
      return Err(RailError::message(
        "compiler fact cache reference has an invalid package name",
      ));
    }
    Ok(())
  }
}

impl CompilerFactEvidenceValidation {
  pub(crate) fn object(
    producer_authority: CompilerFactProducerAuthority,
    required_coverage: BTreeSet<CompilerFactCoverage>,
    reference: CompilerFactObjectReference,
  ) -> RailResult<CompilerEvidenceValidation> {
    producer_authority.validate()?;
    reference.validate()?;
    if required_coverage.is_empty() {
      return Err(RailError::message("compiler fact object cache coverage is empty"));
    }
    let validation = CompilerFactEvidenceValidationKind::Object {
      producer_authority,
      required_coverage,
      reference,
    };
    Self::bind(validation).map(CompilerEvidenceValidation::CompilerFacts)
  }

  pub(crate) fn set(
    cache_key: CompilerFactCacheKey,
    objects: Vec<CompilerFactObjectReference>,
  ) -> RailResult<CompilerEvidenceValidation> {
    cache_key.validate()?;
    validate_fact_references(&objects)?;
    Self::bind(CompilerFactEvidenceValidationKind::Set { cache_key, objects })
      .map(CompilerEvidenceValidation::CompilerFacts)
  }

  pub(crate) fn set_candidate_key(cache_key: &CompilerFactCacheKey) -> RailResult<String> {
    Ok(framed_sha256(
      EVIDENCE_CANDIDATE_KEY_PREFIX,
      b"cargo-rail-compiler-fact-set-candidate-v1\0",
      &[(b"cache-key", &cache_key.canonical_bytes()?)],
    ))
  }

  fn bind(validation: CompilerFactEvidenceValidationKind) -> RailResult<Self> {
    let candidate_key = fact_validation_candidate_key(&validation)?;
    let validation_bytes = serde_json::to_vec(&validation)?;
    let action_key = framed_sha256(
      EVIDENCE_ACTION_KEY_PREFIX,
      b"cargo-rail-compiler-fact-action-v1\0",
      &[
        (b"candidate-key", candidate_key.as_bytes()),
        (b"validation", &validation_bytes),
      ],
    );
    Ok(Self {
      fact_validation_version: FACT_EVIDENCE_VALIDATION_VERSION,
      action_key,
      candidate_key,
      validation,
    })
  }

  pub(crate) fn set_objects<'a>(
    &'a self,
    expected: &CompilerFactCacheKey,
  ) -> Option<&'a [CompilerFactObjectReference]> {
    match &self.validation {
      CompilerFactEvidenceValidationKind::Set { cache_key, objects } if cache_key == expected => Some(objects),
      CompilerFactEvidenceValidationKind::Object { .. } | CompilerFactEvidenceValidationKind::Set { .. } => None,
    }
  }

  pub(crate) fn matches_object(
    &self,
    producer_authority: &CompilerFactProducerAuthority,
    required_coverage: &BTreeSet<CompilerFactCoverage>,
    expected: &CompilerFactObjectReference,
  ) -> bool {
    matches!(
      &self.validation,
      CompilerFactEvidenceValidationKind::Object {
        producer_authority: producer,
        required_coverage: coverage,
        reference,
      } if producer == producer_authority && coverage == required_coverage && reference == expected
    )
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.fact_validation_version != FACT_EVIDENCE_VALIDATION_VERSION {
      return Err(RailError::message(
        "compiler fact evidence validation has an incompatible schema",
      ));
    }
    match &self.validation {
      CompilerFactEvidenceValidationKind::Object {
        producer_authority,
        required_coverage,
        reference,
      } => {
        producer_authority.validate()?;
        reference.validate()?;
        if required_coverage.is_empty() {
          return Err(RailError::message("compiler fact object cache coverage is empty"));
        }
      }
      CompilerFactEvidenceValidationKind::Set { cache_key, objects } => {
        cache_key.validate()?;
        validate_fact_references(objects)?;
      }
    }
    if fact_validation_candidate_key(&self.validation)? != self.candidate_key {
      return Err(RailError::message(
        "compiler fact evidence candidate identity does not match its authority",
      ));
    }
    let rebound = Self::bind(self.validation.clone())?;
    if rebound.action_key != self.action_key {
      return Err(RailError::message(
        "compiler fact evidence action identity does not match its authority",
      ));
    }
    Ok(())
  }
}

fn fact_validation_candidate_key(validation: &CompilerFactEvidenceValidationKind) -> RailResult<String> {
  match validation {
    CompilerFactEvidenceValidationKind::Object {
      producer_authority,
      required_coverage,
      reference,
    } => Ok(framed_sha256(
      EVIDENCE_CANDIDATE_KEY_PREFIX,
      b"cargo-rail-compiler-fact-object-candidate-v1\0",
      &[
        (b"producer", &serde_json::to_vec(producer_authority)?),
        (b"coverage", &serde_json::to_vec(required_coverage)?),
        (b"reference", &serde_json::to_vec(reference)?),
      ],
    )),
    CompilerFactEvidenceValidationKind::Set { cache_key, .. } => {
      CompilerFactEvidenceValidation::set_candidate_key(cache_key)
    }
  }
}

fn validate_fact_references(objects: &[CompilerFactObjectReference]) -> RailResult<()> {
  if objects.is_empty() {
    return Err(RailError::message("compiler fact cache set is empty"));
  }
  let mut previous = None;
  let mut units = BTreeSet::new();
  for reference in objects {
    reference.validate()?;
    if previous.is_some_and(|value: &CompilerFactObjectReference| value >= reference)
      || !units.insert(reference.unit_identity.as_str())
    {
      return Err(RailError::message(
        "compiler fact cache references are not unique by object and unit and strictly sorted",
      ));
    }
    previous = Some(reference);
  }
  Ok(())
}

/// Deterministic typed compiler evidence stored by the local CAS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum CompilerEvidenceObject {
  Diagnostics(CompilerDiagnosticsEvidenceObject),
  CompilerFacts(CompilerFactEvidenceObject),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerDiagnosticsEvidenceObject {
  version: u32,
  evidence: TargetEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum CompilerFactEvidencePayload {
  Object(Box<CompilerFactObject>),
  Set(Vec<CompilerFactObjectReference>),
}

/// Immutable compiler-fact payload stored through the shared CAS lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactEvidenceObject {
  fact_object_version: u32,
  payload: CompilerFactEvidencePayload,
}

impl CompilerEvidenceObject {
  pub(crate) fn from_entry(entry: &CompilerDiagEntry) -> Self {
    Self::Diagnostics(CompilerDiagnosticsEvidenceObject {
      version: EVIDENCE_OBJECT_VERSION,
      evidence: entry.evidence.clone(),
    })
  }

  pub(crate) fn diagnostics_evidence(&self) -> Option<&TargetEvidence> {
    match self {
      Self::Diagnostics(object) => Some(&object.evidence),
      Self::CompilerFacts(_) => None,
    }
  }

  pub(crate) fn compiler_facts(&self) -> Option<&CompilerFactEvidenceObject> {
    match self {
      Self::Diagnostics(_) => None,
      Self::CompilerFacts(object) => Some(object),
    }
  }

  pub(crate) fn from_compiler_fact_object(object: CompilerFactObject) -> Self {
    Self::CompilerFacts(CompilerFactEvidenceObject {
      fact_object_version: FACT_EVIDENCE_OBJECT_VERSION,
      payload: CompilerFactEvidencePayload::Object(Box::new(object)),
    })
  }

  pub(crate) fn from_compiler_fact_set(objects: Vec<CompilerFactObjectReference>) -> RailResult<Self> {
    validate_fact_references(&objects)?;
    Ok(Self::CompilerFacts(CompilerFactEvidenceObject {
      fact_object_version: FACT_EVIDENCE_OBJECT_VERSION,
      payload: CompilerFactEvidencePayload::Set(objects),
    }))
  }

  pub(crate) fn identity(&self) -> RailResult<String> {
    match self {
      Self::Diagnostics(object) if object.version != EVIDENCE_OBJECT_VERSION => {
        return Err(RailError::message(
          "compiler evidence object has an incompatible schema",
        ));
      }
      Self::CompilerFacts(object) if object.fact_object_version != FACT_EVIDENCE_OBJECT_VERSION => {
        return Err(RailError::message(
          "compiler fact evidence object has an incompatible schema",
        ));
      }
      _ => {}
    }
    let bytes = serde_json::to_vec(self)?;
    Ok(framed_sha256(
      EVIDENCE_OBJECT_PREFIX,
      b"cargo-rail-compiler-evidence\0",
      &[(b"object", &bytes)],
    ))
  }
}

impl CompilerFactEvidenceObject {
  pub(crate) fn fact_object(&self) -> Option<&CompilerFactObject> {
    match &self.payload {
      CompilerFactEvidencePayload::Object(object) => Some(object.as_ref()),
      CompilerFactEvidencePayload::Set(_) => None,
    }
  }

  pub(crate) fn fact_set(&self) -> Option<&[CompilerFactObjectReference]> {
    match &self.payload {
      CompilerFactEvidencePayload::Object(_) => None,
      CompilerFactEvidencePayload::Set(objects) => Some(objects),
    }
  }
}

pub(crate) fn validate_evidence_action_key(value: &str) -> RailResult<()> {
  validate_identity(value, EVIDENCE_ACTION_KEY_PREFIX)
}

pub(crate) fn validate_evidence_candidate_key(value: &str) -> RailResult<()> {
  validate_identity(value, EVIDENCE_CANDIDATE_KEY_PREFIX)
}

pub(crate) fn validate_evidence_object(value: &str) -> RailResult<()> {
  validate_identity(value, EVIDENCE_OBJECT_PREFIX)
}

fn evidence_candidate_key(key: &CompilerDiagKey) -> RailResult<String> {
  let package = logical_package_identity(key);
  let features = serde_json::to_vec(&key.features)?;
  Ok(framed_sha256(
    EVIDENCE_CANDIDATE_KEY_PREFIX,
    b"cargo-rail-compiler-evidence-candidate\0",
    &[
      (b"package", package.as_bytes()),
      (b"target", key.target.as_str().as_bytes()),
      (b"features", &features),
    ],
  ))
}

fn evidence_action_key(
  key: &CompilerDiagKey,
  collector_version: u32,
  observations: &[crate::compiler::observation::CompilationObservationManifest],
) -> RailResult<String> {
  let key = semantic_key_bytes(key)?;
  let observations = serde_json::to_vec(observations)?;
  Ok(framed_sha256(
    EVIDENCE_ACTION_KEY_PREFIX,
    b"cargo-rail-compiler-evidence-action\0",
    &[
      (b"collector-version", &collector_version.to_le_bytes()),
      (b"key", &key),
      (b"observations", &observations),
    ],
  ))
}

fn semantic_key_bytes(key: &CompilerDiagKey) -> RailResult<Vec<u8>> {
  #[derive(Serialize)]
  struct SemanticKey<'a> {
    package: &'a str,
    target: &'a crate::compiler::model::PlatformTarget,
    features: &'a crate::compiler::model::FeatureSelection,
    rustc_version: &'a str,
    cargo_version: &'a str,
    host_triple: &'a str,
    toolchain_fingerprint: &'a str,
    target_fingerprint: &'a str,
    lock_fingerprint: &'a str,
    manifest_fingerprint: &'a str,
    source_fingerprint: &'a str,
    compiler_env_fingerprint: &'a str,
    cargo_config_fingerprint: &'a str,
  }

  serde_json::to_vec(&SemanticKey {
    package: logical_package_identity(key),
    target: &key.target,
    features: &key.features,
    rustc_version: &key.rustc_version,
    cargo_version: &key.cargo_version,
    host_triple: &key.host_triple,
    toolchain_fingerprint: &key.toolchain_fingerprint,
    target_fingerprint: &key.target_fingerprint,
    lock_fingerprint: &key.lock_fingerprint,
    manifest_fingerprint: &key.manifest_fingerprint,
    source_fingerprint: &key.source_fingerprint,
    compiler_env_fingerprint: &key.compiler_env_fingerprint,
    cargo_config_fingerprint: &key.cargo_config_fingerprint,
  })
  .map_err(Into::into)
}

fn logical_package_identity(key: &CompilerDiagKey) -> &str {
  if !key.package_name.is_empty() {
    &key.package_name
  } else if key.package_id.repr.starts_with("path+") {
    key
      .package_id
      .repr
      .rsplit_once('#')
      .map_or(&key.package_id.repr, |(_, logical)| logical)
  } else {
    &key.package_id.repr
  }
}

fn canonical_evidence_key(key: &CompilerDiagKey) -> CompilerDiagKey {
  let mut canonical = key.clone();
  canonical.package_name = logical_package_identity(key).to_string();
  canonical.package_id.repr.clone_from(&canonical.package_name);
  canonical
}

fn framed_sha256(prefix: &str, domain: &[u8], frames: &[(&[u8], &[u8])]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(domain);
  for (tag, value) in frames {
    hasher.update((tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
  }
  crate::instrumentation::record_hash_operation();
  format!(
    "{prefix}{}",
    crate::source::ContentDigest::from_sha256_bytes(hasher.finalize().into())
  )
}

fn validate_identity(value: &str, prefix: &str) -> RailResult<()> {
  let digest = value
    .strip_prefix(prefix)
    .ok_or_else(|| RailError::message("compiler evidence identity has the wrong domain or version"))?;
  if digest.len() != 64
    || !digest
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
  {
    return Err(RailError::message(
      "compiler evidence identity is not canonical SHA-256",
    ));
  }
  Ok(())
}

/// Persistent compiler diagnostics store.
pub struct CompilerDiagnosticsStore {
  path: PathBuf,
  cache: CompilerDiagCacheFile,
  cas: Option<crate::cache::cas::LocalCas>,
  legacy_digest: Option<crate::source::ContentDigest>,
  pending: HashSet<String>,
  dirty: bool,
  prior_by_configuration: HashMap<String, (u64, CompilerDiagKey, u32)>,
  cached_packages: HashSet<String>,
  discarded_reason: Option<&'static str>,
}

impl CompilerDiagnosticsStore {
  /// Load compiler evidence from the shared local CAS and any bounded legacy file.
  pub fn load(workspace_root: &Path) -> Self {
    let cas = crate::cache::cas::LocalCas::open().ok();
    Self::load_with_cas(workspace_root, cas)
  }

  fn load_with_cas(workspace_root: &Path, cas: Option<crate::cache::cas::LocalCas>) -> Self {
    let path = crate::workspace::cargo_rail_state_root(workspace_root)
      .join("cache")
      .join("compiler-diags-v1.json");

    let (cache, legacy_digest, discarded_reason) = match read_cache(&path) {
      Ok(Some(mut legacy)) if legacy.cache.version == COMPILER_DIAG_CACHE_VERSION => {
        match prune_entries(
          std::mem::take(&mut legacy.cache.entries),
          now_unix_ms(),
          CacheLimits::PRODUCTION,
        ) {
          Ok((entries, pruned)) => {
            let _ = pruned;
            legacy.cache.entries = entries;
            (legacy.cache, Some(legacy.digest), None)
          }
          Err(_) => (CompilerDiagCacheFile::default(), None, Some("cache_unreadable")),
        }
      }
      Ok(Some(_)) => (CompilerDiagCacheFile::default(), None, Some("schema_changed")),
      Ok(None) => (CompilerDiagCacheFile::default(), None, None),
      Err(reason) => (CompilerDiagCacheFile::default(), None, Some(reason)),
    };
    let dirty = legacy_digest.is_some();
    let discarded_reason = discarded_reason.or_else(|| store_unavailable_reason(dirty, &cas));
    let mut store = Self {
      path,
      cache,
      cas,
      legacy_digest,
      pending: HashSet::new(),
      dirty,
      prior_by_configuration: HashMap::new(),
      cached_packages: HashSet::new(),
      discarded_reason,
    };
    store.rebuild_index();
    store
  }

  #[cfg(test)]
  fn load_legacy_only(workspace_root: &Path) -> Self {
    Self::load_with_cas(workspace_root, None)
  }

  /// Return cached entry for the exact key.
  pub fn get(&mut self, key: &CompilerDiagKey) -> Option<&CompilerDiagEntry> {
    let id = key.stable_id();
    let legacy_hit = self.cache.entries.get(&id).is_some_and(|entry| {
      entry.collector_version == COLLECTOR_VERSION
        && entry.evidence.completeness == DiagnosticsCompleteness::Complete
        && semantic_key_bytes(&entry.key)
          .is_ok_and(|stored| semantic_key_bytes(key).is_ok_and(|current| stored == current))
    });
    if legacy_hit {
      return self.cache.entries.get(&id);
    }

    let candidate_key = match evidence_candidate_key(key) {
      Ok(candidate_key) => candidate_key,
      Err(_) => {
        self.discarded_reason = Some("cache_identity_unavailable");
        return None;
      }
    };
    let candidates = match self
      .cas
      .as_ref()
      .map(|cas| cas.compiler_evidence_candidates(&candidate_key))
      .transpose()
    {
      Ok(Some(candidates)) => candidates,
      Ok(None) => return None,
      Err(_) => {
        self.discarded_reason = Some("local_cache_unreadable");
        return None;
      }
    };
    let mut hit = None;
    for candidate in candidates {
      let Some(validation) = candidate.validation.diagnostics() else {
        continue;
      };
      let Some(evidence) = candidate.evidence.diagnostics_evidence() else {
        continue;
      };
      if evidence.completeness != DiagnosticsCompleteness::Complete {
        continue;
      }
      let generated_at_unix_ms = u64::try_from(candidate.created_unix_nanos / 1_000_000).unwrap_or(u64::MAX);
      self.record_prior(generated_at_unix_ms, &validation.key, validation.collector_version());
      if hit.is_none() && validation.collector_version() == COLLECTOR_VERSION && validation.matches(key) {
        hit = Some(CompilerDiagEntry {
          key: key.clone(),
          evidence: evidence.clone(),
          generated_at_unix_ms,
          collector_version: validation.collector_version(),
          observations: validation.observations().to_vec(),
        });
      }
    }
    if let Some(entry) = hit {
      self.cache.entries.insert(id.clone(), entry);
    }
    self.cache.entries.get(&id)
  }

  /// Explain why an exact semantic key was not reusable.
  pub fn miss_reason(&self, key: &CompilerDiagKey) -> &'static str {
    if let Some(reason) = self.discarded_reason {
      return reason;
    }
    let Some((_, prior, collector_version)) = self.prior_by_configuration.get(&configuration_id(key)) else {
      return if self.cached_packages.contains(logical_package_identity(key)) {
        "configuration_not_cached"
      } else {
        "cold_cache"
      };
    };
    if *collector_version != COLLECTOR_VERSION {
      "collector_changed"
    } else if prior.toolchain_fingerprint != key.toolchain_fingerprint
      || prior.rustc_version != key.rustc_version
      || prior.cargo_version != key.cargo_version
      || prior.host_triple != key.host_triple
    {
      "toolchain_changed"
    } else if prior.target_fingerprint != key.target_fingerprint {
      "target_identity_changed"
    } else if prior.lock_fingerprint != key.lock_fingerprint {
      "lockfile_changed"
    } else if prior.manifest_fingerprint != key.manifest_fingerprint {
      "manifest_changed"
    } else if prior.source_fingerprint != key.source_fingerprint {
      "source_changed"
    } else if prior.compiler_env_fingerprint != key.compiler_env_fingerprint {
      "compiler_environment_changed"
    } else if prior.cargo_config_fingerprint != key.cargo_config_fingerprint {
      "cargo_config_changed"
    } else {
      "semantic_key_changed"
    }
  }

  /// Upsert one entry.
  pub fn put(&mut self, entry: CompilerDiagEntry) {
    if entry.evidence.completeness != DiagnosticsCompleteness::Complete {
      return;
    }
    let id = entry.key.stable_id();
    self.record_prior(entry.generated_at_unix_ms, &entry.key, entry.collector_version);
    self.cache.entries.insert(id.clone(), entry);
    self.pending.insert(id);
    self.dirty = true;
  }

  /// Persist dirty cache state to disk.
  pub fn flush(&mut self) -> RailResult<()> {
    if !self.dirty {
      return Ok(());
    }

    self.prune()?;
    let Some(cas) = &self.cas else {
      self.dirty = false;
      self.pending.clear();
      return Ok(());
    };
    let publish_all = self.legacy_digest.is_some();
    for (id, entry) in &self.cache.entries {
      if !publish_all && !self.pending.contains(id) {
        continue;
      }
      let validation = CompilerEvidenceValidation::from_entry(entry)?;
      let evidence = CompilerEvidenceObject::from_entry(entry);
      cas.store_compiler_evidence(crate::cache::cas::CompilerEvidenceStoreRequest {
        validation: &validation,
        evidence: &evidence,
      })?;
    }
    if let Some(digest) = self.legacy_digest
      && remove_legacy_if_unchanged(&self.path, digest)?
    {
      self.legacy_digest = None;
    }
    self.dirty = false;
    self.pending.clear();
    Ok(())
  }

  fn prune(&mut self) -> RailResult<()> {
    let (entries, _) = prune_entries(
      std::mem::take(&mut self.cache.entries),
      now_unix_ms(),
      CacheLimits::PRODUCTION,
    )?;
    self.cache.entries = entries;
    self.rebuild_index();
    Ok(())
  }

  fn rebuild_index(&mut self) {
    self.prior_by_configuration.clear();
    self.cached_packages.clear();
    let entries = self
      .cache
      .entries
      .values()
      .map(|entry| (entry.generated_at_unix_ms, entry.key.clone(), entry.collector_version))
      .collect::<Vec<_>>();
    for (generated_at_unix_ms, key, collector_version) in entries {
      self.record_prior(generated_at_unix_ms, &key, collector_version);
    }
  }

  fn record_prior(&mut self, generated_at_unix_ms: u64, key: &CompilerDiagKey, collector_version: u32) {
    self.cached_packages.insert(logical_package_identity(key).to_string());
    let id = configuration_id(key);
    let replace = self
      .prior_by_configuration
      .get(&id)
      .is_none_or(|(generated, _, _)| *generated < generated_at_unix_ms);
    if replace {
      self
        .prior_by_configuration
        .insert(id, (generated_at_unix_ms, key.clone(), collector_version));
    }
  }
}

#[derive(Clone, Copy)]
struct CacheLimits {
  entries: usize,
  age_ms: u64,
  file_bytes: usize,
  entry_bytes: usize,
}

impl CacheLimits {
  const PRODUCTION: Self = Self {
    entries: MAX_COMPILER_DIAG_CACHE_ENTRIES,
    age_ms: MAX_CACHE_AGE_MS,
    file_bytes: MAX_CACHE_BYTES,
    entry_bytes: MAX_CACHE_ENTRY_BYTES,
  };
}

struct LegacyCache {
  cache: CompilerDiagCacheFile,
  digest: crate::source::ContentDigest,
}

fn read_cache(path: &Path) -> Result<Option<LegacyCache>, &'static str> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(_) => return Err("cache_unreadable"),
  };
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !has_single_link(&metadata) {
    return Err("cache_not_bounded_regular_file");
  }
  if metadata.len() > MAX_CACHE_BYTES as u64 {
    return Err("cache_too_large");
  }

  let mut file = File::open(path).map_err(|_| "cache_unreadable")?;
  let opened = file.metadata().map_err(|_| "cache_unreadable")?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != metadata.len() {
    return Err("cache_changed_before_read");
  }

  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  (&mut file)
    .take(metadata.len().saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(|_| "cache_unreadable")?;
  if bytes.len() as u64 != metadata.len() {
    return Err("cache_changed_while_reading");
  }
  let digest = crate::source::ContentDigest::sha256(&bytes);
  serde_json::from_slice(&bytes)
    .map(|cache| Some(LegacyCache { cache, digest }))
    .map_err(|_| "cache_unreadable")
}

fn remove_legacy_if_unchanged(path: &Path, expected: crate::source::ContentDigest) -> RailResult<bool> {
  let Some(legacy) = read_cache(path).map_err(RailError::message)? else {
    return Ok(true);
  };
  if legacy.digest != expected {
    return Ok(false);
  }
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !has_single_link(&metadata) {
    return Ok(false);
  }
  fs::remove_file(path)?;
  if let Some(parent) = path.parent() {
    match fs::remove_dir(parent) {
      Ok(()) => {}
      Err(error)
        if matches!(
          error.kind(),
          std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
        ) => {}
      Err(error) => return Err(error.into()),
    }
  }
  Ok(true)
}

fn store_unavailable_reason(legacy_available: bool, cas: &Option<crate::cache::cas::LocalCas>) -> Option<&'static str> {
  (!legacy_available && cas.is_none()).then_some("local_cache_unavailable")
}

fn prune_entries(
  entries: BTreeMap<String, CompilerDiagEntry>,
  now: u64,
  limits: CacheLimits,
) -> RailResult<(BTreeMap<String, CompilerDiagEntry>, bool)> {
  let original_len = entries.len();
  let mut candidates = entries
    .into_iter()
    .filter(|(id, entry)| {
      id == &entry.key.stable_id()
        && entry.evidence.completeness == DiagnosticsCompleteness::Complete
        && now.saturating_sub(entry.generated_at_unix_ms) <= limits.age_ms
    })
    .collect::<Vec<_>>();
  candidates.sort_unstable_by(|(left_id, left), (right_id, right)| {
    right
      .generated_at_unix_ms
      .cmp(&left.generated_at_unix_ms)
      .then_with(|| left_id.cmp(right_id))
  });

  let mut retained = BTreeMap::new();
  let mut retained_bytes = serialized_len(&CompilerDiagCacheFile::default())?;
  for (id, entry) in candidates {
    if retained.len() == limits.entries {
      break;
    }
    let id_bytes = serialized_len(&id)?;
    let entry_bytes = serialized_len(&entry)?;
    let entry_record_bytes = id_bytes
      .checked_add(1)
      .and_then(|bytes| bytes.checked_add(entry_bytes))
      .ok_or_else(|| RailError::message("compiler diagnostics cache size overflow"))?;
    let file_bytes = entry_record_bytes
      .checked_add(usize::from(!retained.is_empty()))
      .ok_or_else(|| RailError::message("compiler diagnostics cache size overflow"))?;
    let Some(total_bytes) = retained_bytes.checked_add(file_bytes) else {
      continue;
    };
    if entry_record_bytes > limits.entry_bytes || total_bytes > limits.file_bytes {
      continue;
    }
    retained_bytes = total_bytes;
    retained.insert(id, entry);
  }
  let pruned = retained.len() != original_len;
  Ok((retained, pruned))
}

fn serialized_len(value: &impl Serialize) -> RailResult<usize> {
  #[derive(Default)]
  struct Counter(usize);

  impl std::io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
      self.0 = self
        .0
        .checked_add(bytes.len())
        .ok_or_else(|| std::io::Error::other("serialized compiler diagnostics size overflow"))?;
      Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  let mut counter = Counter::default();
  serde_json::to_writer(&mut counter, value)?;
  Ok(counter.0)
}

fn now_unix_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_millis() as u64)
    .unwrap_or(0)
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

fn configuration_id(key: &CompilerDiagKey) -> String {
  format!(
    "{}\u{1f}{}\u{1f}{}",
    logical_package_identity(key),
    key.target.as_str(),
    key.features.label()
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compiler::model::{DiagnosticsCompleteness, FeatureSelection, PlatformTarget, TargetEvidence};
  use cargo_metadata::PackageId;
  use std::collections::{BTreeMap, BTreeSet};

  fn key() -> CompilerDiagKey {
    CompilerDiagKey {
      package_id: PackageId {
        repr: "path+file:///workspace#member@0.1.0".to_string(),
      },
      package_name: "member".to_string(),
      target: PlatformTarget::from("default"),
      features: FeatureSelection::Default,
      rustc_version: "rustc test".to_string(),
      cargo_version: "cargo test".to_string(),
      host_triple: "test-host".to_string(),
      toolchain_fingerprint: "sha256:toolchain".to_string(),
      target_fingerprint: "sha256:target".to_string(),
      lock_fingerprint: "sha256:lock".to_string(),
      manifest_fingerprint: "sha256:manifest".to_string(),
      source_fingerprint: "sha256:source".to_string(),
      compiler_env_fingerprint: "sha256:environment".to_string(),
      cargo_config_fingerprint: "sha256:configuration".to_string(),
    }
  }

  fn entry(package: &str, generated_at_unix_ms: u64, padding: usize) -> CompilerDiagEntry {
    let mut key = key();
    key.package_id.repr = format!("path+file:///workspace#{package}@0.1.0");
    key.rustc_version.push_str(&"x".repeat(padding));
    CompilerDiagEntry {
      key,
      evidence: TargetEvidence {
        platform: PlatformTarget::from("default"),
        features: FeatureSelection::Default,
        compiled_units: BTreeSet::new(),
        unused_crates: BTreeSet::new(),
        unit_evidence: Vec::new(),
        completeness: DiagnosticsCompleteness::Complete,
      },
      generated_at_unix_ms,
      collector_version: COLLECTOR_VERSION,
      observations: Vec::new(),
    }
  }

  fn entry_map(entries: impl IntoIterator<Item = CompilerDiagEntry>) -> BTreeMap<String, CompilerDiagEntry> {
    entries
      .into_iter()
      .map(|entry| (entry.key.stable_id(), entry))
      .collect()
  }

  fn encoded_cache_bytes(entries: BTreeMap<String, CompilerDiagEntry>) -> usize {
    serde_json::to_vec(&CompilerDiagCacheFile {
      version: COMPILER_DIAG_CACHE_VERSION,
      entries,
    })
    .expect("cache should serialize")
    .len()
  }

  fn pair_bytes(id: &str, entry: &CompilerDiagEntry) -> usize {
    serde_json::to_vec(id).expect("entry ID should serialize").len()
      + 1
      + serde_json::to_vec(entry).expect("entry should serialize").len()
  }

  #[test]
  fn prior_collector_semantics_never_authorize_reuse() {
    let root = tempfile::tempdir().expect("temporary workspace should be created");
    let path = root.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
    std::fs::create_dir_all(path.parent().expect("cache path should have a parent"))
      .expect("cache directory should be created");
    let key = key();
    let mut cache = CompilerDiagCacheFile::default();
    cache.entries.insert(
      key.stable_id(),
      CompilerDiagEntry {
        key: key.clone(),
        evidence: TargetEvidence {
          platform: PlatformTarget::from("default"),
          features: FeatureSelection::Default,
          compiled_units: BTreeSet::new(),
          unused_crates: BTreeSet::new(),
          unit_evidence: Vec::new(),
          completeness: DiagnosticsCompleteness::Complete,
        },
        generated_at_unix_ms: now_unix_ms(),
        collector_version: COLLECTOR_VERSION - 1,
        observations: Vec::new(),
      },
    );
    std::fs::write(&path, serde_json::to_vec(&cache).expect("cache should serialize"))
      .expect("cache should be written");

    let mut store = CompilerDiagnosticsStore::load_legacy_only(root.path());
    assert!(
      store.get(&key).is_none(),
      "prior collector evidence must not be returned"
    );
    assert_eq!(store.miss_reason(&key), "collector_changed");
  }

  #[test]
  fn pruning_retains_the_newest_entries_within_the_exact_file_bound() {
    let oldest = entry("oldest", 100, 0);
    let middle = entry("middle", 200, 0);
    let newest = entry("newest", 300, 0);
    let expected = entry_map([middle.clone(), newest.clone()]);
    let file_bytes = encoded_cache_bytes(expected.clone());
    let limits = CacheLimits {
      entries: usize::MAX,
      age_ms: u64::MAX,
      file_bytes,
      entry_bytes: usize::MAX,
    };

    let (retained, pruned) = prune_entries(entry_map([oldest, middle, newest]), 300, limits).expect("prune cache");

    assert!(pruned);
    assert_eq!(retained.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>());
    assert_eq!(encoded_cache_bytes(retained), file_bytes);
  }

  #[test]
  fn one_oversized_entry_does_not_displace_valid_evidence() {
    let oversized = entry("oversized", 300, 2048);
    let valid = entry("valid", 200, 0);
    let valid_id = valid.key.stable_id();
    let limits = CacheLimits {
      entries: 1,
      age_ms: u64::MAX,
      file_bytes: usize::MAX,
      entry_bytes: pair_bytes(&valid_id, &valid),
    };

    let (retained, pruned) = prune_entries(entry_map([oversized, valid.clone()]), 300, limits).expect("prune cache");

    assert!(pruned);
    let expected = entry_map([valid]);
    assert_eq!(retained.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>());
  }

  #[test]
  fn per_entry_bound_is_independent_of_map_position() {
    let first = entry("first", 300, 0);
    let bounded = entry("bounded", 200, 32);
    let bounded_id = bounded.key.stable_id();
    let expected = entry_map([first.clone(), bounded.clone()]);
    let limits = CacheLimits {
      entries: 2,
      age_ms: u64::MAX,
      file_bytes: usize::MAX,
      entry_bytes: pair_bytes(&bounded_id, &bounded),
    };

    let (retained, pruned) = prune_entries(entry_map([first, bounded]), 300, limits).expect("prune cache");

    assert!(!pruned);
    assert_eq!(retained.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>());
  }

  #[test]
  fn pruning_enforces_age_and_count_before_persistence() {
    let expired = entry("expired", 100, 0);
    let older = entry("older", 250, 0);
    let newest = entry("newest", 300, 0);
    let expected = entry_map([newest.clone()]);
    let limits = CacheLimits {
      entries: 1,
      age_ms: 100,
      file_bytes: usize::MAX,
      entry_bytes: usize::MAX,
    };

    let (retained, pruned) = prune_entries(entry_map([expired, older, newest]), 300, limits).expect("prune cache");

    assert!(pruned);
    assert_eq!(retained.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>());
  }

  #[test]
  fn deserialization_stops_at_the_entry_count_bound() {
    let entries = (0..=MAX_COMPILER_DIAG_CACHE_ENTRIES)
      .map(|index| entry(&format!("member-{index}"), 1, 0))
      .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&CompilerDiagCacheFile {
      version: COMPILER_DIAG_CACHE_VERSION,
      entries: entry_map(entries),
    })
    .expect("oversized entry map should serialize");

    let error = serde_json::from_slice::<CompilerDiagCacheFile>(&bytes)
      .expect_err("entry count above the bound must be rejected");
    assert!(
      error.to_string().contains("4096-entry bound"),
      "unexpected error: {error}"
    );
  }

  #[test]
  fn malformed_entry_binding_never_authorizes_reuse() {
    let root = tempfile::tempdir().expect("temporary workspace should be created");
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let path = root.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
    fs::create_dir_all(path.parent().expect("cache path should have a parent"))
      .expect("cache directory should be created");
    let entry = entry("member", now_unix_ms(), 0);
    let key = entry.key.clone();
    let cache = CompilerDiagCacheFile {
      version: COMPILER_DIAG_CACHE_VERSION,
      entries: BTreeMap::from([("substituted-key".to_string(), entry)]),
    };
    fs::write(&path, serde_json::to_vec(&cache).expect("cache should serialize")).expect("cache should be written");

    let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
    let mut store = CompilerDiagnosticsStore::load_with_cas(root.path(), Some(cas));
    assert!(store.get(&key).is_none());
    assert_eq!(store.miss_reason(&key), "cold_cache");
    store.flush().expect("pruned cache should persist");
    assert!(!path.exists(), "the fully migrated legacy file should be removed");
  }

  #[test]
  fn compiler_evidence_round_trips_across_equivalent_workspace_roots() {
    let first_root = tempfile::tempdir().expect("first workspace should be created");
    let second_root = tempfile::tempdir().expect("second workspace should be created");
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let first_cas =
      crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
    let original = entry("member", now_unix_ms(), 0);
    let mut first = CompilerDiagnosticsStore::load_with_cas(first_root.path(), Some(first_cas));
    first.put(original.clone());
    first.flush().expect("compiler evidence should publish");
    assert!(
      !first_root
        .path()
        .join("target/cargo-rail/cache/compiler-diags-v1.json")
        .exists(),
      "CAS publication must not recreate the monolithic legacy file"
    );

    let second_cas =
      crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should reopen");
    let mut equivalent_key = original.key.clone();
    equivalent_key.package_id.repr = "path+file:///different/root#member@0.1.0".to_string();
    let mut second = CompilerDiagnosticsStore::load_with_cas(second_root.path(), Some(second_cas));
    let reused = second
      .get(&equivalent_key)
      .expect("equivalent root should reuse evidence");

    assert_eq!(reused.key, equivalent_key);
    assert_eq!(reused.evidence, original.evidence);
    assert_eq!(reused.observations, original.observations);
  }

  #[test]
  fn incomplete_evidence_is_never_published_or_reused() {
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
    let mut incomplete = entry("member", now_unix_ms(), 0);
    incomplete.evidence.completeness = DiagnosticsCompleteness::Incomplete;
    let key = incomplete.key.clone();

    let mut store = CompilerDiagnosticsStore::load_with_cas(workspace.path(), Some(cas));
    store.put(incomplete);
    store
      .flush()
      .expect("an incomplete result should require no persistence");

    assert!(store.get(&key).is_none(), "incomplete evidence authorized reuse");
    let results = cache_root.path().join("cargo-rail/local-cas-v2/results");
    assert!(
      fs::read_dir(results)
        .expect("results directory should remain readable")
        .next()
        .is_none(),
      "incomplete evidence was published"
    );
  }

  #[test]
  fn valid_legacy_evidence_is_imported_once_before_the_monolith_is_removed() {
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let path = workspace.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
    fs::create_dir_all(path.parent().expect("legacy cache should have a parent"))
      .expect("legacy cache directory should be created");
    let original = entry("member", now_unix_ms(), 0);
    fs::write(
      &path,
      serde_json::to_vec(&CompilerDiagCacheFile {
        version: COMPILER_DIAG_CACHE_VERSION,
        entries: entry_map([original.clone()]),
      })
      .expect("legacy cache should serialize"),
    )
    .expect("legacy cache should be written");

    let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
    let mut migrating = CompilerDiagnosticsStore::load_with_cas(workspace.path(), Some(cas));
    assert!(
      migrating.get(&original.key).is_some(),
      "legacy evidence should remain warm"
    );
    migrating.flush().expect("legacy evidence should migrate");
    assert!(!path.exists(), "legacy input should disappear only after publication");

    let reopened =
      crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should reopen");
    let mut store = CompilerDiagnosticsStore::load_with_cas(workspace.path(), Some(reopened));
    assert_eq!(
      store.get(&original.key).map(|entry| &entry.evidence),
      Some(&original.evidence),
      "migrated evidence should remain reusable"
    );
  }

  #[test]
  fn corrupt_compiler_evidence_is_a_fail_closed_miss() {
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let original = entry("member", now_unix_ms(), 0);
    let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
    let mut writer = CompilerDiagnosticsStore::load_with_cas(workspace.path(), Some(cas));
    writer.put(original.clone());
    writer.flush().expect("compiler evidence should publish");

    let results = cache_root.path().join("cargo-rail/local-cas-v2/results");
    let bundle = fs::read_dir(results)
      .expect("results should be readable")
      .next()
      .expect("compiler evidence bundle should exist")
      .expect("result entry should be readable")
      .path();
    let evidence = fs::read_dir(bundle.join("evidence"))
      .expect("evidence directory should be readable")
      .next()
      .expect("evidence object should exist")
      .expect("evidence entry should be readable")
      .path();
    let mut bytes = fs::read(&evidence).expect("evidence should be readable");
    let index = bytes.iter().position(|byte| *byte == b'0').unwrap_or(0);
    bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };
    fs::write(evidence, bytes).expect("evidence should be corrupted");

    let reopened =
      crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should reopen");
    let mut reader = CompilerDiagnosticsStore::load_with_cas(workspace.path(), Some(reopened));
    assert!(reader.get(&original.key).is_none());
    assert_eq!(reader.miss_reason(&original.key), "local_cache_unreadable");
  }

  #[test]
  fn concurrent_equivalent_evidence_publications_converge() {
    let cache_root = tempfile::tempdir().expect("temporary cache should be created");
    let original = entry("member", now_unix_ms(), 0);
    let validation = CompilerEvidenceValidation::from_entry(&original).expect("validation should build");
    let evidence = CompilerEvidenceObject::from_entry(&original);
    std::thread::scope(|scope| {
      for _ in 0..2 {
        let cache_root = cache_root.path().to_path_buf();
        let validation = validation.clone();
        let evidence = evidence.clone();
        scope.spawn(move || {
          let cas =
            crate::cache::cas::LocalCas::open_at(&cache_root, 1024 * 1024).expect("concurrent local CAS should open");
          cas
            .store_compiler_evidence(crate::cache::cas::CompilerEvidenceStoreRequest {
              validation: &validation,
              evidence: &evidence,
            })
            .expect("equivalent publication should converge");
        });
      }
    });

    let root = cache_root.path().join("cargo-rail/local-cas-v2");
    assert_eq!(fs::read_dir(root.join("results")).expect("results").count(), 1);
    assert_eq!(fs::read_dir(root.join("pins")).expect("pins").count(), 1);
    let candidate_directories = fs::read_dir(root.join("compiler-evidence-candidates"))
      .expect("candidate index")
      .collect::<Result<Vec<_>, _>>()
      .expect("candidate directories should be readable");
    assert_eq!(candidate_directories.len(), 1);
    assert_eq!(
      fs::read_dir(candidate_directories[0].path())
        .expect("candidate actions")
        .count(),
      1
    );
    let reopened =
      crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should reopen for status");
    let status = reopened.status().expect("local CAS status should be readable");
    assert_eq!(status.results, 1);
    assert_eq!(status.pins, 1);
    assert_eq!(status.objects, 3);
    assert_eq!(status.active_leases, 0);
    assert_eq!(status.stale_leases, 0);
    assert_eq!(status.reclaimable_bytes, 0);
    assert!(status.bytes <= status.max_bytes);
  }

  #[test]
  fn oversized_cache_is_rejected_before_deserialization() {
    let root = tempfile::tempdir().expect("temporary workspace should be created");
    let path = root.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
    fs::create_dir_all(path.parent().expect("cache path should have a parent"))
      .expect("cache directory should be created");
    File::create(&path)
      .expect("oversized cache should be created")
      .set_len(MAX_CACHE_BYTES as u64 + 1)
      .expect("oversized cache length should be set");

    let store = CompilerDiagnosticsStore::load_legacy_only(root.path());
    assert_eq!(store.miss_reason(&key()), "cache_too_large");
  }

  #[cfg(unix)]
  #[test]
  fn linked_cache_files_are_rejected_without_following_them() {
    use std::os::unix::fs::symlink;

    for hard_link in [false, true] {
      let root = tempfile::tempdir().expect("temporary workspace should be created");
      let outside = tempfile::tempdir().expect("outside directory should be created");
      let path = root.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
      fs::create_dir_all(path.parent().expect("cache path should have a parent"))
        .expect("cache directory should be created");
      let outside_file = outside.path().join("compiler-diags-v1.json");
      fs::write(
        &outside_file,
        serde_json::to_vec(&CompilerDiagCacheFile::default()).expect("serialize cache"),
      )
      .expect("outside cache should be written");
      if hard_link {
        fs::hard_link(&outside_file, &path).expect("hard link should be created");
      } else {
        symlink(&outside_file, &path).expect("symlink should be created");
      }

      let store = CompilerDiagnosticsStore::load_legacy_only(root.path());
      assert_eq!(store.miss_reason(&key()), "cache_not_bounded_regular_file");
    }
  }
}
