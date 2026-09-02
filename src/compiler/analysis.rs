//! Stable analysis authority and native-result evidence bindings.
//!
//! The analysis contract contains only reusable semantic authority. Physical
//! paths and one-shot run capabilities remain in [`super::session`]. Native
//! action and result identities remain owned by the native cache; this module
//! only binds independently validated compiler evidence to an exact pair.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::cas::{CompilerEvidenceStoreRequest, LocalCas};
use crate::compiler::diagnostics_store::{
    CompilerEvidenceObject, CompilerFactEvidenceValidation, CompilerFactObjectReference,
    CompilerObservationEvidenceValidation, NativeEvidenceBindingValidation,
};
use crate::compiler::facts::{
    CompilerFactCoverage, CompilerFactObjectExpectation, CompilerFactProducerAuthority, ValidatedCompilerFactObject,
};
use crate::compiler::model::FeatureSelection;
use crate::compiler::scheduler::CompilerFactFamily;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const ANALYSIS_CONTRACT_VERSION: u32 = 1;
const NATIVE_EVIDENCE_BINDING_VERSION: u32 = 1;
const FACT_IMPORT_VERSION: u32 = 1;
const MAX_FACT_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const ANALYSIS_CONTRACT_ID_PREFIX: &str = "analysis-contract-v1-sha256-";
const ANALYSIS_OBSERVATION_VERSION: u32 = 1;

/// Root-independent evidence requirements for one exact Cargo analysis view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalysisContract {
    version: u32,
    families: BTreeSet<CompilerFactFamily>,
    package: String,
    platform: String,
    features: FeatureSelection,
    variant: String,
    configuration_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    producer: Option<CompilerFactProducerAuthority>,
    required_coverage: BTreeSet<CompilerFactCoverage>,
}

impl AnalysisContract {
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor makes every stable analysis authority explicit"
    )]
    pub(crate) fn new(
        families: BTreeSet<CompilerFactFamily>,
        package: String,
        platform: String,
        features: FeatureSelection,
        variant: String,
        configuration_identity: String,
        producer: Option<CompilerFactProducerAuthority>,
        required_coverage: BTreeSet<CompilerFactCoverage>,
    ) -> RailResult<Self> {
        let contract = Self {
            version: ANALYSIS_CONTRACT_VERSION,
            families,
            package,
            platform,
            features,
            variant,
            configuration_identity,
            producer,
            required_coverage,
        };
        contract.validate()?;
        Ok(contract)
    }

    pub(crate) fn identity(&self) -> RailResult<String> {
        self.validate()?;
        Ok(format!(
            "{ANALYSIS_CONTRACT_ID_PREFIX}{}",
            ContentDigest::sha256(&serde_json::to_vec(self)?)
        ))
    }

    pub(crate) fn families(&self) -> &BTreeSet<CompilerFactFamily> {
        &self.families
    }

    pub(crate) fn producer(&self) -> Option<&CompilerFactProducerAuthority> {
        self.producer.as_ref()
    }

    pub(crate) fn required_coverage(&self) -> &BTreeSet<CompilerFactCoverage> {
        &self.required_coverage
    }

    pub(crate) fn requires_typed_facts(&self) -> bool {
        self.families.contains(&CompilerFactFamily::TypedRustItems)
    }

    pub(crate) fn validate(&self) -> RailResult<()> {
        if self.version != ANALYSIS_CONTRACT_VERSION
            || self.families.is_empty()
            || self.package.is_empty()
            || self.platform.is_empty()
            || self.variant.is_empty()
            || self.configuration_identity.is_empty()
            || [
                &self.package,
                &self.platform,
                &self.variant,
                &self.configuration_identity,
            ]
            .into_iter()
            .any(|value| value.len() > 4096 || value.contains(['\0', '\n', '\r']))
        {
            return Err(RailError::message("analysis contract authority is incomplete"));
        }
        let typed = self.requires_typed_facts();
        if typed != self.producer.is_some() || typed != !self.required_coverage.is_empty() {
            return Err(RailError::message(
                "analysis contract typed producer and coverage authority is inconsistent",
            ));
        }
        if let Some(producer) = &self.producer {
            producer.validate()?;
        }
        Ok(())
    }
}

