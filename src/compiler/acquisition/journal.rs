//! Versioned, crash-safe progress for concurrent compiler acquisition.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::compiler::diagnostics_store::CompilerFactCacheKey;
use crate::compiler::model::{CompilerDiagKey, PlatformTarget};
use crate::compiler::scheduler::{CompilerAcquisitionPlan, CompilerAcquisitionView, ViewIx};
use crate::error::{RailError, RailResult};
use crate::progress;
use crate::source::ContentDigest;

const ACQUISITION_CONTRACT_V1: u32 = 1;
const ACQUISITION_CONTRACT_V2: u32 = 2;
const ACQUISITION_IDENTITY_PREFIX: &str = "surface-acquisition-v2-sha256-";
const EVIDENCE_IDENTITY_PREFIX: &str = "surface-acquisition-evidence-v1-sha256-";
const V1_DIRECTORY: &str = "surface-acquisitions-v1";
const V2_DIRECTORY: &str = "surface-acquisitions-v2";
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JOURNAL_GROUPS: usize = 256;
const MAX_V1_RECORDS_PER_VIEW: usize = 4;
const COMPLETION_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// One Surface product whose policy depends on compiler acquisition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CompilerAcquisitionProduct {
    pub(crate) package: String,
    pub(crate) cargo_target: String,
    pub(crate) kind: String,
}

