//! Separately versioned remote transport for compiler-evidence CAS objects.
//!
//! This protocol never grants reuse directly. It transports existing typed
//! validation/object pairs into the local CAS, where diagnostics, fact, and
//! native-binding stores apply their ordinary completeness rules.

use serde::{Deserialize, Serialize};

use super::object::{MAX_CONDITIONAL_ATTEMPTS, ObjectStore, PutCondition, PutOutcome, StoredBytes, TransferMetrics};
use super::{RemoteCacheSelection, RemoteProtocolMarkerState, RemoteStoreError, RemoteStoreResult};
use crate::cache::cas::{CompilerEvidenceStoreRequest, LocalCas};
use crate::compiler::diagnostics_store::{
    CompilerEvidenceObject, CompilerEvidenceValidation, validate_evidence_action_key, validate_evidence_candidate_key,
};

const OBJECT_NAMESPACE: &str = "evidence-v1";
const PROTOCOL_MARKER: &[u8] = b"cargo-rail-compiler-evidence-v1\n";
const RECORD_VERSION: u32 = 1;
const INDEX_VERSION: u32 = 1;
const MAX_RECORD_BYTES: u64 = 129 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceRecord {
    version: u32,
    candidate_key: String,
    action_key: String,
    validation: CompilerEvidenceValidation,
    evidence: CompilerEvidenceObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateIndex {
    version: u32,
    candidate_key: String,
    state: CandidateState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CandidateState {
    Unique { action_key: String },
    Conflict { first: String, second: String },
}

pub(super) struct EvidenceStore {
    transport: Box<dyn EvidenceTransport>,
}

trait EvidenceTransport: Send + Sync {
    fn metrics(&self) -> TransferMetrics;
    fn can_write(&self) -> bool;
    fn get(&self, suffix: &str, maximum: u64) -> RemoteStoreResult<Option<StoredBytes>>;
    fn put(&self, suffix: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome>;
}

impl EvidenceTransport for ObjectStore {
    fn metrics(&self) -> TransferMetrics {
        ObjectStore::metrics(self)
    }

    fn can_write(&self) -> bool {
        ObjectStore::can_write(self)
    }

    fn get(&self, suffix: &str, maximum: u64) -> RemoteStoreResult<Option<StoredBytes>> {
        self.get_namespaced(OBJECT_NAMESPACE, suffix, maximum)
    }

    fn put(&self, suffix: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
        self.put_namespaced(OBJECT_NAMESPACE, suffix, body, condition)
    }
}

impl EvidenceStore {
    pub(super) fn connect(selection: &RemoteCacheSelection) -> RemoteStoreResult<Self> {
        let store = Self {
            transport: Box::new(super::object::connect_transport(selection)?),
        };
        store.ensure_protocol_marker()?;
        Ok(store)
    }

    pub(super) fn metrics(&self) -> TransferMetrics {
        self.transport.metrics()
    }

    pub(super) fn import(&self, cas: &LocalCas, candidate_key: &str) -> RemoteStoreResult<usize> {
        validate_evidence_candidate_key(candidate_key)
            .map_err(|_| RemoteStoreError::integrity("remote evidence candidate identity is invalid"))?;
        let Some(index) = self.read_index(candidate_key)? else {
            return Ok(0);
        };
        let action_key = match index.state {
            CandidateState::Unique { action_key } => action_key,
            CandidateState::Conflict { .. } => {
                return Err(RemoteStoreError::integrity(
                    "remote evidence candidate is durably conflicted",
                ));
            }
        };
        let record = self
            .read_record(candidate_key, &action_key)?
            .ok_or_else(|| RemoteStoreError::integrity("remote evidence index references a missing object"))?;
        cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &record.validation,
            evidence: &record.evidence,
        })
        .map_err(|_| RemoteStoreError::integrity("remote evidence failed local CAS admission"))?;
        let admitted = cas
            .compiler_evidence_candidates(candidate_key)
            .map_err(|_| RemoteStoreError::integrity("imported evidence failed local CAS verification"))?
            .into_iter()
            .any(|candidate| candidate.validation == record.validation && candidate.evidence == record.evidence);
        if !admitted {
            return Err(RemoteStoreError::integrity(
                "imported evidence did not survive local CAS verification",
            ));
        }
        Ok(1)
    }

    pub(super) fn publish(
        &self,
        validation: &CompilerEvidenceValidation,
        evidence: &CompilerEvidenceObject,
    ) -> RemoteStoreResult<()> {
        if !self.transport.can_write() {
            return Err(RemoteStoreError::configuration(
                "selected remote cache mode does not permit evidence publication",
            ));
        }
        let record = EvidenceRecord {
            version: RECORD_VERSION,
            candidate_key: validation.candidate_key().to_string(),
            action_key: validation.action_key().to_string(),
            validation: validation.clone(),
            evidence: evidence.clone(),
        };
        validate_record(&record, validation.candidate_key(), validation.action_key())?;
        let bytes = encode_canonical(&record, MAX_RECORD_BYTES, "evidence record")?;
        let suffix = record_suffix(&record.candidate_key, &record.action_key)?;
        match self.transport.put(&suffix, &bytes, PutCondition::Absent)? {
            PutOutcome::Written => {}
            PutOutcome::PreconditionFailed => {
                let existing = self
                    .transport
                    .get(&suffix, MAX_RECORD_BYTES)?
                    .ok_or_else(|| RemoteStoreError::integrity("remote evidence object disappeared after conflict"))?;
                let existing =
                    decode_canonical::<EvidenceRecord>(&existing.bytes, MAX_RECORD_BYTES, "evidence record")?;
                validate_record(&existing, &record.candidate_key, &record.action_key)?;
                if existing != record {
                    return Err(RemoteStoreError::integrity(
                        "remote evidence action has conflicting immutable content",
                    ));
                }
            }
        }
        self.publish_index(&record.candidate_key, &record.action_key)
    }

    fn ensure_protocol_marker(&self) -> RemoteStoreResult<RemoteProtocolMarkerState> {
        match self.transport.get("protocol", PROTOCOL_MARKER.len() as u64)? {
            Some(marker) if marker.bytes == PROTOCOL_MARKER => return Ok(RemoteProtocolMarkerState::Existing),
            Some(_) => {
                return Err(RemoteStoreError::integrity(
                    "remote evidence protocol marker is incompatible",
                ));
            }
            None if !self.transport.can_write() => {
                return Err(RemoteStoreError::configuration(
                    "remote evidence protocol marker is unavailable",
                ));
            }
            None => {}
        }
        let _ = self.transport.put("protocol", PROTOCOL_MARKER, PutCondition::Absent)?;
        match self.transport.get("protocol", PROTOCOL_MARKER.len() as u64)? {
            Some(marker) if marker.bytes == PROTOCOL_MARKER => Ok(RemoteProtocolMarkerState::Initialized),
            _ => Err(RemoteStoreError::integrity(
                "remote evidence protocol marker did not converge",
            )),
        }
    }

    fn read_index(&self, candidate_key: &str) -> RemoteStoreResult<Option<CandidateIndex>> {
        let suffix = index_suffix(candidate_key)?;
        let Some(stored) = self.transport.get(&suffix, MAX_INDEX_BYTES)? else {
            return Ok(None);
        };
        let index = decode_canonical::<CandidateIndex>(&stored.bytes, MAX_INDEX_BYTES, "evidence candidate index")?;
        validate_index(&index, candidate_key)?;
        Ok(Some(index))
    }

    fn read_record(&self, candidate_key: &str, action_key: &str) -> RemoteStoreResult<Option<EvidenceRecord>> {
        let suffix = record_suffix(candidate_key, action_key)?;
        let Some(stored) = self.transport.get(&suffix, MAX_RECORD_BYTES)? else {
            return Ok(None);
        };
        let record = decode_canonical::<EvidenceRecord>(&stored.bytes, MAX_RECORD_BYTES, "evidence record")?;
        validate_record(&record, candidate_key, action_key)?;
        Ok(Some(record))
    }

    fn publish_index(&self, candidate_key: &str, action_key: &str) -> RemoteStoreResult<()> {
        let suffix = index_suffix(candidate_key)?;
        for _ in 0..MAX_CONDITIONAL_ATTEMPTS {
            let existing = self.transport.get(&suffix, MAX_INDEX_BYTES)?;
            let (mut index, condition) = match existing {
                Some(stored) => {
                    let index =
                        decode_canonical::<CandidateIndex>(&stored.bytes, MAX_INDEX_BYTES, "evidence candidate index")?;
                    validate_index(&index, candidate_key)?;
                    (index, PutCondition::Match(stored.etag))
                }
                None => (
                    CandidateIndex {
                        version: INDEX_VERSION,
                        candidate_key: candidate_key.to_string(),
                        state: CandidateState::Unique {
                            action_key: action_key.to_string(),
                        },
                    },
                    PutCondition::Absent,
                ),
            };
            if condition_matches_existing(&condition) {
                index.state = match index.state {
                    CandidateState::Unique { action_key: existing } if existing == action_key => return Ok(()),
                    CandidateState::Unique { action_key: existing } => {
                        let (first, second) = canonical_action_pair(existing, action_key.to_string())?;
                        CandidateState::Conflict { first, second }
                    }
                    CandidateState::Conflict { .. } => {
                        return Err(RemoteStoreError::integrity(
                            "remote evidence candidate is durably conflicted",
                        ));
                    }
                };
            }
            let bytes = encode_canonical(&index, MAX_INDEX_BYTES, "evidence candidate index")?;
            if matches!(self.transport.put(&suffix, &bytes, condition)?, PutOutcome::Written) {
                return match index.state {
                    CandidateState::Unique { .. } => Ok(()),
                    CandidateState::Conflict { .. } => Err(RemoteStoreError::integrity(
                        "remote evidence candidate became durably conflicted",
                    )),
                };
            }
        }
        Err(RemoteStoreError::unavailable(
            "remote evidence index publication remained contended",
        ))
    }
}

fn validate_record(record: &EvidenceRecord, candidate_key: &str, action_key: &str) -> RemoteStoreResult<()> {
    validate_evidence_candidate_key(candidate_key)
        .map_err(|_| RemoteStoreError::integrity("remote evidence candidate identity is invalid"))?;
    validate_evidence_action_key(action_key)
        .map_err(|_| RemoteStoreError::integrity("remote evidence action identity is invalid"))?;
    if record.version != RECORD_VERSION
        || record.candidate_key != candidate_key
        || record.action_key != action_key
        || record.validation.candidate_key() != candidate_key
        || record.validation.action_key() != action_key
    {
        return Err(RemoteStoreError::integrity(
            "remote evidence record does not match its candidate and action key",
        ));
    }
    record
        .validation
        .validate_object()
        .and_then(|()| record.evidence.identity().map(|_| ()))
        .map_err(|_| RemoteStoreError::integrity("remote evidence record failed typed validation"))?;
    Ok(())
}

fn validate_index(index: &CandidateIndex, candidate_key: &str) -> RemoteStoreResult<()> {
    validate_evidence_candidate_key(candidate_key)
        .map_err(|_| RemoteStoreError::integrity("remote evidence candidate identity is invalid"))?;
    if index.version != INDEX_VERSION || index.candidate_key != candidate_key {
        return Err(RemoteStoreError::integrity(
            "remote evidence candidate index is invalid",
        ));
    }
    match &index.state {
        CandidateState::Unique { action_key } => validate_evidence_action_key(action_key)
            .map_err(|_| RemoteStoreError::integrity("remote evidence candidate action is invalid")),
        CandidateState::Conflict { first, second } => {
            let canonical = canonical_action_pair(first.clone(), second.clone())?;
            if canonical != (first.clone(), second.clone()) {
                return Err(RemoteStoreError::integrity("remote evidence conflict is not canonical"));
            }
            Ok(())
        }
    }
}

fn condition_matches_existing(condition: &PutCondition) -> bool {
    matches!(condition, PutCondition::Match(_))
}

fn canonical_action_pair(first: String, second: String) -> RemoteStoreResult<(String, String)> {
    validate_evidence_action_key(&first)
        .and_then(|()| validate_evidence_action_key(&second))
        .map_err(|_| RemoteStoreError::integrity("remote evidence conflict action is invalid"))?;
    if first == second {
        return Err(RemoteStoreError::integrity(
            "remote evidence conflict repeats one action",
        ));
    }
    Ok(if first < second {
        (first, second)
    } else {
        (second, first)
    })
}

fn index_suffix(candidate_key: &str) -> RemoteStoreResult<String> {
    let digest = identity_digest(candidate_key, "remote evidence candidate identity")?;
    let shard = digest
        .get(..2)
        .ok_or_else(|| RemoteStoreError::integrity("remote evidence candidate identity has no shard"))?;
    Ok(format!("candidates/{shard}/{candidate_key}"))
}

fn record_suffix(candidate_key: &str, action_key: &str) -> RemoteStoreResult<String> {
    let candidate_digest = identity_digest(candidate_key, "remote evidence candidate identity")?;
    let _ = identity_digest(action_key, "remote evidence action identity")?;
    let shard = candidate_digest
        .get(..2)
        .ok_or_else(|| RemoteStoreError::integrity("remote evidence candidate identity has no shard"))?;
    Ok(format!("objects/{shard}/{candidate_key}/{action_key}"))
}

fn identity_digest<'a>(identity: &'a str, label: &str) -> RemoteStoreResult<&'a str> {
    identity
        .rsplit_once('-')
        .map(|(_, digest)| digest)
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| RemoteStoreError::integrity(format!("{label} has no canonical digest")))
}

