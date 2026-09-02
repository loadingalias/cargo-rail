//! Exact, independently reusable compiler-fact objects in the selected profile's local CAS.
//!
//! One small set object proves completeness for a scheduled Cargo view. Each
//! compiler-owned fact object is stored and authenticated separately, so an
//! interrupted publication cannot turn a partial set into reuse authority.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::cache::cas::{CompilerEvidenceStoreRequest, LocalCas};
use crate::compiler::diagnostics_store::{
    CompilerEvidenceObject, CompilerFactCacheKey, CompilerFactEvidenceValidation, CompilerFactObjectReference,
};
use crate::compiler::facts::{CompilerFactObjectExpectation, ValidatedCompilerFactObject};
use crate::error::{RailError, RailResult};

const MAX_FACT_OBJECTS_PER_VIEW: usize = 1_000_000;

/// Optional profile-owned CAS authority for typed fact reuse.
pub(crate) struct CompilerFactStore {
    cas: Option<LocalCas>,
    remote: Option<Arc<crate::remote_cache::RemoteStore>>,
}

impl CompilerFactStore {
    pub(crate) fn load_with_remote(remote: Option<Arc<crate::remote_cache::RemoteStore>>) -> Self {
        Self {
            cas: LocalCas::open().ok(),
            remote,
        }
    }

    pub(crate) const fn durability_available(&self) -> bool {
        self.cas.is_some()
    }

    /// Load one complete exact set. Any missing or mismatched object is a miss.
    pub(crate) fn get(&self, key: &CompilerFactCacheKey) -> RailResult<Option<Vec<ValidatedCompilerFactObject>>> {
        let Some(cas) = &self.cas else {
            return Ok(None);
        };
        let candidate_key = CompilerFactEvidenceValidation::set_candidate_key(key)?;
        let mut candidates = cas.compiler_evidence_candidates(&candidate_key)?;
        if candidates.is_empty()
            && let Some(remote) = &self.remote
            && remote.import_compiler_evidence(cas, &candidate_key).is_ok()
        {
            candidates = cas.compiler_evidence_candidates(&candidate_key)?;
        }
        for candidate in candidates {
            let Some(validation) = candidate.validation.compiler_facts() else {
                continue;
            };
            let Some(references) = validation.set_objects(key) else {
                continue;
            };
            let Some(stored_set) = candidate
                .evidence
                .compiler_facts()
                .and_then(|evidence| evidence.fact_set())
            else {
                continue;
            };
            if references != stored_set || !set_covers_requested_packages(key, references) {
                continue;
            }
            let mut objects = Vec::with_capacity(references.len());
            let mut complete = true;
            for reference in references {
                match load_object(cas, self.remote.as_deref(), key, reference)? {
                    Some(object) => objects.push(object),
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete {
                objects.sort_by(|left, right| left.identity().cmp(right.identity()));
                return Ok(Some(objects));
            }
        }
        Ok(None)
    }

    /// Publish objects first and the completeness set last.
    pub(crate) fn put(&self, key: &CompilerFactCacheKey, objects: &[ValidatedCompilerFactObject]) -> RailResult<()> {
        let Some(cas) = &self.cas else {
            return Ok(());
        };
        if objects.len() > MAX_FACT_OBJECTS_PER_VIEW {
            return Err(RailError::message(
                "compiler fact cache set exceeds its object-count bound",
            ));
        }
        let mut objects_by_identity = BTreeMap::new();
        for object in objects {
            if object.object().producer_authority != *key.producer_authority()
                || !key.required_coverage().is_subset(&object.object().completion.coverage)
                || !key.typed_packages().contains(&object.object().unit.package.name)
            {
                return Err(RailError::message(
                    "compiler fact object is outside its cache producer, coverage, or package authority",
                ));
            }
            let reference = CompilerFactObjectReference::new(
                object.identity().to_string(),
                object.object().unit.identity.clone(),
                object.object().unit.package.name.clone(),
            )?;
            if objects_by_identity.insert(reference, object).is_some() {
                return Err(RailError::message(
                    "compiler fact cache set contains a duplicate object",
                ));
            }
        }
        let references = objects_by_identity.keys().cloned().collect::<Vec<_>>();
        if !set_covers_requested_packages(key, &references) {
            return Err(RailError::message(
                "compiler fact cache set is duplicate or incomplete for its requested packages",
            ));
        }

        let mut remote_complete = self.remote.is_some();
        for (reference, object) in &objects_by_identity {
            let published = store_object(cas, self.remote.as_deref(), key, object, reference)?;
            remote_complete &= published;
        }

        let validation = CompilerFactEvidenceValidation::set(key.clone(), references.clone())?;
        let evidence = CompilerEvidenceObject::from_compiler_fact_set(references)?;
        cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })?;
        if remote_complete && let Some(remote) = &self.remote {
            drop(remote.publish_compiler_evidence(&validation, &evidence));
        }
        Ok(())
    }
}

