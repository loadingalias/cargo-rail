//! Persistent store for compiler diagnostics cache entries.

use crate::compiler::analysis::NativeEvidenceBinding;
use crate::compiler::facts::{
    CompilerFactCoverage, CompilerFactObject, CompilerFactProducerAuthority, FRAGMENT_OBJECT_IDENTITY_PREFIX,
};
use crate::compiler::model::{
    COLLECTOR_VERSION, CompilerDiagEntry, CompilerDiagKey, DiagnosticsCompleteness, TargetEvidence,
};
use crate::error::{RailError, RailResult};
use rscrypto::Sha256;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

pub(crate) const EVIDENCE_CANDIDATE_KEY_PREFIX: &str = "compiler-evidence-candidate-v1-sha256-";
pub(crate) const EVIDENCE_ACTION_KEY_PREFIX: &str = "compiler-evidence-action-v1-sha256-";
pub(crate) const EVIDENCE_OBJECT_PREFIX: &str = "compiler-evidence-v1-sha256-";
const EVIDENCE_RESULT_PREFIX: &str = "compiler-evidence-result-v1-sha256-";
const EVIDENCE_VALIDATION_VERSION: u32 = 1;
const EVIDENCE_OBJECT_VERSION: u32 = 1;
const FACT_EVIDENCE_VALIDATION_VERSION: u32 = 1;
const FACT_EVIDENCE_OBJECT_VERSION: u32 = 1;
const OBSERVATION_EVIDENCE_VALIDATION_VERSION: u32 = 1;
const OBSERVATION_EVIDENCE_OBJECT_VERSION: u32 = 1;
const NATIVE_BINDING_VALIDATION_VERSION: u32 = 1;
const NATIVE_BINDING_OBJECT_VERSION: u32 = 1;

/// Fully observed authority for one compiler-evidence result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum CompilerEvidenceValidation {
    Diagnostics(CompilerDiagnosticsEvidenceValidation),
    CompilerFacts(CompilerFactEvidenceValidation),
    Observation(CompilerObservationEvidenceValidation),
    NativeBinding(NativeEvidenceBindingValidation),
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

/// CAS authority for one root-independent raw compiler observation object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerObservationEvidenceValidation {
    observation_validation_version: u32,
    action_key: String,
    candidate_key: String,
    object_identity: String,
}