fn encode_canonical<T: Serialize>(value: &T, maximum: u64, label: &str) -> RemoteStoreResult<Vec<u8>> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| RemoteStoreError::integrity(format!("remote {label} encoding failed")))?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} exceeds its byte bound"
        )));
    }
    Ok(bytes)
}

fn decode_canonical<T>(bytes: &[u8], maximum: u64, label: &str) -> RemoteStoreResult<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} exceeds its byte bound"
        )));
    }
    let value = serde_json::from_slice::<T>(bytes)
        .map_err(|_| RemoteStoreError::integrity(format!("remote {label} is malformed")))?;
    if encode_canonical(&value, maximum, label)? != bytes {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} is not canonically encoded"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    use crate::compiler::analysis::{AnalysisContract, AnalysisEvidenceStore, NativeEvidenceBinding};
    use crate::compiler::diagnostics_store::NativeEvidenceBindingValidation;
    use crate::compiler::model::FeatureSelection;
    use crate::compiler::observation::{
        CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, RawCompilerInvocation,
    };
    use crate::compiler::scheduler::CompilerFactFamily;

    #[derive(Default)]
    struct MemoryTransport {
        state: Mutex<MemoryState>,
    }

    #[derive(Default)]
    struct MemoryState {
        generation: u64,
        objects: BTreeMap<String, (Vec<u8>, String)>,
    }

    impl EvidenceTransport for MemoryTransport {
        fn metrics(&self) -> TransferMetrics {
            TransferMetrics::default()
        }

        fn can_write(&self) -> bool {
            true
        }

        fn get(&self, suffix: &str, maximum: u64) -> RemoteStoreResult<Option<StoredBytes>> {
            let state = self
                .state
                .lock()
                .map_err(|_| RemoteStoreError::unavailable("memory transport lock failed"))?;
            let Some((bytes, etag)) = state.objects.get(suffix) else {
                return Ok(None);
            };
            if bytes.len() as u64 > maximum {
                return Err(RemoteStoreError::integrity(
                    "memory transport object exceeds its byte bound",
                ));
            }
            Ok(Some(StoredBytes {
                bytes: bytes.clone(),
                etag: etag.clone(),
            }))
        }

        fn put(&self, suffix: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RemoteStoreError::unavailable("memory transport lock failed"))?;
            let permitted = match (&condition, state.objects.get(suffix)) {
                (PutCondition::Absent, None) => true,
                (PutCondition::Absent, Some(_)) | (PutCondition::Match(_), None) => false,
                (PutCondition::Match(expected), Some((_, actual))) => expected == actual,
            };
            if !permitted {
                return Ok(PutOutcome::PreconditionFailed);
            }
            state.generation = state.generation.saturating_add(1);
            let etag = format!("memory-{}", state.generation);
            state.objects.insert(suffix.to_string(), (body.to_vec(), etag));
            Ok(PutOutcome::Written)
        }
    }

    fn binding_record() -> EvidenceRecord {
        let contract = AnalysisContract::new(
            BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
            "demo".to_string(),
            "default".to_string(),
            FeatureSelection::Default,
            "cargo-check-all-targets".to_string(),
            "configuration".to_string(),
            None,
            BTreeSet::new(),
        )
        .expect("contract");
        let binding = NativeEvidenceBinding::new(
            format!("{}{:064x}", crate::compiler::native_cache::ACTION_KEY_PREFIX, 1),
            format!("{}{:064x}", crate::compiler::native_cache::RESULT_KEY_PREFIX, 2),
            &contract,
            format!(
                "{}{:064x}",
                crate::compiler::diagnostics_store::EVIDENCE_OBJECT_PREFIX,
                3
            ),
            Vec::new(),
        )
        .expect("binding");
        let validation = NativeEvidenceBindingValidation::from_binding(binding.clone()).expect("validation");
        EvidenceRecord {
            version: RECORD_VERSION,
            candidate_key: validation.candidate_key().to_string(),
            action_key: validation.action_key().to_string(),
            validation,
            evidence: CompilerEvidenceObject::from_native_binding(binding),
        }
    }

    fn observation(action: &str, result: &str) -> RawCompilerInvocation {
        RawCompilerInvocation {
            version: crate::compiler::observation::COMPILATION_OBSERVATION_VERSION,
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
    fn evidence_v1_is_independent_from_native_v6() {
        assert_eq!(OBJECT_NAMESPACE, "evidence-v1");
        assert_eq!(PROTOCOL_MARKER, b"cargo-rail-compiler-evidence-v1\n");
        assert_ne!(OBJECT_NAMESPACE, super::super::object::OBJECT_NAMESPACE);
    }

    #[test]
    fn canonical_record_binds_candidate_action_validation_and_object() {
        let record = binding_record();
        let bytes = encode_canonical(&record, MAX_RECORD_BYTES, "evidence record").expect("encode");
        let decoded = decode_canonical::<EvidenceRecord>(&bytes, MAX_RECORD_BYTES, "evidence record").expect("decode");
        validate_record(&decoded, &record.candidate_key, &record.action_key).expect("record");
        assert_eq!(decoded, record);
    }

    #[test]
    fn index_conflicts_are_canonical_and_terminal() {
        let record = binding_record();
        let mut index = CandidateIndex {
            version: INDEX_VERSION,
            candidate_key: record.candidate_key.clone(),
            state: CandidateState::Unique {
                action_key: record.action_key.clone(),
            },
        };
        validate_index(&index, &record.candidate_key).expect("index");
        let other = format!(
            "{}{:064x}",
            crate::compiler::diagnostics_store::EVIDENCE_ACTION_KEY_PREFIX,
            9
        );
        let (first, second) = canonical_action_pair(other, record.action_key).expect("canonical conflict");
        index.state = CandidateState::Conflict {
            first: second,
            second: first,
        };
        validate_index(&index, &record.candidate_key).expect_err("noncanonical conflict");
    }

    #[test]
    fn binding_record_cannot_be_rekeyed() {
        let record = binding_record();
        let other = format!(
            "{}{:064x}",
            crate::compiler::diagnostics_store::EVIDENCE_ACTION_KEY_PREFIX,
            9
        );
        validate_record(&record, &record.candidate_key, &other).expect_err("rekeyed binding");
    }

    #[test]
    fn remote_graph_import_requires_every_object_then_reuses_through_the_owning_store() {
        let source = tempfile::tempdir().expect("source CAS");
        let source_cas = LocalCas::open_at(source.path(), 16 * 1024 * 1024).expect("source CAS");
        let contract = AnalysisContract::new(
            BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
            "demo".to_string(),
            "default".to_string(),
            FeatureSelection::Default,
            "cargo-check-all-targets".to_string(),
            "configuration".to_string(),
            None,
            BTreeSet::new(),
        )
        .expect("contract");
        let action = format!("{}{:064x}", crate::compiler::native_cache::ACTION_KEY_PREFIX, 1);
        let result = format!("{}{:064x}", crate::compiler::native_cache::RESULT_KEY_PREFIX, 2);
        AnalysisEvidenceStore::from_cas(source_cas.clone())
            .put(
                &contract,
                action.clone(),
                result.clone(),
                &observation(&action, &result),
                &[],
            )
            .expect("source graph");

        let binding_candidate = NativeEvidenceBindingValidation::candidate_key(
            &action,
            &result,
            &contract.identity().expect("contract identity"),
        );
        let binding = source_cas
            .compiler_evidence_candidates(&binding_candidate)
            .expect("binding candidates")
            .into_iter()
            .next()
            .expect("binding");
        let observation_identity = binding
            .evidence
            .native_binding()
            .expect("binding payload")
            .observation()
            .to_string();
        let observation_validation =
            crate::compiler::diagnostics_store::CompilerObservationEvidenceValidation::from_object_identity(
                observation_identity,
            )
            .expect("observation validation");
        let observation = source_cas
            .compiler_evidence_candidates(observation_validation.candidate_key())
            .expect("observation candidates")
            .into_iter()
            .next()
            .expect("observation");

        let remote = EvidenceStore {
            transport: Box::new(MemoryTransport::default()),
        };
        remote
            .publish(&observation.validation, &observation.evidence)
            .expect("publish observation");
        remote
            .publish(&binding.validation, &binding.evidence)
            .expect("publish binding");

        let destination = tempfile::tempdir().expect("destination CAS");
        let destination_cas = LocalCas::open_at(destination.path(), 16 * 1024 * 1024).expect("destination CAS");
        remote
            .import(&destination_cas, &binding_candidate)
            .expect("import binding");
        let destination_store = AnalysisEvidenceStore::from_cas(destination_cas.clone());
        assert!(
            destination_store
                .get(&contract, &action, &result)
                .expect("incomplete lookup")
                .is_none(),
            "a binding without its referenced observation granted reuse"
        );

        remote
            .import(&destination_cas, observation_validation.candidate_key())
            .expect("import observation");
        assert!(
            destination_store
                .get(&contract, &action, &result)
                .expect("complete lookup")
                .is_some(),
            "the owning analysis store rejected a fully imported remote graph"
        );
    }
}