fn store_object(
    cas: &LocalCas,
    remote: Option<&crate::remote_cache::RemoteStore>,
    key: &CompilerFactCacheKey,
    object: &ValidatedCompilerFactObject,
    reference: &CompilerFactObjectReference,
) -> RailResult<bool> {
    let validation = CompilerFactEvidenceValidation::object(
        key.producer_authority().clone(),
        key.required_coverage().clone(),
        reference.clone(),
    )?;
    let evidence = CompilerEvidenceObject::from_compiler_fact_object(object.object().clone());
    cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
        validation: &validation,
        evidence: &evidence,
    })?;
    Ok(remote.is_some_and(|remote| remote.publish_compiler_evidence(&validation, &evidence).is_ok()))
}

fn load_object(
    cas: &LocalCas,
    remote: Option<&crate::remote_cache::RemoteStore>,
    key: &CompilerFactCacheKey,
    reference: &CompilerFactObjectReference,
) -> RailResult<Option<ValidatedCompilerFactObject>> {
    let expected_validation = CompilerFactEvidenceValidation::object(
        key.producer_authority().clone(),
        key.required_coverage().clone(),
        reference.clone(),
    )?;
    let mut candidates = cas.compiler_evidence_candidates(expected_validation.candidate_key())?;
    if candidates.is_empty()
        && let Some(remote) = remote
        && remote
            .import_compiler_evidence(cas, expected_validation.candidate_key())
            .is_ok()
    {
        candidates = cas.compiler_evidence_candidates(expected_validation.candidate_key())?;
    }
    for candidate in candidates {
        let Some(validation) = candidate.validation.compiler_facts() else {
            continue;
        };
        if !validation.matches_object(key.producer_authority(), key.required_coverage(), reference) {
            continue;
        }
        let Some(object) = candidate
            .evidence
            .compiler_facts()
            .and_then(|evidence| evidence.fact_object())
        else {
            continue;
        };
        let bytes = serde_json::to_vec(object)?;
        let expectation = CompilerFactObjectExpectation::new(
            key.producer_authority().clone(),
            reference.unit_identity.clone(),
            key.required_coverage().clone(),
        );
        let object = ValidatedCompilerFactObject::from_bytes(&bytes, &expectation)?;
        if object.identity() == reference.object_identity && object.object().unit.package.name == reference.package {
            return Ok(Some(object));
        }
    }
    Ok(None)
}