/// Root-independent analysis evidence layered over one exact native result.
///
/// Native action and result validation already own arguments, declared inputs,
/// dependency artifacts, selected reads, and emitted outputs. Those fields are
/// late-bound to the current Cargo sandbox during restore and therefore cannot
/// enter this reusable object. The remaining fields carry the compiler and
/// analysis facts that the native result alone does not authorize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalysisObservation {
    version: u32,
    observation: crate::compiler::observation::RawCompilerInvocation,
}

impl AnalysisObservation {
    pub(crate) fn capture(observation: &crate::compiler::observation::RawCompilerInvocation) -> RailResult<Self> {
        if !observation.success {
            return Err(RailError::message(
                "analysis observation is not a complete successful compiler invocation",
            ));
        }
        let mut observation = observation.clone();
        observation.compiler_arguments.clear();
        observation.declared_inputs.clear();
        observation.observed_reads.clear();
        observation.dependency_artifacts.clear();
        observation.emitted_outputs.clear();
        observation.cache_wrapper = None;
        let projected = Self {
            version: ANALYSIS_OBSERVATION_VERSION,
            observation,
        };
        projected.validate()?;
        Ok(projected)
    }

    /// Build the only successful portable observation a distributed worker is
    /// allowed to return for this already captured invocation.
    pub(crate) fn distributed_success(
        observation: &crate::compiler::observation::RawCompilerInvocation,
    ) -> RailResult<Self> {
        let mut successful = observation.clone();
        successful.success = true;
        Self::capture(&successful)
    }

    pub(crate) fn canonical_bytes(&self) -> RailResult<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).map_err(Into::into)
    }

    /// Decode canonical worker bytes and bind them to the exact local
    /// pre-execution observation. An echoed or fabricated alternative cannot
    /// change any analysis-owned field.
    pub(crate) fn from_distributed_bytes(
        bytes: &[u8],
        current: &crate::compiler::observation::RawCompilerInvocation,
    ) -> RailResult<Self> {
        let observation: Self = serde_json::from_slice(bytes)?;
        observation.validate()?;
        if serde_json::to_vec(&observation)? != bytes || observation != Self::distributed_success(current)? {
            return Err(RailError::message(
                "distributed analysis observation does not match the exact local invocation",
            ));
        }
        Ok(observation)
    }

    pub(crate) fn restore_into(
        &self,
        current: &crate::compiler::observation::RawCompilerInvocation,
    ) -> RailResult<crate::compiler::observation::RawCompilerInvocation> {
        self.validate()?;
        let stored = &self.observation;
        if stored.version != current.version
            || stored.mode != current.mode
            || stored.crate_name != current.crate_name
            || stored.crate_types != current.crate_types
            || stored.target_argument != current.target_argument
            || stored.cfg != current.cfg
            || stored.emit_modes != current.emit_modes
            || stored.test_mode != current.test_mode
            || stored.compiler_fact_unit != current.compiler_fact_unit
        {
            return Err(RailError::message(
                "analysis observation does not match the current compiler invocation",
            ));
        }
        let mut restored = stored.clone();
        restored.compiler_arguments = current.compiler_arguments.clone();
        restored.declared_inputs = current.declared_inputs.clone();
        restored.observed_reads = current.observed_reads.clone();
        restored.dependency_artifacts = current.dependency_artifacts.clone();
        restored.emitted_outputs = current.emitted_outputs.clone();
        restored.cache_wrapper = current.cache_wrapper.clone();
        Ok(restored)
    }

    pub(crate) fn compiler_fact_unit(&self) -> Option<&crate::compiler::facts::CompilerFactUnit> {
        self.observation.compiler_fact_unit.as_ref()
    }

    pub(crate) fn validate(&self) -> RailResult<()> {
        if self.version != ANALYSIS_OBSERVATION_VERSION
            || !self.observation.success
            || !self.observation.compiler_arguments.is_empty()
            || !self.observation.declared_inputs.is_empty()
            || !self.observation.observed_reads.is_empty()
            || !self.observation.dependency_artifacts.is_empty()
            || !self.observation.emitted_outputs.is_empty()
            || self.observation.cache_wrapper.is_some()
        {
            return Err(RailError::message(
                "analysis observation contains incomplete or native-result-owned state",
            ));
        }
        Ok(())
    }
}