/// Stable inputs needed to bind and resume one Surface acquisition.
#[derive(Debug, Clone)]
pub(crate) struct CompilerAcquisitionRequest {
    pub(crate) workspace_identity: String,
    pub(crate) checkout_identity: String,
    pub(crate) snapshot_identity: String,
    pub(crate) configuration_fingerprint: String,
    pub(crate) products: Vec<CompilerAcquisitionProduct>,
    pub(crate) resume_manifest: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerAcquisitionCargoTarget {
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct EvidenceIdentity(String);

impl EvidenceIdentity {
    fn new(bytes: &[u8]) -> Self {
        Self(format!("{EVIDENCE_IDENTITY_PREFIX}{}", ContentDigest::sha256(bytes)))
    }

    fn validate(&self) -> RailResult<()> {
        validate_sha256_identity(&self.0, EVIDENCE_IDENTITY_PREFIX, "acquisition evidence")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FailureClass {
    Worker,
    Integration,
    Coordinator,
    Journal,
    Sandbox,
    Broker,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum DurableViewState {
    Pending,
    Running { attempt: u32 },
    Complete { evidence: EvidenceIdentity },
    Failed { class: FailureClass },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableViewRecord {
    /// Read-only compatibility for v2 journals written while the canonical
    /// array position was redundantly serialized. New journals omit it; view
    /// identity and `view_index` are the only durable lookup authority.
    #[serde(rename = "ordinal", default, skip_serializing_if = "Option::is_none")]
    legacy_ordinal: Option<usize>,
    view_index: usize,
    view_identity: String,
    selected_products: Vec<CompilerAcquisitionProduct>,
    packages: [String; 1],
    target_triple: String,
    feature_profile: String,
    command_class: String,
    cargo_targets: Vec<CompilerAcquisitionCargoTarget>,
    attempts: u32,
    durable: DurableViewState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableGroupState {
    sequence: u64,
    views: Vec<usize>,
    terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalSchemas {
    compiler_fact: u32,
    observation: u32,
    native_cache: u32,
    collector: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerAcquisitionHeaderV2 {
    record: String,
    surface_acquisition_contract_version: u32,
    acquisition_identity: String,
    workspace_identity: String,
    checkout_identity: String,
    snapshot_identity: String,
    configuration_fingerprint: String,
    plan_identity: String,
    candidate_set_identity: String,
    compiler_set_identity: String,
    schemas: JournalSchemas,
    manifest: String,
    resume_command: Vec<String>,
    concurrency: usize,
    journal_batch: usize,
    artifact_soft_limit_bytes: u64,
    artifact_hard_limit_bytes: u64,
    products: Vec<CompilerAcquisitionProduct>,
    view_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSummary {
    state: String,
    failure: Option<FailureClass>,
    pending: usize,
    running: usize,
    completed: usize,
    failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcquisitionJournalV2 {
    header: CompilerAcquisitionHeaderV2,
    views: Vec<DurableViewRecord>,
    groups: Vec<DurableGroupState>,
    sequence: u64,
    summary: Option<DurableSummary>,
}

#[derive(Debug, Deserialize)]
struct CompilerAcquisitionHeaderV1 {
    record: String,
    surface_acquisition_contract_version: u32,
    acquisition_identity: String,
    snapshot_identity: String,
    configuration_fingerprint: String,
    views: usize,
}

#[derive(Debug, Deserialize)]
struct CompilerAcquisitionViewV1 {
    record: String,
    acquisition_identity: String,
    ordinal: usize,
    view_identity: String,
    status: String,
}

enum ResumeJournal {
    V1 { completed_prefix: usize },
    V2(Box<AcquisitionJournalV2>),
}

/// The coordinator-owned v2 writer. Workers never receive a reference to it.
pub(crate) struct CompilerAcquisitionJournal {
    path: PathBuf,
    document: AcquisitionJournalV2,
    dirty_views: BTreeSet<usize>,
    pending_completions: usize,
    pending_since: Option<Instant>,
    revalidation_required: BTreeSet<usize>,
    #[cfg(test)]
    fault: Option<JournalFault>,
}

impl CompilerAcquisitionJournal {
    #[expect(
        clippy::too_many_arguments,
        reason = "the journal header binds every independent acquisition authority at one boundary"
    )]
    pub(crate) fn begin(
        workspace_root: &Path,
        request: &CompilerAcquisitionRequest,
        plan: &CompilerAcquisitionPlan,
        compiler_set_identity: String,
        artifact_soft_limit_bytes: u64,
        artifact_hard_limit_bytes: u64,
        concurrency: usize,
        journal_batch: usize,
    ) -> RailResult<Self> {
        if journal_batch == 0 {
            return Err(RailError::message(
                "compiler acquisition journal batch must be non-zero",
            ));
        }
        let state_root = crate::workspace::cargo_rail_state_root(workspace_root);
        let canonical_state_root = crate::utils::canonicalize_existing(&state_root)?;
        let directory = state_root.join(V2_DIRECTORY);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {}
            Ok(_) => {
                return Err(RailError::message(format!(
                    "Surface acquisition journal directory '{}' is not a real directory",
                    directory.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&directory).map_err(|error| {
                    RailError::message(format!(
                        "failed to create Surface acquisition journal directory '{}': {error}",
                        directory.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
        let directory = crate::utils::canonicalize_existing(&directory)?;
        if directory.parent() != Some(canonical_state_root.as_path()) {
            return Err(RailError::message(
                "Surface acquisition journal directory escaped the workspace state root",
            ));
        }
        let schemas = JournalSchemas {
            compiler_fact: crate::compiler::facts::COMPILER_FACT_PROTOCOL_VERSION,
            observation: crate::compiler::invocation::OBSERVATION_PROTOCOL_VERSION,
            native_cache: crate::compiler::native_cache::native_cache_capability_schema_version(),
            collector: crate::compiler::model::COLLECTOR_VERSION,
        };
        let binding_bytes = serde_json::to_vec(&(
            ACQUISITION_CONTRACT_V2,
            &request.workspace_identity,
            &request.checkout_identity,
            &request.snapshot_identity,
            &request.configuration_fingerprint,
            plan.identity().as_str(),
            plan.candidate_set_identity(),
            &compiler_set_identity,
            &schemas,
        ))?;
        let acquisition_identity = format!("{ACQUISITION_IDENTITY_PREFIX}{}", ContentDigest::sha256(&binding_bytes));
        let path = directory.join(format!("{acquisition_identity}.json"));
        let display_path = display_path(workspace_root, &path);
        let resume_command = vec![
            "cargo".to_string(),
            "rail".to_string(),
            "surface".to_string(),
            "--resume".to_string(),
            display_path.clone(),
            "--format".to_string(),
            "json".to_string(),
        ];
        let header = CompilerAcquisitionHeaderV2 {
            record: "manifest".to_string(),
            surface_acquisition_contract_version: ACQUISITION_CONTRACT_V2,
            acquisition_identity,
            workspace_identity: request.workspace_identity.clone(),
            checkout_identity: request.checkout_identity.clone(),
            snapshot_identity: request.snapshot_identity.clone(),
            configuration_fingerprint: request.configuration_fingerprint.clone(),
            plan_identity: plan.identity().as_str().to_string(),
            candidate_set_identity: plan.candidate_set_identity().to_string(),
            compiler_set_identity,
            schemas,
            manifest: display_path.clone(),
            resume_command,
            concurrency,
            journal_batch,
            artifact_soft_limit_bytes,
            artifact_hard_limit_bytes,
            products: request.products.clone(),
            view_count: plan.view_count(),
        };
        let mut views = plan
            .execution_order()
            .map(|view| durable_view(&header, view))
            .collect::<RailResult<Vec<_>>>()?;
        let mut sequence = 0;
        let mut groups = Vec::new();
        let mut revalidation_required = BTreeSet::new();
        let mut resumed_v2 = false;
        if let Some(resume_path) = request.resume_manifest.as_deref() {
            match read_resume_journal(workspace_root, resume_path, Some(&header), Some(&views))? {
                ResumeJournal::V1 { completed_prefix } => {
                    for (position, view) in views.iter_mut().take(completed_prefix).enumerate() {
                        view.durable = DurableViewState::Complete {
                            evidence: EvidenceIdentity::new(view.view_identity.as_bytes()),
                        };
                        revalidation_required.insert(position);
                    }
                }
                ResumeJournal::V2(previous) => {
                    let previous = *previous;
                    resumed_v2 = true;
                    sequence = previous.sequence;
                    groups = previous.groups;
                    for (position, (view, old)) in views.iter_mut().zip(previous.views).enumerate() {
                        view.attempts = old.attempts;
                        view.cargo_targets = old.cargo_targets;
                        match old.durable {
                            DurableViewState::Complete { evidence } => {
                                view.durable = DurableViewState::Complete { evidence };
                                revalidation_required.insert(position);
                            }
                            DurableViewState::Running { .. }
                            | DurableViewState::Pending
                            | DurableViewState::Failed { .. } => {}
                        }
                    }
                }
            }
        }
        let document = AcquisitionJournalV2 {
            header,
            views,
            groups,
            sequence,
            summary: None,
        };
        if !resumed_v2 {
            document.validate()?;
        }
        let mut journal = Self {
            path,
            document,
            dirty_views: BTreeSet::new(),
            pending_completions: 0,
            pending_since: None,
            revalidation_required,
            #[cfg(test)]
            fault: None,
        };
        if resumed_v2 {
            journal.dirty_views.extend(0..journal.document.views.len());
            journal.flush()?;
        } else {
            journal.persist_initial()?;
        }
        progress!("  Surface acquisition manifest: {display_path}");
        Ok(journal)
    }

    pub(crate) fn evidence_identity(
        &self,
        index: ViewIx,
        diagnostic_keys: &[CompilerDiagKey],
        fact_key: Option<&CompilerFactCacheKey>,
    ) -> RailResult<EvidenceIdentity> {
        let view = self.view(index)?;
        let bytes = serde_json::to_vec(&(&view.view_identity, diagnostic_keys, fact_key))?;
        Ok(EvidenceIdentity::new(&bytes))
    }

    /// Reconcile a recovered completion with the independently validated stores.
    pub(crate) fn revalidate(&mut self, index: ViewIx, evidence: Option<EvidenceIdentity>) -> RailResult<()> {
        let position = self.position(index)?;
        if let Some(evidence) = evidence {
            evidence.validate()?;
            self.document.views[position].durable = DurableViewState::Complete { evidence };
        } else {
            self.document.views[position].durable = DurableViewState::Pending;
            self.document.views[position].cargo_targets.clear();
        }
        self.revalidation_required.remove(&position);
        self.dirty_views.insert(position);
        Ok(())
    }

    /// Persist all recovery decisions before any new process starts.
    pub(crate) fn seal_revalidation(&mut self) -> RailResult<()> {
        if !self.revalidation_required.is_empty() {
            return Err(RailError::message(
                "compiler acquisition journal recovery was not revalidated against every completed view",
            ));
        }
        self.flush()
    }

    /// Record one coordinator dispatch batch before any of its Cargo children launch.
    pub(crate) fn running_batch(&mut self, indices: &[ViewIx]) -> RailResult<()> {
        if indices.is_empty() {
            return Err(RailError::message(
                "compiler acquisition journal running batch is empty",
            ));
        }
        let positions = indices
            .iter()
            .map(|index| self.position(*index))
            .collect::<RailResult<BTreeSet<_>>>()?;
        if positions.len() != indices.len()
            || positions.iter().any(|position| {
                !matches!(
                    self.document.views[*position].durable,
                    DurableViewState::Pending | DurableViewState::Failed { .. }
                )
            })
        {
            return Err(RailError::message(
                "compiler acquisition journal can start only unique unresolved work",
            ));
        }
        for position in positions {
            let view = &mut self.document.views[position];
            view.attempts = view
                .attempts
                .checked_add(1)
                .ok_or_else(|| RailError::message("compiler acquisition journal attempt count overflowed"))?;
            view.durable = DurableViewState::Running { attempt: view.attempts };
            self.dirty_views.insert(position);
        }
        self.flush()
    }

    /// Buffer an out-of-order durable completion and group its atomic commit.
    pub(crate) fn complete(&mut self, index: ViewIx, evidence: EvidenceIdentity, durable: bool) -> RailResult<()> {
        evidence.validate()?;
        let position = self.position(index)?;
        let view = &mut self.document.views[position];
        if !matches!(view.durable, DurableViewState::Running { .. }) {
            return Err(RailError::message(
                "compiler acquisition journal completed a view that was not running",
            ));
        }
        view.durable = if durable {
            DurableViewState::Complete { evidence }
        } else {
            DurableViewState::Pending
        };
        self.dirty_views.insert(position);
        self.pending_completions = self.pending_completions.saturating_add(1);
        self.pending_since.get_or_insert_with(Instant::now);
        if self.pending_completions >= self.document.header.journal_batch {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn flush_if_due(&mut self) -> RailResult<()> {
        if self
            .pending_since
            .is_some_and(|started| started.elapsed() >= COMPLETION_FLUSH_INTERVAL)
        {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn completion_flush_timeout(&self) -> Option<Duration> {
        self.pending_since.map(|started| {
            COMPLETION_FLUSH_INTERVAL
                .checked_sub(started.elapsed())
                .unwrap_or(Duration::ZERO)
        })
    }

    pub(crate) fn fail(
        &mut self,
        primary: Option<(ViewIx, Vec<CompilerAcquisitionCargoTarget>, FailureClass)>,
        terminal_class: FailureClass,
    ) -> RailResult<()> {
        if let Some((index, cargo_targets, class)) = primary {
            let position = self.position(index)?;
            let view = &mut self.document.views[position];
            if !matches!(view.durable, DurableViewState::Complete { .. }) {
                view.durable = DurableViewState::Failed { class };
                view.cargo_targets = cargo_targets;
                self.dirty_views.insert(position);
            }
        }
        for (position, view) in self.document.views.iter_mut().enumerate() {
            if matches!(view.durable, DurableViewState::Running { .. }) {
                view.durable = DurableViewState::Failed {
                    class: FailureClass::Cancelled,
                };
                self.dirty_views.insert(position);
            }
        }
        self.document.summary = Some(self.summary("partial", Some(terminal_class)));
        self.flush()
    }

    pub(crate) fn finish(&mut self) -> RailResult<()> {
        if self
            .document
            .views
            .iter()
            .any(|view| matches!(view.durable, DurableViewState::Running { .. }))
        {
            return Err(RailError::message(
                "compiler acquisition journal cannot finish with a running view",
            ));
        }
        self.document.summary = Some(self.summary("complete", None));
        self.flush()
    }

    pub(crate) fn resume_help(&self) -> String {
        format!(
            "resume with '{}' after correcting the source failure; Cargo-Rail will recapture the workspace and revalidate every exact complete evidence object",
            self.document.header.resume_command.join(" ")
        )
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn summary(&self, state: &str, failure: Option<FailureClass>) -> DurableSummary {
        let count = |predicate: fn(&DurableViewState) -> bool| {
            self.document
                .views
                .iter()
                .filter(|view| predicate(&view.durable))
                .count()
        };
        DurableSummary {
            state: state.to_string(),
            failure,
            pending: count(|state| matches!(state, DurableViewState::Pending)),
            running: count(|state| matches!(state, DurableViewState::Running { .. })),
            completed: count(|state| matches!(state, DurableViewState::Complete { .. })),
            failed: count(|state| matches!(state, DurableViewState::Failed { .. })),
        }
    }

    fn position(&self, index: ViewIx) -> RailResult<usize> {
        self.document
            .views
            .iter()
            .position(|view| view.view_index == index.offset())
            .ok_or_else(|| RailError::message("Surface acquisition journal view identity disappeared"))
    }

    fn view(&self, index: ViewIx) -> RailResult<&DurableViewRecord> {
        let position = self.position(index)?;
        self.document
            .views
            .get(position)
            .ok_or_else(|| RailError::message("Surface acquisition journal view is out of bounds"))
    }

    fn persist_initial(&mut self) -> RailResult<()> {
        let encoded = serde_json::to_vec_pretty(&self.document)?;
        self.persist_bytes(&encoded)
    }

    fn flush(&mut self) -> RailResult<()> {
        if self.dirty_views.is_empty() && self.document.summary.is_none() {
            return Ok(());
        }
        let mut next = self.document.clone();
        next.sequence = next
            .sequence
            .checked_add(1)
            .ok_or_else(|| RailError::message("compiler acquisition journal sequence overflowed"))?;
        next.groups.push(DurableGroupState {
            sequence: next.sequence,
            views: self.dirty_views.iter().copied().collect(),
            terminal: next.summary.is_some(),
        });
        if next.groups.len() > MAX_JOURNAL_GROUPS {
            let excess = next.groups.len() - MAX_JOURNAL_GROUPS;
            next.groups.drain(..excess);
        }
        next.validate()?;
        let encoded = serde_json::to_vec_pretty(&next)?;
        self.persist_bytes(&encoded)?;
        self.document = next;
        self.dirty_views.clear();
        self.pending_completions = 0;
        self.pending_since = None;
        Ok(())
    }

    fn persist_bytes(&mut self, encoded: &[u8]) -> RailResult<()> {
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(RailError::message(
                "Surface acquisition manifest exceeds its byte bound",
            ));
        }
        #[cfg(test)]
        if self.fault == Some(JournalFault::BeforeReplace) {
            self.fault = None;
            return Err(RailError::message("injected journal replace failure"));
        }
        let started = crate::instrumentation::compiler_acquisition_timer();
        crate::utils::write_file_atomic(&self.path, encoded)?;
        crate::instrumentation::record_compiler_acquisition_journal_write(started, encoded.len(), true, true);
        #[cfg(test)]
        if self.fault == Some(JournalFault::AfterReplace) {
            self.fault = None;
            return Err(RailError::message("injected journal sync acknowledgement failure"));
        }
        Ok(())
    }
}

impl AcquisitionJournalV2 {
    fn validate(&self) -> RailResult<()> {
        let header = &self.header;
        if header.record != "manifest"
            || header.surface_acquisition_contract_version != ACQUISITION_CONTRACT_V2
            || header.view_count != self.views.len()
            || header.journal_batch == 0
        {
            return Err(RailError::message(
                "Surface acquisition manifest has an invalid v2 contract",
            ));
        }
        validate_sha256_identity(&header.acquisition_identity, ACQUISITION_IDENTITY_PREFIX, "acquisition")?;
        let mut identities = BTreeSet::new();
        let mut indices = BTreeSet::new();
        for (ordinal, view) in self.views.iter().enumerate() {
            let running_attempt = match view.durable {
                DurableViewState::Running { attempt } => Some(attempt),
                _ => None,
            };
            if view.legacy_ordinal.is_some_and(|legacy| legacy != ordinal)
                || !identities.insert(view.view_identity.as_str())
                || !indices.insert(view.view_index)
                || view.view_index >= self.views.len()
                || view.packages[0].is_empty()
                || running_attempt.is_some_and(|attempt| attempt == 0 || view.attempts != attempt)
            {
                return Err(RailError::message(
                    "Surface acquisition manifest has invalid, duplicate, or inconsistent view state",
                ));
            }
            if let DurableViewState::Complete { evidence } = &view.durable {
                evidence.validate()?;
            }
        }
        let mut previous = None;
        for group in &self.groups {
            if group.sequence == 0
                || group.sequence > self.sequence
                || previous.is_some_and(|prior| prior >= group.sequence)
                || group.views.windows(2).any(|pair| pair[0] >= pair[1])
                || group.views.iter().any(|ordinal| *ordinal >= self.views.len())
            {
                return Err(RailError::message(
                    "Surface acquisition manifest has invalid durability-group ordering",
                ));
            }
            previous = Some(group.sequence);
        }
        if self.groups.last().map_or(self.sequence != 0, |group| {
            group.sequence != self.sequence || group.terminal != self.summary.is_some()
        }) {
            return Err(RailError::message(
                "Surface acquisition manifest has an inconsistent durability sequence",
            ));
        }
        if let Some(summary) = &self.summary {
            let counts = self.views.iter().fold([0_usize; 4], |mut counts, view| {
                let offset = match view.durable {
                    DurableViewState::Pending => 0,
                    DurableViewState::Running { .. } => 1,
                    DurableViewState::Complete { .. } => 2,
                    DurableViewState::Failed { .. } => 3,
                };
                counts[offset] += 1;
                counts
            });
            let counts_match = counts == [summary.pending, summary.running, summary.completed, summary.failed];
            let terminal_state_matches = match (summary.state.as_str(), summary.failure) {
                // A successful analysis may leave a view pending when its
                // execution became unnecessary or its evidence could not be
                // published. A later resume executes it if still required.
                ("complete", None) => counts[1] == 0 && counts[3] == 0,
                ("partial", Some(_)) => counts[1] == 0,
                _ => false,
            };
            if !counts_match || !terminal_state_matches {
                return Err(RailError::message(
                    "Surface acquisition manifest has an inconsistent terminal summary",
                ));
            }
        }
        Ok(())
    }
}

fn durable_view(
    header: &CompilerAcquisitionHeaderV2,
    view: CompilerAcquisitionView<'_>,
) -> RailResult<DurableViewRecord> {
    let view_bytes = serde_json::to_vec(&(
        view.command_class(),
        PlatformTarget::from(view.platform()),
        view.features(),
        BTreeSet::from([view.package()]),
        view.fact_families(),
    ))?;
    Ok(DurableViewRecord {
        legacy_ordinal: None,
        view_index: view.index().offset(),
        view_identity: format!("compiler-view-v1-sha256-{}", ContentDigest::sha256(&view_bytes)),
        selected_products: header
            .products
            .iter()
            .filter(|product| product.package == view.package())
            .cloned()
            .collect(),
        packages: [view.package().to_string()],
        target_triple: view.platform().to_string(),
        feature_profile: view.features().label(),
        command_class: view.command_class().to_string(),
        cargo_targets: Vec::new(),
        attempts: 0,
        durable: DurableViewState::Pending,
    })
}

fn display_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn validate_sha256_identity(value: &str, prefix: &str, label: &str) -> RailResult<()> {
    let digest = value
        .strip_prefix(prefix)
        .ok_or_else(|| RailError::message(format!("Surface {label} identity has the wrong domain or version")))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RailError::message(format!(
            "Surface {label} identity is not canonical SHA-256"
        )));
    }
    Ok(())
}

fn resolve_resume_path(workspace_root: &Path, path: &Path) -> RailResult<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
        RailError::message(format!(
            "failed to inspect Surface acquisition manifest '{}': {error}",
            candidate.display()
        ))
    })?;
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(RailError::message(format!(
            "Surface acquisition manifest '{}' is not a bounded real regular file",
            candidate.display()
        )));
    }
    let candidate = crate::utils::canonicalize_existing(&candidate)?;
    let state_root = crate::workspace::cargo_rail_state_root(workspace_root);
    let authorized = [V1_DIRECTORY, V2_DIRECTORY]
        .into_iter()
        .filter_map(|directory| crate::utils::canonicalize_existing(&state_root.join(directory)).ok())
        .any(|directory| candidate.starts_with(directory));
    if !authorized {
        return Err(RailError::message(format!(
            "Surface acquisition manifest '{}' is outside the current workspace's journal authority",
            candidate.display()
        )));
    }
    Ok(candidate)
}

fn read_bounded(path: &Path) -> RailResult<Vec<u8>> {
    let mut file = fs::File::open(path).map_err(RailError::from)?;
    let expected_len = file.metadata().map_err(RailError::from)?.len();
    if expected_len > MAX_JOURNAL_BYTES {
        return Err(RailError::message(
            "Surface acquisition manifest exceeds its byte bound",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(RailError::from)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(RailError::message(
            "Surface acquisition manifest exceeds its byte bound",
        ));
    }
    if !crate::utils::private_file_matches_path(&file, path, expected_len).map_err(RailError::from)? {
        return Err(RailError::message(
            "Surface acquisition manifest changed identity while it was read",
        ));
    }
    Ok(bytes)
}

fn read_resume_journal(
    workspace_root: &Path,
    path: &Path,
    expected_v2: Option<&CompilerAcquisitionHeaderV2>,
    expected_views: Option<&[DurableViewRecord]>,
) -> RailResult<ResumeJournal> {
    let path = resolve_resume_path(workspace_root, path)?;
    let bytes = read_bounded(&path)?;
    let first_line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| RailError::message("Surface acquisition manifest is empty"))?;
    if let Ok(document) = serde_json::from_slice::<AcquisitionJournalV2>(&bytes) {
        document.validate()?;
        if let Some(expected) = expected_v2 {
            validate_v2_binding(&document.header, expected)?;
        }
        if let Some(expected) = expected_views
            && (document.views.len() != expected.len()
                || document.views.iter().zip(expected).any(|(actual, expected)| {
                    actual.view_index != expected.view_index || actual.view_identity != expected.view_identity
                }))
        {
            return Err(RailError::message(
                "Surface acquisition resume view catalog differs from the current plan",
            ));
        }
        Ok(ResumeJournal::V2(Box::new(document)))
    } else {
        let first: serde_json::Value = serde_json::from_slice(first_line)?;
        match first
            .get("surface_acquisition_contract_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(version) if version == u64::from(ACQUISITION_CONTRACT_V1) => {
                read_v1(&bytes, expected_v2, expected_views)
            }
            _ => Err(RailError::message(
                "Surface acquisition manifest has an unsupported contract",
            )),
        }
    }
}

fn validate_v2_binding(actual: &CompilerAcquisitionHeaderV2, expected: &CompilerAcquisitionHeaderV2) -> RailResult<()> {
    if actual.workspace_identity != expected.workspace_identity
        || actual.checkout_identity != expected.checkout_identity
        || actual.snapshot_identity != expected.snapshot_identity
        || actual.configuration_fingerprint != expected.configuration_fingerprint
        || actual.plan_identity != expected.plan_identity
        || actual.candidate_set_identity != expected.candidate_set_identity
        || actual.compiler_set_identity != expected.compiler_set_identity
        || actual.schemas != expected.schemas
        || actual.acquisition_identity != expected.acquisition_identity
    {
        return Err(RailError::with_help(
            "Surface acquisition resume authority differs from the current workspace, plan, compiler set, or schema",
            "rerun 'cargo rail surface' without --resume to plan the changed acquisition",
        ));
    }
    Ok(())
}

fn read_v1(
    bytes: &[u8],
    expected_v2: Option<&CompilerAcquisitionHeaderV2>,
    expected_views: Option<&[DurableViewRecord]>,
) -> RailResult<ResumeJournal> {
    let mut lines = BufReader::new(bytes).lines();
    let header: CompilerAcquisitionHeaderV1 = serde_json::from_str(
        &lines
            .next()
            .transpose()
            .map_err(RailError::from)?
            .ok_or_else(|| RailError::message("Surface acquisition manifest is empty"))?,
    )?;
    if header.record != "manifest" || header.surface_acquisition_contract_version != ACQUISITION_CONTRACT_V1 {
        return Err(RailError::message(
            "Surface acquisition manifest has an invalid v1 contract",
        ));
    }
    if let Some(expected) = expected_v2
        && (header.snapshot_identity != expected.snapshot_identity
            || header.configuration_fingerprint != expected.configuration_fingerprint)
    {
        return Err(RailError::with_help(
            "Surface acquisition v1 resume snapshot or configuration differs from the current acquisition",
            "rerun 'cargo rail surface' without --resume to plan the changed acquisition",
        ));
    }
    let expected = expected_views
        .ok_or_else(|| RailError::message("Surface acquisition v1 migration requires the current view catalog"))?;
    if header.views != expected.len() {
        return Err(RailError::message(
            "Surface acquisition v1 resume view count differs from the current plan",
        ));
    }
    let record_limit = header
        .views
        .checked_mul(MAX_V1_RECORDS_PER_VIEW)
        .and_then(|count| count.checked_add(16))
        .ok_or_else(|| RailError::message("Surface acquisition v1 record bound overflowed"))?;
    let mut statuses = vec!["planned".to_string(); header.views];
    for (record, line) in lines.enumerate() {
        if record >= record_limit {
            return Err(RailError::message(
                "Surface acquisition v1 manifest exceeds its record bound",
            ));
        }
        let line = line.map_err(RailError::from)?;
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("record").and_then(serde_json::Value::as_str) != Some("view") {
            continue;
        }
        let view: CompilerAcquisitionViewV1 = serde_json::from_value(value)?;
        let current = expected
            .get(view.ordinal)
            .ok_or_else(|| RailError::message("Surface acquisition v1 view ordinal is out of bounds"))?;
        if view.record != "view"
            || view.acquisition_identity != header.acquisition_identity
            || view.view_identity != current.view_identity
        {
            return Err(RailError::message(
                "Surface acquisition v1 view does not match the current canonical view catalog",
            ));
        }
        statuses[view.ordinal] = view.status;
    }
    let completed_prefix = statuses
        .iter()
        .take_while(|status| matches!(status.as_str(), "reused" | "completed"))
        .count();
    Ok(ResumeJournal::V1 { completed_prefix })
}

/// Validate path authority and the effective configuration before analysis builds the exact plan.
pub(crate) fn validate_compiler_acquisition_resume(
    workspace_root: &Path,
    path: &Path,
    configuration_fingerprint: &str,
) -> RailResult<()> {
    let path = resolve_resume_path(workspace_root, path).map_err(|error| {
        RailError::with_help(
            error.to_string(),
            "run 'cargo rail surface' once without --resume to create an acquisition manifest",
        )
    })?;
    let bytes = read_bounded(&path)?;
    let first_line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| RailError::message("Surface acquisition manifest is empty"))?;
    let (version, configuration) = if let Ok(document) = serde_json::from_slice::<AcquisitionJournalV2>(&bytes) {
        document.validate()?;
        (
            Some(u64::from(document.header.surface_acquisition_contract_version)),
            Some(document.header.configuration_fingerprint),
        )
    } else {
        let first: serde_json::Value = serde_json::from_slice(first_line)?;
        (
            first
                .get("surface_acquisition_contract_version")
                .and_then(serde_json::Value::as_u64),
            first
                .get("configuration_fingerprint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        )
    };
    if !matches!(version, Some(1 | 2)) {
        return Err(RailError::message(
            "Surface acquisition manifest has an unsupported contract",
        ));
    }
    if configuration.as_deref() != Some(configuration_fingerprint) {
        return Err(RailError::with_help(
            "Surface acquisition resume configuration differs from the planned acquisition",
            "rerun 'cargo rail surface' without --resume to plan the changed configuration",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalFault {
    BeforeReplace,
    AfterReplace,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(resume_manifest: Option<PathBuf>) -> CompilerAcquisitionRequest {
        CompilerAcquisitionRequest {
            workspace_identity: "workspace-v1".to_string(),
            checkout_identity: "checkout-v1".to_string(),
            snapshot_identity: "snapshot-v1".to_string(),
            configuration_fingerprint: "configuration-v1".to_string(),
            products: Vec::new(),
            resume_manifest,
        }
    }

    fn begin(
        root: &Path,
        plan: &CompilerAcquisitionPlan,
        resume_manifest: Option<PathBuf>,
        batch: usize,
    ) -> CompilerAcquisitionJournal {
        fs::create_dir_all(crate::workspace::cargo_rail_state_root(root)).expect("state root");
        CompilerAcquisitionJournal::begin(
            root,
            &request(resume_manifest),
            plan,
            "compiler-set-v1".to_string(),
            1024,
            2048,
            plan.view_count(),
            batch,
        )
        .expect("journal")
    }

    fn read_document(path: &Path) -> AcquisitionJournalV2 {
        serde_json::from_slice(&fs::read(path).expect("journal bytes")).expect("journal document")
    }

    fn state(document: &AcquisitionJournalV2, index: ViewIx) -> &DurableViewState {
        &document
            .views
            .iter()
            .find(|view| view.view_index == index.offset())
            .expect("view")
            .durable
    }

    fn evidence(journal: &CompilerAcquisitionJournal, index: ViewIx) -> EvidenceIdentity {
        journal.evidence_identity(index, &[], None).expect("evidence identity")
    }

    #[test]
    fn out_of_order_completions_commit_as_one_canonical_group() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha", "beta", "gamma"]).expect("plan");
        let order = plan
            .execution_order()
            .map(CompilerAcquisitionView::index)
            .collect::<Vec<_>>();
        let first = order[0];
        let last = order[2];
        let mut journal = begin(root.path(), &plan, None, 2);
        let initial = fs::read_to_string(journal.path()).expect("initial journal");
        assert!(
            !initial.contains("\"ordinal\""),
            "new v2 journals must not serialize ordinal authority"
        );

        journal.running_batch(&[last, first]).expect("start batch");
        journal
            .complete(last, evidence(&journal, last), true)
            .expect("buffer last completion");
        let before_group = read_document(journal.path());
        assert!(matches!(state(&before_group, last), DurableViewState::Running { .. }));

        journal
            .complete(first, evidence(&journal, first), true)
            .expect("commit completion group");
        let after_group = read_document(journal.path());
        assert!(matches!(state(&after_group, first), DurableViewState::Complete { .. }));
        assert!(matches!(state(&after_group, last), DurableViewState::Complete { .. }));
        assert_eq!(after_group.sequence, 2, "one running batch and one completion batch");
        assert_eq!(after_group.groups.len(), 2);
        let group = after_group.groups.last().expect("completion group");
        assert_eq!(group.views.len(), 2);
        assert!(group.views.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn forced_termination_recovers_complete_views_and_resets_running_views() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha", "beta", "gamma"]).expect("plan");
        let order = plan
            .execution_order()
            .map(CompilerAcquisitionView::index)
            .collect::<Vec<_>>();
        let complete = order[0];
        let interrupted = order[1];
        let path = {
            let mut journal = begin(root.path(), &plan, None, 1);
            journal.running_batch(&[complete]).expect("start complete view");
            journal
                .complete(complete, evidence(&journal, complete), true)
                .expect("complete view");
            journal.running_batch(&[interrupted]).expect("start interrupted view");
            journal.path().to_path_buf()
        };

        let mut resumed = begin(root.path(), &plan, Some(path), 1);
        assert!(matches!(
            resumed.view(complete).expect("complete state").durable,
            DurableViewState::Complete { .. }
        ));
        assert!(matches!(
            resumed.view(interrupted).expect("interrupted state").durable,
            DurableViewState::Pending
        ));
        assert!(resumed.seal_revalidation().is_err());
        resumed
            .revalidate(complete, Some(evidence(&resumed, complete)))
            .expect("revalidate complete evidence");
        resumed.seal_revalidation().expect("seal recovery");
    }

    #[test]
    fn v1_reader_migrates_only_the_completed_ordinal_prefix() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha", "beta", "gamma"]).expect("plan");
        let journal = begin(root.path(), &plan, None, 2);
        let header = &journal.document.header;
        let views = &journal.document.views;
        let mut records = vec![serde_json::json!({
            "record": "manifest",
            "surface_acquisition_contract_version": 1,
            "acquisition_identity": "surface-acquisition-v1-sha256-deadbeef",
            "snapshot_identity": header.snapshot_identity,
            "configuration_fingerprint": header.configuration_fingerprint,
            "views": views.len(),
        })];
        for (ordinal, view) in views.iter().enumerate() {
            records.push(serde_json::json!({
                "record": "view",
                "acquisition_identity": "surface-acquisition-v1-sha256-deadbeef",
                "ordinal": ordinal,
                "view_identity": view.view_identity,
                "status": "planned",
            }));
        }
        records.push(serde_json::json!({
            "record": "view",
            "acquisition_identity": "surface-acquisition-v1-sha256-deadbeef",
            "ordinal": 1,
            "view_identity": views[1].view_identity,
            "status": "completed",
        }));
        let encoded = records
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            read_v1(encoded.as_bytes(), Some(header), Some(views)).expect("out-of-order v1"),
            ResumeJournal::V1 { completed_prefix: 0 }
        ));

        records.push(serde_json::json!({
            "record": "view",
            "acquisition_identity": "surface-acquisition-v1-sha256-deadbeef",
            "ordinal": 0,
            "view_identity": views[0].view_identity,
            "status": "reused",
        }));
        let encoded = records
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            read_v1(encoded.as_bytes(), Some(header), Some(views)).expect("prefix v1"),
            ResumeJournal::V1 { completed_prefix: 2 }
        ));
    }

    #[test]
    fn replace_and_sync_acknowledgement_faults_recover_at_an_atomic_boundary() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha"]).expect("plan");
        let view = plan.execution_order().next().expect("view").index();

        let mut before = begin(root.path(), &plan, None, 1);
        before.running_batch(&[view]).expect("running");
        before.fault = Some(JournalFault::BeforeReplace);
        assert!(before.complete(view, evidence(&before, view), true).is_err());
        assert!(matches!(
            state(&read_document(before.path()), view),
            DurableViewState::Running { .. }
        ));

        let mut after = begin(root.path(), &plan, None, 1);
        after.running_batch(&[view]).expect("running");
        after.fault = Some(JournalFault::AfterReplace);
        assert!(after.complete(view, evidence(&after, view), true).is_err());
        assert!(matches!(
            state(&read_document(after.path()), view),
            DurableViewState::Complete { .. }
        ));
    }

    #[test]
    fn duration_bound_flushes_a_partial_completion_group() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha"]).expect("plan");
        let view = plan.execution_order().next().expect("view").index();
        let mut journal = begin(root.path(), &plan, None, 8);
        journal.running_batch(&[view]).expect("running");
        journal
            .complete(view, evidence(&journal, view), true)
            .expect("buffer completion");
        journal.pending_since = Some(
            Instant::now()
                .checked_sub(COMPLETION_FLUSH_INTERVAL)
                .expect("test duration is representable"),
        );
        journal.flush_if_due().expect("duration flush");
        assert!(matches!(
            state(&read_document(journal.path()), view),
            DurableViewState::Complete { .. }
        ));
    }

    #[test]
    fn fatal_transition_flushes_completed_progress_and_terminal_failure_class() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha", "beta"]).expect("plan");
        let views = plan
            .execution_order()
            .map(CompilerAcquisitionView::index)
            .collect::<Vec<_>>();
        let mut journal = begin(root.path(), &plan, None, 8);
        journal.running_batch(&views).expect("running batch");
        journal
            .complete(views[0], evidence(&journal, views[0]), true)
            .expect("buffer completed progress");
        journal
            .fail(Some((views[1], Vec::new(), FailureClass::Worker)), FailureClass::Worker)
            .expect("terminal journal");

        let terminal = read_document(journal.path());
        assert!(matches!(state(&terminal, views[0]), DurableViewState::Complete { .. }));
        assert!(matches!(
            state(&terminal, views[1]),
            DurableViewState::Failed {
                class: FailureClass::Worker
            }
        ));
        assert_eq!(
            terminal.summary.as_ref().and_then(|summary| summary.failure),
            Some(FailureClass::Worker)
        );
        assert_eq!(terminal.summary.as_ref().map(|summary| summary.running), Some(0));
        assert!(terminal.groups.last().is_some_and(|group| group.terminal));
    }

    #[test]
    fn terminal_partial_journal_can_enter_a_new_resume_sequence() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha", "beta"]).expect("plan");
        let views = plan
            .execution_order()
            .map(CompilerAcquisitionView::index)
            .collect::<Vec<_>>();
        let path = {
            let mut journal = begin(root.path(), &plan, None, 8);
            journal.running_batch(&views).expect("running batch");
            journal
                .complete(views[0], evidence(&journal, views[0]), true)
                .expect("buffer completed progress");
            journal
                .fail(
                    Some((views[1], Vec::new(), FailureClass::Coordinator)),
                    FailureClass::Coordinator,
                )
                .expect("terminal partial journal");
            journal.path().to_path_buf()
        };

        let resumed = begin(root.path(), &plan, Some(path), 8);
        let active = read_document(resumed.path());
        assert!(active.summary.is_none());
        assert!(active.groups.last().is_some_and(|group| !group.terminal));
        assert!(active.groups.iter().any(|group| group.terminal));
        assert!(matches!(state(&active, views[0]), DurableViewState::Complete { .. }));
        assert!(matches!(state(&active, views[1]), DurableViewState::Pending));
    }

    #[test]
    fn successful_non_durable_view_finishes_but_remains_pending_for_resume() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha"]).expect("plan");
        let view = plan.execution_order().next().expect("view").index();
        let path = {
            let mut journal = begin(root.path(), &plan, None, 1);
            journal.running_batch(&[view]).expect("running");
            journal
                .complete(view, evidence(&journal, view), false)
                .expect("non-durable completion");
            journal.finish().expect("successful terminal journal");
            let terminal = read_document(journal.path());
            assert!(matches!(state(&terminal, view), DurableViewState::Pending));
            assert_eq!(
                terminal.summary.as_ref().map(|summary| summary.state.as_str()),
                Some("complete")
            );
            assert_eq!(terminal.summary.as_ref().map(|summary| summary.pending), Some(1));
            journal.path().to_path_buf()
        };

        let resumed = begin(root.path(), &plan, Some(path), 1);
        assert!(matches!(
            resumed.view(view).expect("view").durable,
            DurableViewState::Pending
        ));
    }

    #[test]
    fn binding_and_group_corruption_fail_closed() {
        let root = tempfile::tempdir().expect("root");
        let plan = CompilerAcquisitionPlan::journal_test_plan(&["alpha"]).expect("plan");
        let journal = begin(root.path(), &plan, None, 1);
        let mut mismatched = journal.document.header.clone();
        mismatched.compiler_set_identity.push_str("-changed");
        assert!(validate_v2_binding(&mismatched, &journal.document.header).is_err());

        let mut corrupted = journal.document;
        corrupted.sequence = 1;
        corrupted.groups.push(DurableGroupState {
            sequence: 1,
            views: vec![1],
            terminal: false,
        });
        assert!(corrupted.validate().is_err());

        corrupted.groups.clear();
        corrupted.groups.push(DurableGroupState {
            sequence: 1,
            views: Vec::new(),
            terminal: true,
        });
        corrupted.summary = Some(DurableSummary {
            state: "complete".to_string(),
            failure: None,
            pending: 0,
            running: 0,
            completed: 1,
            failed: 0,
        });
        assert!(corrupted.validate().is_err());
    }
}