fn set_covers_requested_packages(key: &CompilerFactCacheKey, references: &[CompilerFactObjectReference]) -> bool {
    if references.len() > MAX_FACT_OBJECTS_PER_VIEW {
        return false;
    }
    if references.is_empty() {
        return true;
    }
    references
        .iter()
        .map(|reference| reference.package.clone())
        .collect::<BTreeSet<_>>()
        == *key.typed_packages()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cargo_metadata::PackageId;

    use super::*;
    use crate::compiler::facts::{
        COMPILER_IDENTITY_PREFIX, CompilerFactCompletion, CompilerFactCoverage, CompilerFactDomain, CompilerFactObject,
        CompilerFactPackage, CompilerFactProducerAuthority, CompilerFactRole, CompilerFactTargetKind, CompilerFactUnit,
        DRIVER_IDENTITY_PREFIX, INVOCATION_IDENTITY_PREFIX, ValidatedCompilerFactObject,
    };
    use crate::compiler::model::{CompilerDiagKey, FeatureSelection, PlatformTarget};

    #[test]
    fn complete_fact_set_reopens_from_independent_cas_objects() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        let (key, object) = fact_fixture();
        let identity = object.identity().to_string();

        store.put(&key, &[object]).expect("publish complete fact set");
        drop(store);

        let reopened = CompilerFactStore {
            cas: Some(LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("reopen local CAS")),
            remote: None,
        };
        let hit = reopened.get(&key).expect("fact lookup").expect("complete hit");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].identity(), identity);
    }

    #[test]
    fn empty_fact_set_is_a_complete_reusable_result() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        let (key, _) = fact_fixture();

        store.put(&key, &[]).expect("publish empty fact set");
        let hit = store.get(&key).expect("fact lookup").expect("complete empty hit");
        assert!(hit.is_empty());
    }

    #[test]
    fn set_manifest_without_its_object_is_a_cache_miss() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let (key, object) = fact_fixture();
        let reference = CompilerFactObjectReference::new(
            object.identity().to_string(),
            object.object().unit.identity.clone(),
            object.object().unit.package.name.clone(),
        )
        .expect("object reference");
        let validation =
            CompilerFactEvidenceValidation::set(key.clone(), vec![reference.clone()]).expect("set validation");
        let evidence = CompilerEvidenceObject::from_compiler_fact_set(vec![reference]).expect("set evidence");
        cas.store_compiler_evidence(CompilerEvidenceStoreRequest {
            validation: &validation,
            evidence: &evidence,
        })
        .expect("publish incomplete set fixture");

        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        assert!(store.get(&key).expect("fact lookup").is_none());
    }

    #[test]
    fn moved_workspace_root_reuses_the_same_exact_fact_set() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        let (key, object) = fact_fixture();
        store.put(&key, &[object]).expect("publish complete fact set");

        let mut package = package_fixture();
        package.package_id.repr = "path+file:///different/root#demo@1.0.0".to_string();
        let moved = fact_key(package, producer_fixture(), coverage_fixture(), '4');
        assert!(store.get(&moved).expect("moved-root lookup").is_some());
    }

    #[test]
    fn every_fact_set_authority_change_is_a_cache_miss() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        let (key, object) = fact_fixture();
        store.put(&key, &[object]).expect("publish complete fact set");

        let mut source = package_fixture();
        source.source_fingerprint = "sha256:changed-source".to_string();
        let mut toolchain = package_fixture();
        toolchain.toolchain_fingerprint = "sha256:changed-toolchain".to_string();
        let mut environment = package_fixture();
        environment.compiler_env_fingerprint = "sha256:changed-environment".to_string();
        let changed_producer = CompilerFactProducerAuthority {
            compiler_identity: identity(COMPILER_IDENTITY_PREFIX, '8'),
            driver_identity: identity(DRIVER_IDENTITY_PREFIX, '9'),
        };
        let changed_coverage = BTreeSet::from([CompilerFactCoverage::Definitions, CompilerFactCoverage::Visibility]);
        let misses = [
            fact_key(source, producer_fixture(), coverage_fixture(), '4'),
            fact_key(toolchain, producer_fixture(), coverage_fixture(), '4'),
            fact_key(environment, producer_fixture(), coverage_fixture(), '4'),
            fact_key(package_fixture(), changed_producer, coverage_fixture(), '4'),
            fact_key(package_fixture(), producer_fixture(), changed_coverage, '4'),
            fact_key(package_fixture(), producer_fixture(), coverage_fixture(), '7'),
        ];
        for changed in misses {
            assert!(store.get(&changed).expect("changed-authority lookup").is_none());
        }
    }

    #[test]
    fn corrupt_fact_object_cannot_authorize_a_hit() {
        let cache = tempfile::tempdir().expect("cache root");
        let cas = LocalCas::open_at(cache.path(), 16 * 1024 * 1024).expect("local CAS");
        let store = CompilerFactStore {
            cas: Some(cas),
            remote: None,
        };
        let (key, object) = fact_fixture();
        store.put(&key, &[object]).expect("publish complete fact set");

        let results = cache.path().join("cargo-rail/local-cas-v2/results");
        let evidence = fs::read_dir(results)
            .expect("results directory")
            .filter_map(Result::ok)
            .filter_map(|entry| fs::read_dir(entry.path().join("evidence")).ok())
            .flatten()
            .filter_map(Result::ok)
            .find(|entry| {
                fs::read(entry.path())
                    .is_ok_and(|bytes| bytes.windows(15).any(|window| window == b"\"kind\":\"object\""))
            })
            .expect("fact object evidence")
            .path();
        let mut bytes = fs::read(&evidence).expect("fact object bytes");
        let index = bytes.iter().position(|byte| *byte == b'0').unwrap_or(0);
        bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };
        fs::write(evidence, bytes).expect("corrupt fact object");

        assert!(store.get(&key).is_err(), "corrupt fact evidence authorized reuse");
    }

    fn fact_fixture() -> (CompilerFactCacheKey, ValidatedCompilerFactObject) {
        let producer = producer_fixture();
        let coverage = coverage_fixture();
        let unit = CompilerFactUnit {
            identity: String::new(),
            invocation_identity: identity(INVOCATION_IDENTITY_PREFIX, '3'),
            package: CompilerFactPackage {
                name: "demo".to_string(),
                version: "1.0.0".to_string(),
                source: None,
            },
            cargo_target: "demo".to_string(),
            crate_name: "demo".to_string(),
            target_kind: CompilerFactTargetKind::Library,
            domain: CompilerFactDomain::Production,
            role: CompilerFactRole::Target,
            platform: "x86_64-unknown-linux-gnu".to_string(),
            features: Vec::new(),
            cfg: Vec::new(),
        }
        .bind_identity()
        .expect("unit identity");
        let object = CompilerFactObject {
            version: crate::compiler::facts::COMPILER_FACT_PROTOCOL_VERSION,
            producer_authority: producer.clone(),
            unit: unit.clone(),
            strings: Vec::new(),
            sources: Vec::new(),
            items: Vec::new(),
            edges: Vec::new(),
            entry_points: Vec::new(),
            retentions: Vec::new(),
            completion: CompilerFactCompletion {
                complete: true,
                coverage: coverage.clone(),
                strings: 0,
                sources: 0,
                items: 0,
                edges: 0,
                entry_points: 0,
                retentions: 0,
            },
        };
        let bytes = serde_json::to_vec(&object).expect("fact object bytes");
        let expectation = CompilerFactObjectExpectation::new(producer.clone(), unit.identity, coverage.clone());
        let object = ValidatedCompilerFactObject::from_bytes(&bytes, &expectation).expect("validated fact object");
        let key = fact_key(package_fixture(), producer, coverage, '4');
        (key, object)
    }

    fn package_fixture() -> CompilerDiagKey {
        CompilerDiagKey {
            package_id: PackageId {
                repr: "path+file:///elsewhere#demo@1.0.0".to_string(),
            },
            package_name: "demo".to_string(),
            target: PlatformTarget::from("default"),
            features: FeatureSelection::Default,
            rustc_version: "rustc 1.95.0".to_string(),
            cargo_version: "cargo 1.95.0".to_string(),
            host_triple: "x86_64-unknown-linux-gnu".to_string(),
            toolchain_fingerprint: "sha256:toolchain".to_string(),
            target_fingerprint: "sha256:target".to_string(),
            lock_fingerprint: "sha256:lock".to_string(),
            manifest_fingerprint: "sha256:manifest".to_string(),
            source_fingerprint: "sha256:source".to_string(),
            compiler_env_fingerprint: "sha256:environment".to_string(),
            cargo_config_fingerprint: "sha256:config".to_string(),
        }
    }

    fn fact_key(
        package: CompilerDiagKey,
        producer: CompilerFactProducerAuthority,
        coverage: BTreeSet<CompilerFactCoverage>,
        view_digit: char,
    ) -> CompilerFactCacheKey {
        CompilerFactCacheKey::new(
            identity(crate::compiler::facts::VIEW_IDENTITY_PREFIX, view_digit),
            vec![package],
            BTreeSet::from(["demo".to_string()]),
            producer,
            coverage,
        )
        .expect("fact cache key")
    }

    fn producer_fixture() -> CompilerFactProducerAuthority {
        CompilerFactProducerAuthority {
            compiler_identity: identity(COMPILER_IDENTITY_PREFIX, '1'),
            driver_identity: identity(DRIVER_IDENTITY_PREFIX, '2'),
        }
    }

    fn coverage_fixture() -> BTreeSet<CompilerFactCoverage> {
        BTreeSet::from([CompilerFactCoverage::Definitions])
    }

    fn identity(prefix: &str, digit: char) -> String {
        format!("{prefix}{}", digit.to_string().repeat(64))
    }
}