/// Immutable compiler evidence attached to one exact native cache result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeEvidenceBinding {
    version: u32,
    native_action: String,
    native_result: String,
    contract: String,
    observation: String,
    facts: Vec<CompilerFactObjectReference>,
}

impl NativeEvidenceBinding {
    pub(crate) fn new(
        native_action: String,
        native_result: String,
        contract: &AnalysisContract,
        observation: String,
        mut facts: Vec<CompilerFactObjectReference>,
    ) -> RailResult<Self> {
        facts.sort();
        let binding = Self {
            version: NATIVE_EVIDENCE_BINDING_VERSION,
            native_action,
            native_result,
            contract: contract.identity()?,
            observation,
            facts,
        };
        binding.validate(contract)?;
        Ok(binding)
    }

    pub(crate) fn native_action(&self) -> &str {
        &self.native_action
    }

    pub(crate) fn native_result(&self) -> &str {
        &self.native_result
    }

    pub(crate) fn contract(&self) -> &str {
        &self.contract
    }

    pub(crate) fn observation(&self) -> &str {
        &self.observation
    }

    pub(crate) fn facts(&self) -> &[CompilerFactObjectReference] {
        &self.facts
    }

    pub(crate) fn validate(&self, contract: &AnalysisContract) -> RailResult<()> {
        if self.version != NATIVE_EVIDENCE_BINDING_VERSION
            || !valid_identity(&self.native_action, crate::compiler::native_cache::ACTION_KEY_PREFIX)
            || !valid_identity(&self.native_result, crate::compiler::native_cache::RESULT_KEY_PREFIX)
            || self.contract != contract.identity()?
            || !valid_identity(
                &self.observation,
                crate::compiler::diagnostics_store::EVIDENCE_OBJECT_PREFIX,
            )
            || self.facts.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(RailError::message(
                "native evidence binding does not match its action, result, contract, or evidence",
            ));
        }
        Ok(())
    }
}