/// CAS authority for one native action/result and analysis-contract binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeEvidenceBindingValidation {
    binding_validation_version: u32,
    action_key: String,
    candidate_key: String,
    binding: NativeEvidenceBinding,
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
            Self::Observation(validation) => &validation.action_key,
            Self::NativeBinding(validation) => &validation.action_key,
        }
    }

    pub(crate) fn candidate_key(&self) -> &str {
        match self {
            Self::Diagnostics(validation) => &validation.candidate_key,
            Self::CompilerFacts(validation) => &validation.candidate_key,
            Self::Observation(validation) => &validation.candidate_key,
            Self::NativeBinding(validation) => &validation.candidate_key,
        }
    }

    fn diagnostics(&self) -> Option<&CompilerDiagnosticsEvidenceValidation> {
        match self {
            Self::Diagnostics(validation) => Some(validation),
            Self::CompilerFacts(_) | Self::Observation(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn compiler_facts(&self) -> Option<&CompilerFactEvidenceValidation> {
        match self {
            Self::Diagnostics(_) => None,
            Self::CompilerFacts(validation) => Some(validation),
            Self::Observation(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn observation(&self) -> Option<&CompilerObservationEvidenceValidation> {
        match self {
            Self::Observation(validation) => Some(validation),
            Self::Diagnostics(_) | Self::CompilerFacts(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn native_binding(&self) -> Option<&NativeEvidenceBindingValidation> {
        match self {
            Self::NativeBinding(validation) => Some(validation),
            Self::Diagnostics(_) | Self::CompilerFacts(_) | Self::Observation(_) => None,
        }
    }

    pub(crate) fn validate_object(&self) -> RailResult<()> {
        match self {
            Self::Diagnostics(validation) => validation.validate_object(),
            Self::CompilerFacts(validation) => validation.validate_object(),
            Self::Observation(validation) => validation.validate_object(),
            Self::NativeBinding(validation) => validation.validate_object(),
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

impl CompilerObservationEvidenceValidation {
    pub(crate) fn from_object_identity(object_identity: String) -> RailResult<CompilerEvidenceValidation> {
        validate_evidence_object(&object_identity)?;
        let candidate_key = framed_sha256(
            EVIDENCE_CANDIDATE_KEY_PREFIX,
            b"cargo-rail-compiler-observation-candidate-v1\0",
            &[(b"object", object_identity.as_bytes())],
        );
        let action_key = framed_sha256(
            EVIDENCE_ACTION_KEY_PREFIX,
            b"cargo-rail-compiler-observation-action-v1\0",
            &[(b"candidate", candidate_key.as_bytes())],
        );
        Ok(CompilerEvidenceValidation::Observation(Self {
            observation_validation_version: OBSERVATION_EVIDENCE_VALIDATION_VERSION,
            action_key,
            candidate_key,
            object_identity,
        }))
    }

    pub(crate) fn object_identity(&self) -> &str {
        &self.object_identity
    }

    fn validate_object(&self) -> RailResult<()> {
        if self.observation_validation_version != OBSERVATION_EVIDENCE_VALIDATION_VERSION {
            return Err(RailError::message(
                "compiler observation evidence validation has an incompatible schema",
            ));
        }
        let rebound = Self::from_object_identity(self.object_identity.clone())?;
        if rebound.action_key() != self.action_key || rebound.candidate_key() != self.candidate_key {
            return Err(RailError::message(
                "compiler observation evidence validation identity does not match its object",
            ));
        }
        Ok(())
    }
}

impl NativeEvidenceBindingValidation {
    pub(crate) fn from_binding(binding: NativeEvidenceBinding) -> RailResult<CompilerEvidenceValidation> {
        let candidate_key =
            native_binding_candidate_key(binding.native_action(), binding.native_result(), binding.contract());
        let action_key = framed_sha256(
            EVIDENCE_ACTION_KEY_PREFIX,
            b"cargo-rail-native-evidence-binding-action-v1\0",
            &[
                (b"candidate", candidate_key.as_bytes()),
                (b"binding", &serde_json::to_vec(&binding)?),
            ],
        );
        Ok(CompilerEvidenceValidation::NativeBinding(Self {
            binding_validation_version: NATIVE_BINDING_VALIDATION_VERSION,
            action_key,
            candidate_key,
            binding,
        }))
    }

    pub(crate) fn candidate_key(native_action: &str, native_result: &str, contract: &str) -> String {
        native_binding_candidate_key(native_action, native_result, contract)
    }

    pub(crate) fn binding(&self) -> &NativeEvidenceBinding {
        &self.binding
    }

    fn validate_object(&self) -> RailResult<()> {
        if self.binding_validation_version != NATIVE_BINDING_VALIDATION_VERSION {
            return Err(RailError::message(
                "native evidence binding validation has an incompatible schema",
            ));
        }
        let rebound = Self::from_binding(self.binding.clone())?;
        if rebound.action_key() != self.action_key || rebound.candidate_key() != self.candidate_key {
            return Err(RailError::message(
                "native evidence binding validation identity does not match its binding",
            ));
        }
        Ok(())
    }
}

fn native_binding_candidate_key(native_action: &str, native_result: &str, contract: &str) -> String {
    framed_sha256(
        EVIDENCE_CANDIDATE_KEY_PREFIX,
        b"cargo-rail-native-evidence-binding-candidate-v1\0",
        &[
            (b"native-action", native_action.as_bytes()),
            (b"native-result", native_result.as_bytes()),
            (b"contract", contract.as_bytes()),
        ],
    )
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
    Diagnostics(Box<CompilerDiagnosticsEvidenceObject>),
    CompilerFacts(CompilerFactEvidenceObject),
    Observation(Box<CompilerObservationEvidenceObject>),
    NativeBinding(NativeEvidenceBindingObject),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerObservationEvidenceObject {
    observation_object_version: u32,
    observation: crate::compiler::analysis::AnalysisObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeEvidenceBindingObject {
    binding_object_version: u32,
    binding: NativeEvidenceBinding,
}

impl CompilerEvidenceObject {
    pub(crate) fn from_entry(entry: &CompilerDiagEntry) -> Self {
        Self::Diagnostics(Box::new(CompilerDiagnosticsEvidenceObject {
            version: EVIDENCE_OBJECT_VERSION,
            evidence: entry.evidence.clone(),
        }))
    }

    pub(crate) fn diagnostics_evidence(&self) -> Option<&TargetEvidence> {
        match self {
            Self::Diagnostics(object) => Some(&object.evidence),
            Self::CompilerFacts(_) | Self::Observation(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn compiler_facts(&self) -> Option<&CompilerFactEvidenceObject> {
        match self {
            Self::Diagnostics(_) => None,
            Self::CompilerFacts(object) => Some(object),
            Self::Observation(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn from_observation(observation: crate::compiler::analysis::AnalysisObservation) -> Self {
        Self::Observation(Box::new(CompilerObservationEvidenceObject {
            observation_object_version: OBSERVATION_EVIDENCE_OBJECT_VERSION,
            observation,
        }))
    }

    pub(crate) fn observation_evidence(&self) -> Option<&crate::compiler::analysis::AnalysisObservation> {
        match self {
            Self::Observation(object) => Some(&object.observation),
            Self::Diagnostics(_) | Self::CompilerFacts(_) | Self::NativeBinding(_) => None,
        }
    }

    pub(crate) fn from_native_binding(binding: NativeEvidenceBinding) -> Self {
        Self::NativeBinding(NativeEvidenceBindingObject {
            binding_object_version: NATIVE_BINDING_OBJECT_VERSION,
            binding,
        })
    }

    pub(crate) fn native_binding(&self) -> Option<&NativeEvidenceBinding> {
        match self {
            Self::NativeBinding(object) => Some(&object.binding),
            Self::Diagnostics(_) | Self::CompilerFacts(_) | Self::Observation(_) => None,
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
            Self::Observation(object) if object.observation_object_version != OBSERVATION_EVIDENCE_OBJECT_VERSION => {
                return Err(RailError::message(
                    "compiler observation evidence object has an incompatible schema",
                ));
            }
            Self::Observation(object) => object.observation.validate()?,
            Self::NativeBinding(object) if object.binding_object_version != NATIVE_BINDING_OBJECT_VERSION => {
                return Err(RailError::message(
                    "native evidence binding object has an incompatible schema",
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
        key.package_id
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
        hasher.update(&(tag.len() as u64).to_le_bytes());
        hasher.update(tag);
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    crate::instrumentation::record_hash_operation();
    format!(
        "{prefix}{}",
        crate::source::ContentDigest::from_sha256_bytes(hasher.finalize())
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
#[derive(Debug)]
pub struct CompilerDiagnosticsStore {
    entries: HashMap<String, CompilerDiagEntry>,
    cas: Option<crate::cache::cas::LocalCas>,
    remote: Option<Arc<crate::remote_cache::RemoteStore>>,
    pending: HashMap<String, CompilerDiagEntry>,
    prior_by_configuration: HashMap<String, (u64, CompilerDiagKey, u32)>,
    cached_packages: HashSet<String>,
    discarded_reason: Option<&'static str>,
}

impl CompilerDiagnosticsStore {
    /// Load compiler evidence from the selected profile's local CAS.
    pub fn load() -> Self {
        let cas = crate::cache::cas::LocalCas::open().ok();
        Self::load_with_cas_and_remote(cas, None)
    }

    pub(crate) fn load_with_remote(remote: Option<Arc<crate::remote_cache::RemoteStore>>) -> Self {
        let cas = crate::cache::cas::LocalCas::open().ok();
        Self::load_with_cas_and_remote(cas, remote)
    }

    pub(crate) const fn durability_available(&self) -> bool {
        self.cas.is_some()
    }

    #[cfg(test)]
    fn load_with_cas(cas: Option<crate::cache::cas::LocalCas>) -> Self {
        Self::load_with_cas_and_remote(cas, None)
    }

    fn load_with_cas_and_remote(
        cas: Option<crate::cache::cas::LocalCas>,
        remote: Option<Arc<crate::remote_cache::RemoteStore>>,
    ) -> Self {
        let discarded_reason = cas.is_none().then_some("local_cache_unavailable");
        Self {
            entries: HashMap::new(),
            cas,
            remote,
            pending: HashMap::new(),
            prior_by_configuration: HashMap::new(),
            cached_packages: HashSet::new(),
            discarded_reason,
        }
    }

    /// Return cached entry for the exact key.
    pub fn get(&mut self, key: &CompilerDiagKey) -> Option<&CompilerDiagEntry> {
        let id = key.stable_id();
        let memory_hit = self
            .pending
            .get(&id)
            .or_else(|| self.entries.get(&id))
            .is_some_and(|entry| {
                entry.collector_version == COLLECTOR_VERSION
                    && entry.evidence.completeness == DiagnosticsCompleteness::Complete
                    && semantic_key_bytes(&entry.key)
                        .is_ok_and(|stored| semantic_key_bytes(key).is_ok_and(|current| stored == current))
            });
        if memory_hit {
            return self.pending.get(&id).or_else(|| self.entries.get(&id));
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
            .map(|cas| {
                let candidates = cas.compiler_evidence_candidates(&candidate_key)?;
                if candidates.is_empty()
                    && let Some(remote) = &self.remote
                {
                    match remote.import_compiler_evidence(cas, &candidate_key) {
                        Ok(_) => return cas.compiler_evidence_candidates(&candidate_key),
                        Err(error) => self.discarded_reason = Some(error.cold_reason()),
                    }
                }
                Ok(candidates)
            })
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
            self.entries.insert(id.clone(), entry);
        }
        self.entries.get(&id)
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
        self.pending.insert(id, entry);
    }

    /// Persist dirty cache state to disk.
    pub fn flush(&mut self) -> RailResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let Some(cas) = &self.cas else {
            self.entries.extend(self.pending.drain());
            return Ok(());
        };
        for entry in self.pending.values() {
            let validation = CompilerEvidenceValidation::from_entry(entry)?;
            let evidence = CompilerEvidenceObject::from_entry(entry);
            cas.store_compiler_evidence(crate::cache::cas::CompilerEvidenceStoreRequest {
                validation: &validation,
                evidence: &evidence,
            })?;
            if let Some(remote) = &self.remote {
                drop(remote.publish_compiler_evidence(&validation, &evidence));
            }
        }
        self.entries.extend(self.pending.drain());
        Ok(())
    }

    fn record_prior(&mut self, generated_at_unix_ms: u64, key: &CompilerDiagKey, collector_version: u32) {
        self.cached_packages.insert(logical_package_identity(key).to_string());
        let id = configuration_id(key);
        let replace = self
            .prior_by_configuration
            .get(&id)
            .is_none_or(|(generated, _, _)| *generated < generated_at_unix_ms);
        if replace {
            self.prior_by_configuration
                .insert(id, (generated_at_unix_ms, key.clone(), collector_version));
        }
    }
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
    use std::collections::BTreeSet;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

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

    #[test]
    fn prior_collector_semantics_never_authorize_reuse() {
        let cache_root = tempfile::tempdir().expect("temporary cache should be created");
        let key = key();
        let mut prior = entry("member", now_unix_ms(), 0);
        prior.collector_version = COLLECTOR_VERSION - 1;
        let validation = CompilerEvidenceValidation::from_entry(&prior).expect("validation should build");
        let evidence = CompilerEvidenceObject::from_entry(&prior);
        let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
        cas.store_compiler_evidence(crate::cache::cas::CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })
        .expect("prior evidence should publish");
        let mut store = CompilerDiagnosticsStore::load_with_cas(Some(cas));
        assert!(
            store.get(&key).is_none(),
            "prior collector evidence must not be returned"
        );
        assert_eq!(store.miss_reason(&key), "collector_changed");
    }

    #[test]
    fn compiler_evidence_round_trips_across_equivalent_package_roots() {
        let cache_root = tempfile::tempdir().expect("temporary cache should be created");
        let first_cas =
            crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
        let original = entry("member", now_unix_ms(), 0);
        let mut first = CompilerDiagnosticsStore::load_with_cas(Some(first_cas));
        first.put(original.clone());
        first.flush().expect("compiler evidence should publish");

        let second_cas =
            crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should reopen");
        let mut equivalent_key = original.key.clone();
        equivalent_key.package_id.repr = "path+file:///different/root#member@0.1.0".to_string();
        let mut second = CompilerDiagnosticsStore::load_with_cas(Some(second_cas));
        let reused = second
            .get(&equivalent_key)
            .expect("equivalent root should reuse evidence");

        assert_eq!(reused.key, equivalent_key);
        assert_eq!(reused.evidence, original.evidence);
        assert_eq!(reused.observations, original.observations);
    }

    #[test]
    fn incomplete_evidence_is_never_published_or_reused() {
        let cache_root = tempfile::tempdir().expect("temporary cache should be created");
        let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
        let mut incomplete = entry("member", now_unix_ms(), 0);
        incomplete.evidence.completeness = DiagnosticsCompleteness::Incomplete;
        let key = incomplete.key.clone();

        let mut store = CompilerDiagnosticsStore::load_with_cas(Some(cas));
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
    fn corrupt_compiler_evidence_is_a_fail_closed_miss() {
        let cache_root = tempfile::tempdir().expect("temporary cache should be created");
        let original = entry("member", now_unix_ms(), 0);
        let cas = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024).expect("local CAS should open");
        let mut writer = CompilerDiagnosticsStore::load_with_cas(Some(cas));
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
        let mut reader = CompilerDiagnosticsStore::load_with_cas(Some(reopened));
        assert!(reader.get(&original.key).is_none());
        assert_eq!(reader.miss_reason(&original.key), "local_cache_unreadable");
    }

    #[test]
    fn pending_evidence_remains_reusable_without_a_local_cas() {
        let original = entry("member", now_unix_ms(), 0);
        let mut store = CompilerDiagnosticsStore::load_with_cas(None);

        store.put(original.clone());
        assert!(store.get(&original.key).is_some());

        store.flush().expect("memory-only evidence should flush");
        assert!(store.get(&original.key).is_some());
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
                    let cas = crate::cache::cas::LocalCas::open_at(&cache_root, 1024 * 1024)
                        .expect("concurrent local CAS should open");
                    cas.store_compiler_evidence(crate::cache::cas::CompilerEvidenceStoreRequest {
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
        let reopened = crate::cache::cas::LocalCas::open_at(cache_root.path(), 1024 * 1024)
            .expect("local CAS should reopen for status");
        let status = reopened.status().expect("local CAS status should be readable");
        assert_eq!(status.results, 1);
        assert_eq!(status.pins, 1);
        assert_eq!(status.objects, 3);
        assert_eq!(status.active_leases, 0);
        assert_eq!(status.stale_leases, 0);
        assert_eq!(status.reclaimable_bytes, 0);
        assert!(status.bytes <= status.max_bytes);
    }
}