fn valid_identity(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Fully validated evidence that may satisfy one native analysis invocation.
pub(crate) struct ResolvedNativeEvidence {
    pub(crate) observation: AnalysisObservation,
    pub(crate) facts: Vec<ValidatedCompilerFactObject>,
}

/// Compiler-evidence view over the existing local CAS lifecycle.
pub(crate) struct AnalysisEvidenceStore {
    cas: LocalCas,
}

impl AnalysisEvidenceStore {
    pub(crate) fn from_cas(cas: LocalCas) -> Self {
        Self { cas }
    }

    /// Publish independently reusable evidence first, then the immutable binding.
    pub(crate) fn put(
        &self,
        contract: &AnalysisContract,
        native_action: String,
        native_result: String,
        observation: &crate::compiler::observation::RawCompilerInvocation,
        facts: &[ValidatedCompilerFactObject],
    ) -> RailResult<()> {
        self.put_inner(contract, native_action, native_result, observation, facts, None)
    }

    pub(crate) fn put_with_remote(
        &self,
        contract: &AnalysisContract,
        native_action: String,
        native_result: String,
        observation: &crate::compiler::observation::RawCompilerInvocation,
        facts: &[ValidatedCompilerFactObject],
        remote: &crate::remote_cache::RemoteStore,
    ) -> RailResult<()> {
        self.put_inner(contract, native_action, native_result, observation, facts, Some(remote))
    }

    fn put_inner(
        &self,
        contract: &AnalysisContract,
        native_action: String,
        native_result: String,
        observation: &crate::compiler::observation::RawCompilerInvocation,
        facts: &[ValidatedCompilerFactObject],
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<()> {
        contract.validate()?;
        let cache = observation.cache_wrapper.as_ref();
        if !observation.success
            || cache.and_then(crate::compiler::observation::CompilerCacheWrapperMetadata::action_key)
                != Some(native_action.as_str())
            || cache.and_then(crate::compiler::observation::CompilerCacheWrapperMetadata::result_key)
                != Some(native_result.as_str())
        {
            return Err(RailError::message(
                "compiler observation does not bind the native action and result being published",
            ));
        }
        let observation = AnalysisObservation::capture(observation)?;
        self.put_resolved_inner(contract, native_action, native_result, &observation, facts, remote)
    }

    /// Publish evidence already validated at the distributed client boundary.
    /// Native action/result admission remains separate and must precede reuse.
    pub(crate) fn put_resolved(
        &self,
        contract: &AnalysisContract,
        native_action: String,
        native_result: String,
        observation: &AnalysisObservation,
        facts: &[ValidatedCompilerFactObject],
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<()> {
        self.put_resolved_inner(contract, native_action, native_result, observation, facts, remote)
    }

    fn put_resolved_inner(
        &self,
        contract: &AnalysisContract,
        native_action: String,
        native_result: String,
        observation: &AnalysisObservation,
        facts: &[ValidatedCompilerFactObject],
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<()> {
        contract.validate()?;
        observation.validate()?;
        if !binding_shape_matches(contract, observation, facts) {
            return Err(RailError::message(
                "resolved analysis evidence does not match its contract or compilation unit",
            ));
        }
        let observation_object = CompilerEvidenceObject::from_observation(observation.clone());
        let observation_identity = observation_object.identity()?;
        let observation_validation =
            CompilerObservationEvidenceValidation::from_object_identity(observation_identity.clone())?;
        self.cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &observation_validation,
            evidence: &observation_object,
        })?;
        let mut remote_complete = remote.is_some_and(|remote| {
            remote
                .publish_compiler_evidence(&observation_validation, &observation_object)
                .is_ok()
        });

        let mut references = Vec::with_capacity(facts.len());
        for fact in facts {
            let producer = contract.producer().ok_or_else(|| {
                RailError::message("native evidence binding has facts without typed producer authority")
            })?;
            if &fact.object().producer_authority != producer
                || !contract
                    .required_coverage()
                    .is_subset(&fact.object().completion.coverage)
            {
                return Err(RailError::message(
                    "native evidence fact is outside its analysis producer or coverage authority",
                ));
            }
            let reference = CompilerFactObjectReference::new(
                fact.identity().to_string(),
                fact.object().unit.identity.clone(),
                fact.object().unit.package.name.clone(),
            )?;
            let validation = CompilerFactEvidenceValidation::object(
                producer.clone(),
                contract.required_coverage().clone(),
                reference.clone(),
            )?;
            let evidence = CompilerEvidenceObject::from_compiler_fact_object(fact.object().clone());
            self.cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
                validation: &validation,
                evidence: &evidence,
            })?;
            if remote_complete {
                remote_complete =
                    remote.is_some_and(|remote| remote.publish_compiler_evidence(&validation, &evidence).is_ok());
            }
            references.push(reference);
        }

        let binding =
            NativeEvidenceBinding::new(native_action, native_result, contract, observation_identity, references)?;
        let validation = NativeEvidenceBindingValidation::from_binding(binding.clone())?;
        let evidence = CompilerEvidenceObject::from_native_binding(binding);
        self.cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })?;
        if remote_complete && let Some(remote) = remote {
            drop(remote.publish_compiler_evidence(&validation, &evidence));
        }
        Ok(())
    }

    /// Resolve a binding only after every referenced object validates independently.
    pub(crate) fn get(
        &self,
        contract: &AnalysisContract,
        native_action: &str,
        native_result: &str,
    ) -> RailResult<Option<ResolvedNativeEvidence>> {
        self.get_inner(contract, native_action, native_result, None)
    }

    pub(crate) fn get_with_remote(
        &self,
        contract: &AnalysisContract,
        native_action: &str,
        native_result: &str,
        remote: &crate::remote_cache::RemoteStore,
    ) -> RailResult<Option<ResolvedNativeEvidence>> {
        self.get_inner(contract, native_action, native_result, Some(remote))
    }

    fn get_inner(
        &self,
        contract: &AnalysisContract,
        native_action: &str,
        native_result: &str,
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<Option<ResolvedNativeEvidence>> {
        contract.validate()?;
        let contract_identity = contract.identity()?;
        let candidate_key =
            NativeEvidenceBindingValidation::candidate_key(native_action, native_result, &contract_identity);
        let mut resolved = None;
        let mut candidates = self.cas.compiler_evidence_candidates(&candidate_key)?;
        if candidates.is_empty()
            && let Some(remote) = remote
            && remote.import_compiler_evidence(&self.cas, &candidate_key).is_ok()
        {
            candidates = self.cas.compiler_evidence_candidates(&candidate_key)?;
        }
        for candidate in candidates {
            let Some(validation) = candidate.validation.native_binding() else {
                continue;
            };
            let Some(binding) = candidate.evidence.native_binding() else {
                continue;
            };
            if validation.binding() != binding
                || binding.native_action() != native_action
                || binding.native_result() != native_result
                || binding.validate(contract).is_err()
            {
                continue;
            }
            let Some(observation) = self.load_observation(binding.observation(), remote)? else {
                continue;
            };
            let Some(facts) = self.load_facts(contract, binding.facts(), remote)? else {
                continue;
            };
            if !binding_shape_matches(contract, &observation, &facts) {
                continue;
            }
            if resolved.is_some() {
                // One exact triplet has one immutable attachment. Multiple
                // independently valid payloads are conflicting authority, not
                // alternatives from which lookup may choose by recency.
                return Ok(None);
            }
            resolved = Some(ResolvedNativeEvidence { observation, facts });
        }
        Ok(resolved)
    }

    fn load_observation(
        &self,
        identity: &str,
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<Option<AnalysisObservation>> {
        let expected = CompilerObservationEvidenceValidation::from_object_identity(identity.to_string())?;
        let mut candidates = self.cas.compiler_evidence_candidates(expected.candidate_key())?;
        if candidates.is_empty()
            && let Some(remote) = remote
            && remote
                .import_compiler_evidence(&self.cas, expected.candidate_key())
                .is_ok()
        {
            candidates = self.cas.compiler_evidence_candidates(expected.candidate_key())?;
        }
        for candidate in candidates {
            let Some(validation) = candidate.validation.observation() else {
                continue;
            };
            let Some(observation) = candidate.evidence.observation_evidence() else {
                continue;
            };
            if validation.object_identity() == identity
                && candidate.evidence.identity()? == identity
                && observation.validate().is_ok()
            {
                return Ok(Some(observation.clone()));
            }
        }
        Ok(None)
    }

    fn load_facts(
        &self,
        contract: &AnalysisContract,
        references: &[CompilerFactObjectReference],
        remote: Option<&crate::remote_cache::RemoteStore>,
    ) -> RailResult<Option<Vec<ValidatedCompilerFactObject>>> {
        if references.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let Some(producer) = contract.producer() else {
            return Ok(None);
        };
        let mut facts = Vec::with_capacity(references.len());
        for reference in references {
            let expected = CompilerFactEvidenceValidation::object(
                producer.clone(),
                contract.required_coverage().clone(),
                reference.clone(),
            )?;
            let mut found = None;
            let mut candidates = self.cas.compiler_evidence_candidates(expected.candidate_key())?;
            if candidates.is_empty()
                && let Some(remote) = remote
                && remote
                    .import_compiler_evidence(&self.cas, expected.candidate_key())
                    .is_ok()
            {
                candidates = self.cas.compiler_evidence_candidates(expected.candidate_key())?;
            }
            for candidate in candidates {
                let Some(validation) = candidate.validation.compiler_facts() else {
                    continue;
                };
                let Some(object) = candidate
                    .evidence
                    .compiler_facts()
                    .and_then(|evidence| evidence.fact_object())
                else {
                    continue;
                };
                if !validation.matches_object(producer, contract.required_coverage(), reference) {
                    continue;
                }
                let bytes = serde_json::to_vec(object)?;
                let expectation = CompilerFactObjectExpectation::new(
                    producer.clone(),
                    reference.unit_identity.clone(),
                    contract.required_coverage().clone(),
                );
                let object = ValidatedCompilerFactObject::from_bytes(&bytes, &expectation)?;
                if object.identity() == reference.object_identity
                    && object.object().unit.package.name == reference.package
                {
                    found = Some(object);
                    break;
                }
            }
            let Some(found) = found else {
                return Ok(None);
            };
            facts.push(found);
        }
        Ok(Some(facts))
    }
}

fn binding_shape_matches(
    contract: &AnalysisContract,
    observation: &AnalysisObservation,
    facts: &[ValidatedCompilerFactObject],
) -> bool {
    if !contract.requires_typed_facts() {
        return facts.is_empty();
    }
    match observation.compiler_fact_unit() {
        Some(unit) => {
            facts.len() == 1
                && facts[0].object().unit.identity == unit.identity
                && facts[0].object().unit.package == unit.package
        }
        None => facts.is_empty(),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerFactImport {
    version: u32,
    contract: String,
    object: crate::compiler::facts::CompilerFactObject,
}

/// Materialize validated reusable objects into one command-local collector session.
pub(crate) fn publish_fact_imports(
    directory: &Path,
    contract: &AnalysisContract,
    facts: &[ValidatedCompilerFactObject],
) -> RailResult<()> {
    for fact in facts {
        let import = CompilerFactImport {
            version: FACT_IMPORT_VERSION,
            contract: contract.identity()?,
            object: fact.object().clone(),
        };
        let bytes = serde_json::to_vec(&import)?;
        let identity = ContentDigest::sha256(&bytes);
        let destination = directory.join(format!("compiler-fact-import-sha256-{identity}.json"));
        let mut temporary = tempfile::Builder::new()
            .prefix(".cargo-rail-fact-import-")
            .suffix(".tmp")
            .tempfile_in(directory)?;
        temporary.write_all(&bytes)?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&destination)? != bytes {
                    return Err(RailError::message("compiler fact import content identity collided"));
                }
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(())
}

/// Load command-local imports and bind them to the current session and observed units.
pub(crate) fn load_fact_imports(
    directory: &Path,
    contract: &AnalysisContract,
    invocations: &[crate::compiler::observation::RawCompilerInvocation],
) -> RailResult<Vec<ValidatedCompilerFactObject>> {
    let expected_units = invocations
        .iter()
        .filter_map(|invocation| invocation.compiler_fact_unit.as_ref())
        .map(|unit| (unit.identity.clone(), unit.package.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut paths = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("compiler-fact-import-sha256-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut objects = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || metadata.len() > MAX_FACT_IMPORT_BYTES
        {
            return Err(RailError::message("compiler fact import is not a bounded regular file"));
        }
        let bytes = fs::read(&path)?;
        let import: CompilerFactImport = serde_json::from_slice(&bytes)?;
        if import.version != FACT_IMPORT_VERSION
            || import.contract != contract.identity()?
            || serde_json::to_vec(&import)? != bytes
        {
            return Err(RailError::message(
                "compiler fact import does not match the current analysis contract",
            ));
        }
        let Some(unit) = expected_units.get(&import.object.unit.identity) else {
            return Err(RailError::message(
                "compiler fact import names an unobserved compilation unit",
            ));
        };
        if unit != &import.object.unit.package {
            return Err(RailError::message(
                "compiler fact import package does not match its observed unit",
            ));
        }
        let producer = contract
            .producer()
            .ok_or_else(|| RailError::message("compiler fact import has no typed producer authority"))?;
        let expectation = CompilerFactObjectExpectation::new(
            producer.clone(),
            import.object.unit.identity.clone(),
            contract.required_coverage().clone(),
        );
        objects.push(ValidatedCompilerFactObject::from_bytes(
            &serde_json::to_vec(&import.object)?,
            &expectation,
        )?);
    }
    objects.sort_by(|left, right| left.identity().cmp(right.identity()));
    if objects.windows(2).any(|pair| pair[0].identity() == pair[1].identity()) {
        return Err(RailError::message("compiler fact imports contain a duplicate object"));
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::observation::{
        CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, RawCompilerInvocation,
    };
    use crate::compiler::scheduler::CompilerFactFamily;

    fn contract() -> AnalysisContract {
        AnalysisContract::new(
            BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
            "demo".to_string(),
            "default".to_string(),
            FeatureSelection::Default,
            "cargo-check-all-targets".to_string(),
            format!("sha256:{}", "a".repeat(64)),
            None,
            BTreeSet::new(),
        )
        .expect("analysis contract")
    }

    fn native_identity(prefix: &str, byte: char) -> String {
        format!("{prefix}{}", byte.to_string().repeat(64))
    }

    fn observation(action: &str, result: &str) -> RawCompilerInvocation {
        RawCompilerInvocation {
            version: 6,
            mode: CompilerMode::Rustc,
            crate_name: Some("demo".to_string()),
            crate_types: BTreeSet::from(["lib".to_string()]),
            target_argument: None,
            cfg: BTreeSet::new(),
            emit_modes: BTreeSet::from(["metadata".to_string()]),
            test_mode: false,
            compiler_arguments: vec!["src/lib.rs".to_string()],
            declared_inputs: Vec::new(),
            observed_reads: Vec::new(),
            dependency_artifacts: Vec::new(),
            emitted_outputs: Vec::new(),
            environment_reads: BTreeSet::new(),
            compiler: None,
            wrappers: Vec::new(),
            cache_wrapper: Some(CompilerCacheWrapperMetadata::native(
                CompilerCacheWrapperStatus::Miss,
                "stored_verified_result",
                Some(action.to_string()),
                Some(result.to_string()),
                0,
                0,
            )),
            compiler_exit_code: Some(0),
            success: true,
            bypasses: BTreeSet::new(),
            compiler_fact_unit: None,
        }
    }

    #[test]
    fn contract_identity_excludes_ephemeral_session_state() {
        let first = contract();
        let second = contract();
        assert_eq!(
            first.identity().expect("first identity"),
            second.identity().expect("second identity")
        );
    }

    #[test]
    fn typed_contract_requires_both_producer_and_coverage() {
        let error = AnalysisContract::new(
            BTreeSet::from([CompilerFactFamily::TypedRustItems]),
            "demo".to_string(),
            "default".to_string(),
            FeatureSelection::Default,
            "cargo-check-all-targets".to_string(),
            "configuration".to_string(),
            None,
            BTreeSet::new(),
        )
        .expect_err("incomplete typed contract");
        assert!(error.to_string().contains("producer and coverage"));
    }

    #[test]
    fn complete_native_binding_reopens_its_independent_observation() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas);
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        let observation = observation(&action, &result);

        assert!(
            store
                .get(&contract, &action, &result)
                .expect("initial lookup")
                .is_none()
        );
        store
            .put(&contract, action.clone(), result.clone(), &observation, &[])
            .expect("publish binding");
        let reopened = store
            .get(&contract, &action, &result)
            .expect("bound lookup")
            .expect("complete binding");
        assert_eq!(
            reopened
                .observation
                .restore_into(&observation)
                .expect("restored observation"),
            observation
        );
        assert!(reopened.facts.is_empty());
    }

    #[test]
    fn physical_sandbox_paths_do_not_multiply_exact_binding_candidates() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas.clone());
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        let mut first = observation(&action, &result);
        first.compiler_arguments = vec![
            "--out-dir".to_string(),
            "repository:/target/compiler-artifacts/generation-one/build/debug/deps".to_string(),
        ];
        let mut second = first.clone();
        second.compiler_arguments[1] =
            "repository:/target/compiler-artifacts/generation-two/build/debug/deps".to_string();

        store
            .put(&contract, action.clone(), result.clone(), &first, &[])
            .expect("first binding");
        store
            .put(&contract, action.clone(), result.clone(), &second, &[])
            .expect("second binding");

        let candidate_key = NativeEvidenceBindingValidation::candidate_key(
            &action,
            &result,
            &contract.identity().expect("contract identity"),
        );
        assert_eq!(
            cas.compiler_evidence_candidates(&candidate_key)
                .expect("binding candidates")
                .len(),
            1,
            "one semantic binding must have one immutable CAS candidate"
        );
    }

    #[test]
    fn conflicting_complete_bindings_are_a_fail_closed_miss() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas);
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        let first = observation(&action, &result);
        let mut conflicting = first.clone();
        conflicting.crate_name = Some("conflicting-demo".to_string());

        store
            .put(&contract, action.clone(), result.clone(), &first, &[])
            .expect("first binding");
        store
            .put(&contract, action.clone(), result.clone(), &conflicting, &[])
            .expect("conflicting binding");

        assert!(
            store
                .get(&contract, &action, &result)
                .expect("conflicting lookup")
                .is_none(),
            "lookup must not select one of two conflicting exact bindings"
        );
    }

    #[test]
    fn action_result_and_contract_mismatches_are_clean_misses() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas);
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        store
            .put(
                &contract,
                action.clone(),
                result.clone(),
                &observation(&action, &result),
                &[],
            )
            .expect("publish binding");

        let other_action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'c');
        let other_result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'd');
        assert!(
            store
                .get(&contract, &other_action, &result)
                .expect("action miss")
                .is_none()
        );
        assert!(
            store
                .get(&contract, &action, &other_result)
                .expect("result miss")
                .is_none()
        );
        let other_contract = AnalysisContract::new(
            BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
            "demo".to_string(),
            "other-target".to_string(),
            FeatureSelection::Default,
            "cargo-check-all-targets".to_string(),
            format!("sha256:{}", "a".repeat(64)),
            None,
            BTreeSet::new(),
        )
        .expect("other contract");
        assert!(
            store
                .get(&other_contract, &action, &result)
                .expect("contract miss")
                .is_none()
        );
    }

    #[test]
    fn binding_without_its_observation_object_is_a_miss() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas.clone());
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        let missing_observation = native_identity(crate::compiler::diagnostics_store::EVIDENCE_OBJECT_PREFIX, 'e');
        let binding = NativeEvidenceBinding::new(
            action.clone(),
            result.clone(),
            &contract,
            missing_observation,
            Vec::new(),
        )
        .expect("partial binding");
        let validation = NativeEvidenceBindingValidation::from_binding(binding.clone()).expect("binding validation");
        let evidence = CompilerEvidenceObject::from_native_binding(binding);
        cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })
        .expect("publish partial binding");

        assert!(
            store
                .get(&contract, &action, &result)
                .expect("partial lookup")
                .is_none()
        );
    }

    #[test]
    fn mismatched_binding_validation_and_payload_is_a_miss() {
        let root = tempfile::tempdir().expect("CAS root");
        let cas = LocalCas::open_at(root.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = AnalysisEvidenceStore::from_cas(cas.clone());
        let contract = contract();
        let action = native_identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'a');
        let result = native_identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, 'b');
        let first = NativeEvidenceBinding::new(
            action.clone(),
            result.clone(),
            &contract,
            native_identity(crate::compiler::diagnostics_store::EVIDENCE_OBJECT_PREFIX, 'c'),
            Vec::new(),
        )
        .expect("first binding");
        let second = NativeEvidenceBinding::new(
            action.clone(),
            result.clone(),
            &contract,
            native_identity(crate::compiler::diagnostics_store::EVIDENCE_OBJECT_PREFIX, 'd'),
            Vec::new(),
        )
        .expect("second binding");
        let validation = NativeEvidenceBindingValidation::from_binding(first).expect("first validation");
        let evidence = CompilerEvidenceObject::from_native_binding(second);
        cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })
        .expect("publish mismatched fixture");

        assert!(
            store
                .get(&contract, &action, &result)
                .expect("mismatched lookup")
                .is_none()
        );
    }
}
