//! Authenticated command-local coordination for duplicate compiler actions.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::compiler::scheduler::ViewIx;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const BROKER_PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CAPABILITY_BYTES: usize = 32;
const CAPABILITY_HEX_BYTES: usize = CAPABILITY_BYTES * 2;

/// Stable native-execution identity stage used only to rendezvous work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "stage", content = "identity", rename_all = "snake_case")]
pub(crate) enum ExecutionClaimKey {
    Base(String),
    Selected(String),
    LinkCandidate(String),
}

impl ExecutionClaimKey {
    fn validate(&self) -> RailResult<()> {
        match self {
            Self::Base(identity) => crate::compiler::native_cache::validate_base_action_key(identity),
            Self::Selected(identity) => crate::compiler::native_cache::validate_action_key(identity),
            Self::LinkCandidate(identity) => validate_identity(
                identity,
                crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX,
                "link-candidate selector",
            ),
        }
    }
}

/// One command-local flight identity. It is never sufficient for cache reuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateFlightKey {
    execution: ExecutionClaimKey,
    analysis: String,
}

impl CandidateFlightKey {
    pub(crate) fn new(execution: ExecutionClaimKey, analysis: String) -> RailResult<Self> {
        let key = Self { execution, analysis };
        key.validate()?;
        Ok(key)
    }

    pub(crate) fn claim_identity(&self) -> RailResult<ContentDigest> {
        self.validate()?;
        Ok(ContentDigest::sha256(&serde_json::to_vec(self)?))
    }

    fn validate(&self) -> RailResult<()> {
        self.execution.validate()?;
        validate_identity(
            &self.analysis,
            crate::compiler::analysis::ANALYSIS_CONTRACT_ID_PREFIX,
            "analysis contract",
        )
    }

    fn validate_candidate(&self, candidate: &CandidateResult) -> RailResult<()> {
        self.validate()?;
        candidate.validate()?;
        if matches!(&self.execution, ExecutionClaimKey::Selected(action) if action != candidate.action())
            || crate::compiler::diagnostics_store::NativeEvidenceBindingValidation::candidate_key(
                candidate.action(),
                candidate.result(),
                &self.analysis,
            ) != candidate.evidence_candidate()
        {
            return Err(RailError::message(
                "compiler acquisition completion does not match its exact flight authority",
            ));
        }
        Ok(())
    }
}

/// Exact immutable candidate published by a completed flight leader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateResult {
    action: String,
    result: String,
    evidence_candidate: String,
}

impl CandidateResult {
    pub(crate) fn new(action: String, result: String, evidence_candidate: String) -> RailResult<Self> {
        let candidate = Self {
            action,
            result,
            evidence_candidate,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub(crate) fn action(&self) -> &str {
        &self.action
    }

    pub(crate) fn result(&self) -> &str {
        &self.result
    }

    pub(crate) fn evidence_candidate(&self) -> &str {
        &self.evidence_candidate
    }

    fn validate(&self) -> RailResult<()> {
        crate::compiler::native_cache::validate_action_key(&self.action)?;
        crate::compiler::native_cache::validate_result_key(&self.result)?;
        crate::compiler::diagnostics_store::validate_evidence_candidate_key(&self.evidence_candidate)
    }
}

/// Private endpoint and capability embedded in one authenticated fact session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerEnvironment {
    endpoint: String,
    capability: String,
    view: u32,
}

impl BrokerEnvironment {
    pub(crate) fn validate(&self) -> RailResult<()> {
        validate_capability(&self.capability)?;
        validate_endpoint(&self.endpoint)
    }
}

/// One active Cargo view holding the runtime's whole-view work permit.
pub(crate) struct BrokerView {
    event_tx: mpsc::SyncSender<ActorEvent>,
    environment: BrokerEnvironment,
    finished: bool,
}

impl BrokerView {
    pub(crate) fn environment(&self) -> &BrokerEnvironment {
        &self.environment
    }

    pub(crate) fn finish(mut self) -> RailResult<()> {
        self.finish_inner()?;
        self.finished = true;
        Ok(())
    }

    fn finish_inner(&self) -> RailResult<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.event_tx
            .send(ActorEvent::FinishView {
                view: self.environment.view,
                reply,
            })
            .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped"))?;
        actor_result(response.recv(), "finishing compiler acquisition broker view")
    }

    fn abort_inner(&self) -> RailResult<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.event_tx
            .send(ActorEvent::AbortView {
                view: self.environment.view,
                reply,
            })
            .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped"))?;
        actor_result(response.recv(), "aborting compiler acquisition broker view")
    }
}

impl Drop for BrokerView {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.abort_inner());
        }
    }
}

/// Command-lifetime broker, fixed worker set, and one-slot permit actor.
pub(crate) struct AcquisitionBroker {
    endpoint: String,
    capability: String,
    event_tx: mpsc::SyncSender<ActorEvent>,
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<RailResult<()>>>,
    workers: Vec<JoinHandle<RailResult<()>>>,
    actor: Option<JoinHandle<RailResult<()>>>,
    #[cfg(unix)]
    runtime: Option<tempfile::TempDir>,
    closed: bool,
}

impl AcquisitionBroker {
    pub(crate) fn start(work_permits: usize, cas: crate::cache::cas::LocalCas) -> RailResult<Self> {
        if work_permits == 0 || work_permits > MAX_CONNECTIONS {
            return Err(RailError::message("compiler acquisition work-permit bound is invalid"));
        }
        let capability = random_hex()?;
        let token = random_hex()?;
        #[cfg(unix)]
        let (runtime, endpoint, listener) = {
            use std::os::unix::fs::PermissionsExt as _;

            let mut builder = tempfile::Builder::new();
            builder
                .prefix("cargo-rail-acquisition-")
                .permissions(std::fs::Permissions::from_mode(0o700));
            let runtime = builder.tempdir()?;
            let endpoint = runtime.path().join("broker.sock");
            let endpoint = endpoint
                .to_str()
                .ok_or_else(|| RailError::message("compiler acquisition broker endpoint is not UTF-8"))?
                .to_string();
            let listener = BrokerListener::bind(&endpoint, MAX_CONNECTIONS)?;
            (Some(runtime), endpoint, listener)
        };
        #[cfg(windows)]
        let (endpoint, listener) = {
            let endpoint = format!(r"\\.\pipe\cargo-rail-acquisition-{token}");
            let listener = BrokerListener::bind(&endpoint, MAX_CONNECTIONS)?;
            (endpoint, listener)
        };
        #[cfg(not(any(unix, windows)))]
        let (endpoint, listener) = {
            drop(token);
            return Err(RailError::message(
                "compiler acquisition broker is unavailable on this host",
            ));
        };
        #[cfg(unix)]
        drop(token);

        let event_capacity = MAX_CONNECTIONS
            .checked_mul(2)
            .and_then(|capacity| capacity.checked_add(work_permits))
            .ok_or_else(|| RailError::message("compiler acquisition broker channel bound overflow"))?;
        let (event_tx, event_rx) = mpsc::sync_channel(event_capacity);
        let actor = std::thread::Builder::new()
            .name("cargo-rail-acquisition-coordinator".to_string())
            .spawn(move || run_actor(event_rx, work_permits))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(MAX_CONNECTIONS);
        let accepted_rx = Arc::new(Mutex::new(accepted_rx));
        let sessions = Arc::new(AtomicU64::new(1));
        let connection_workers = work_permits.saturating_mul(2).saturating_add(2).min(MAX_CONNECTIONS);
        let mut workers = Vec::with_capacity(connection_workers);
        for index in 0..connection_workers {
            let accepted_rx = Arc::clone(&accepted_rx);
            let event_tx = event_tx.clone();
            let capability = capability.clone();
            let sessions = Arc::clone(&sessions);
            let cas = cas.clone();
            workers.push(
                std::thread::Builder::new()
                    .name(format!("cargo-rail-acquisition-broker-{index}"))
                    .spawn(move || run_connection_worker(accepted_rx, event_tx, &capability, &sessions, &cas))?,
            );
        }

        let accept_shutdown = Arc::clone(&shutdown);
        let accept = std::thread::Builder::new()
            .name("cargo-rail-acquisition-listener".to_string())
            .spawn(move || run_accept_loop(listener, accepted_tx, &accept_shutdown))?;

        Ok(Self {
            endpoint,
            capability,
            event_tx,
            shutdown,
            accept: Some(accept),
            workers,
            actor: Some(actor),
            #[cfg(unix)]
            runtime,
            closed: false,
        })
    }

    pub(crate) fn begin_view(&self, view: ViewIx) -> RailResult<BrokerView> {
        let view = u32::try_from(view.offset())
            .map_err(|_| RailError::message("compiler acquisition view index exceeds broker protocol"))?;
        self.begin_view_index(view)
    }

    fn begin_view_index(&self, view: u32) -> RailResult<BrokerView> {
        let (reply, response) = mpsc::sync_channel(1);
        self.event_tx
            .send(ActorEvent::BeginView { view, reply })
            .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped"))?;
        actor_result(response.recv(), "starting compiler acquisition broker view")?;
        Ok(BrokerView {
            event_tx: self.event_tx.clone(),
            environment: BrokerEnvironment {
                endpoint: self.endpoint.clone(),
                capability: self.capability.clone(),
                view,
            },
            finished: false,
        })
    }

    pub(crate) fn close(mut self) -> RailResult<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> RailResult<()> {
        if self.closed {
            return Ok(());
        }
        self.shutdown.store(true, Ordering::Release);
        wake_listener(&self.endpoint);
        if let Some(accept) = self.accept.take() {
            join_thread(accept, "compiler acquisition broker listener")?;
        }
        for worker in self.workers.drain(..) {
            join_thread(worker, "compiler acquisition broker worker")?;
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.event_tx
            .send(ActorEvent::Shutdown { reply })
            .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped before shutdown"))?;
        actor_result(response.recv(), "shutting down compiler acquisition broker")?;
        if let Some(actor) = self.actor.take() {
            join_thread(actor, "compiler acquisition broker coordinator")?;
        }
        #[cfg(unix)]
        if let Some(runtime) = self.runtime.take() {
            runtime.close().map_err(|error| {
                RailError::message(format!("failed to remove compiler acquisition broker runtime: {error}"))
            })?;
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for AcquisitionBroker {
    fn drop(&mut self) {
        drop(self.close_inner());
    }
}

/// Role assigned to one authenticated wrapper flight claim.
pub(crate) enum BrokerClaim {
    Leader(BrokerLeader),
    Follower(BrokerFollower),
    Unavailable,
}

impl BrokerClaim {
    pub(crate) fn connect(environment: &BrokerEnvironment, key: CandidateFlightKey) -> RailResult<Self> {
        environment.validate()?;
        key.validate()?;
        let mut stream = BrokerStream::connect(&environment.endpoint)?;
        write_transition(
            &mut stream,
            &environment.capability,
            ClientTransition::Claim {
                view: environment.view,
                key,
            },
        )?;
        match read_server_transition(&mut stream, &environment.capability)? {
            ServerTransition::Lead { claim_contended } => {
                stream.enter_live()?;
                Ok(Self::Leader(BrokerLeader {
                    stream: Some(stream),
                    capability: environment.capability.clone(),
                    claim_contended,
                    completed: false,
                }))
            }
            ServerTransition::Wait => {
                stream.enter_live()?;
                Ok(Self::Follower(BrokerFollower {
                    stream,
                    capability: environment.capability.clone(),
                }))
            }
            ServerTransition::Failed {
                class: BrokerFailureClass::ExecutionClaim,
            } => Ok(Self::Unavailable),
            ServerTransition::Failed { class } => Err(broker_failure(class)),
            ServerTransition::Ack | ServerTransition::Resume { .. } => Err(RailError::message(
                "compiler acquisition broker returned an invalid claim transition",
            )),
        }
    }
}

/// Leader-side broker session retained through execution and publication.
pub(crate) struct BrokerLeader {
    stream: Option<BrokerStream>,
    capability: String,
    claim_contended: bool,
    completed: bool,
}

impl BrokerLeader {
    /// Finish broker-owned execution-claim acquisition before compiler work.
    ///
    /// An uncontended claim is already held by the command process. On
    /// contention the wrapper acknowledges its wait, the broker yields this
    /// view's permit, blocks on the stable shard, then reacquires the permit
    /// before allowing the wrapper to continue. The broker retains the claim
    /// through successful analysis-binding publication.
    pub(crate) fn acquire_execution_claim(&mut self) -> RailResult<bool> {
        if !self.claim_contended {
            return Ok(true);
        }
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| RailError::message("compiler acquisition leader stream is closed"))?;
        write_transition(stream, &self.capability, ClientTransition::Yielded)?;
        expect_ack(stream, &self.capability, "yielding compiler acquisition work permit")?;
        match read_server_transition(stream, &self.capability)? {
            ServerTransition::Resume { candidate: None } => {
                self.claim_contended = false;
                Ok(true)
            }
            ServerTransition::Failed {
                class: BrokerFailureClass::ExecutionClaim,
            } => Ok(false),
            ServerTransition::Failed { class } => Err(broker_failure(class)),
            ServerTransition::Lead { .. }
            | ServerTransition::Wait
            | ServerTransition::Ack
            | ServerTransition::Resume { candidate: Some(_) } => Err(RailError::message(
                "compiler acquisition broker returned an invalid execution-claim transition",
            )),
        }
    }

    pub(crate) fn complete(mut self, candidate: CandidateResult) -> RailResult<()> {
        candidate.validate()?;
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| RailError::message("compiler acquisition leader stream is closed"))?;
        write_transition(stream, &self.capability, ClientTransition::Complete { candidate })?;
        expect_ack(stream, &self.capability, "completing compiler acquisition flight")?;
        self.completed = true;
        self.stream = None;
        Ok(())
    }

    pub(crate) fn fail(mut self, class: BrokerFailureClass) -> RailResult<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| RailError::message("compiler acquisition leader stream is closed"))?;
        write_transition(stream, &self.capability, ClientTransition::Failed { class })?;
        expect_ack(stream, &self.capability, "failing compiler acquisition flight")?;
        self.completed = true;
        self.stream = None;
        Ok(())
    }
}

impl Drop for BrokerLeader {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Some(stream) = self.stream.as_mut() {
            drop(write_transition(stream, &self.capability, ClientTransition::Cancelled));
            drop(read_server_transition(stream, &self.capability));
        }
    }
}

/// Follower-side broker wait. Returned candidates remain non-authoritative.
pub(crate) struct BrokerFollower {
    stream: BrokerStream,
    capability: String,
}

impl BrokerFollower {
    pub(crate) fn wait(mut self) -> RailResult<Option<CandidateResult>> {
        match read_server_transition(&mut self.stream, &self.capability)? {
            ServerTransition::Resume {
                candidate: Some(candidate),
            } => {
                candidate.validate()?;
                Ok(Some(candidate))
            }
            ServerTransition::Failed {
                class: BrokerFailureClass::ExecutionClaim,
            } => Ok(None),
            ServerTransition::Failed { class } => Err(broker_failure(class)),
            ServerTransition::Lead { .. }
            | ServerTransition::Wait
            | ServerTransition::Ack
            | ServerTransition::Resume { candidate: None } => Err(RailError::message(
                "compiler acquisition broker returned an invalid follower transition",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokerFailureClass {
    Cancelled,
    Compiler,
    Evidence,
    ExecutionClaim,
    Protocol,
    Publication,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientEnvelope {
    version: u32,
    capability: String,
    transition: ClientTransition,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClientTransition {
    Claim { view: u32, key: CandidateFlightKey },
    Yielded,
    Complete { candidate: CandidateResult },
    Failed { class: BrokerFailureClass },
    Cancelled,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerEnvelope {
    version: u32,
    capability: String,
    transition: ServerTransition,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ServerTransition {
    Lead {
        claim_contended: bool,
    },
    Wait,
    Ack,
    Resume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidate: Option<CandidateResult>,
    },
    Failed {
        class: BrokerFailureClass,
    },
}

enum ActorEvent {
    BeginView {
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    FinishView {
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    AbortView {
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Claim {
        session: u64,
        view: u32,
        key: CandidateFlightKey,
        follower_resume: mpsc::SyncSender<Result<CandidateResult, BrokerFailureClass>>,
        reply: mpsc::SyncSender<Result<ClaimDisposition, String>>,
    },
    YieldLeader {
        session: u64,
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    ResumeLeader {
        session: u64,
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Complete {
        session: u64,
        candidate: CandidateResult,
        claim: crate::cache::cas::NativeExecutionClaim,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Failed {
        session: u64,
        class: BrokerFailureClass,
        reply: Option<mpsc::SyncSender<Result<(), String>>>,
    },
    Shutdown {
        reply: mpsc::SyncSender<Result<(), String>>,
    },
}

#[derive(Clone, Copy)]
enum ClaimDisposition {
    Lead,
    Wait,
    Reject,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PermitState {
    Held,
    Yielded,
}

struct Flight {
    leader: u64,
    leader_view: u32,
    outcome: FlightOutcome,
    followers: Vec<PendingFollower>,
}

enum ExecutionClaimGuard {
    Held {
        _claim: crate::cache::cas::NativeExecutionClaim,
    },
    #[cfg(test)]
    Simulated,
}

enum FlightOutcome {
    Running,
    Completed {
        candidate: CandidateResult,
        _claim: ExecutionClaimGuard,
    },
    Failed(BrokerFailureClass),
}

struct PendingFollower {
    session: u64,
    view: u32,
    resume: mpsc::SyncSender<Result<CandidateResult, BrokerFailureClass>>,
}

struct PendingFollowerResume {
    follower: PendingFollower,
    outcome: Result<CandidateResult, BrokerFailureClass>,
}

struct PendingStart {
    view: u32,
    reply: mpsc::SyncSender<Result<(), String>>,
}

enum PendingPermit {
    Leader {
        session: u64,
        view: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Follower(PendingFollowerResume),
}

impl PendingPermit {
    fn view(&self) -> u32 {
        match self {
            Self::Leader { view, .. } => *view,
            Self::Follower(pending) => pending.follower.view,
        }
    }

    fn cancel(&self) {
        match self {
            Self::Leader { reply, .. } => {
                drop(reply.send(Err("compiler acquisition view was cancelled".to_string())));
            }
            Self::Follower(pending) => {
                drop(pending.follower.resume.send(Err(BrokerFailureClass::Cancelled)));
            }
        }
    }
}

struct ActorState {
    capacity: usize,
    available: usize,
    views: FxHashMap<u32, PermitState>,
    flights: FxHashMap<CandidateFlightKey, Flight>,
    leaders: FxHashMap<u64, CandidateFlightKey>,
    pending: VecDeque<PendingPermit>,
    pending_starts: VecDeque<PendingStart>,
}

impl ActorState {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            available: capacity,
            views: FxHashMap::default(),
            flights: FxHashMap::default(),
            leaders: FxHashMap::default(),
            pending: VecDeque::new(),
            pending_starts: VecDeque::new(),
        }
    }

    fn request_begin_view(&mut self, view: u32, reply: mpsc::SyncSender<Result<(), String>>) {
        if self.views.contains_key(&view) || self.pending_starts.iter().any(|pending| pending.view == view) {
            drop(reply.send(Err("compiler acquisition work permit is duplicated".to_string())));
            return;
        }
        if self.available == 0 {
            crate::instrumentation::record_compiler_acquisition_work_permit_wait();
            self.pending_starts.push_back(PendingStart { view, reply });
            debug_assert!(self.validate().is_ok());
            return;
        }
        drop(reply.send(self.begin_view(view)));
    }

    fn begin_view(&mut self, view: u32) -> Result<(), String> {
        if self.views.contains_key(&view) || self.available == 0 {
            return Err("compiler acquisition work permit is unavailable or duplicated".to_string());
        }
        self.available -= 1;
        self.views.insert(view, PermitState::Held);
        self.validate()
    }

    fn finish_view(&mut self, view: u32) -> Result<(), String> {
        if self.views.get(&view) != Some(&PermitState::Held) {
            return Err("terminal compiler acquisition view has no held work permit".to_string());
        }
        if self
            .flights
            .values()
            .any(|flight| flight.leader_view == view && matches!(flight.outcome, FlightOutcome::Running))
        {
            return Err("terminal compiler acquisition view has an incomplete leader flight".to_string());
        }
        if self
            .flights
            .values()
            .any(|flight| flight.followers.iter().any(|follower| follower.view == view))
            || self.pending.iter().any(|pending| pending.view() == view)
        {
            return Err("terminal compiler acquisition view still has a waiting follower".to_string());
        }
        let completed = self
            .flights
            .iter()
            .filter_map(|(key, flight)| (flight.leader_view == view).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in completed {
            let flight = self
                .flights
                .remove(&key)
                .ok_or_else(|| "completed compiler acquisition flight disappeared".to_string())?;
            let outcome = match flight.outcome {
                FlightOutcome::Completed { candidate, .. } => Ok(candidate),
                FlightOutcome::Failed(class) => Err(class),
                FlightOutcome::Running => {
                    return Err("completed compiler acquisition outcome disappeared".to_string());
                }
            };
            self.queue_followers(flight.followers, outcome);
        }
        self.views.remove(&view);
        self.available += 1;
        self.resume_pending();
        self.validate()
    }

    fn abort_view(&mut self, view: u32) -> Result<(), String> {
        let state = self
            .views
            .remove(&view)
            .ok_or_else(|| "compiler acquisition view was not registered".to_string())?;
        if state == PermitState::Held {
            self.available += 1;
        }

        let led = self
            .flights
            .iter()
            .filter_map(|(key, flight)| (flight.leader_view == view).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in led {
            if let Some(flight) = self.flights.remove(&key) {
                self.leaders.remove(&flight.leader);
                self.queue_followers(flight.followers, Err(BrokerFailureClass::Cancelled));
            }
        }
        for flight in self.flights.values_mut() {
            flight.followers.retain(|follower| {
                if follower.view == view {
                    drop(follower.resume.send(Err(BrokerFailureClass::Cancelled)));
                    false
                } else {
                    true
                }
            });
        }
        self.pending.retain(|pending| {
            if pending.view() == view {
                pending.cancel();
                false
            } else {
                true
            }
        });
        self.resume_pending();
        self.validate()
    }

    fn claim(&mut self, session: u64, view: u32, key: CandidateFlightKey) -> Result<ClaimDisposition, String> {
        if self.views.get(&view) != Some(&PermitState::Held) {
            return Err("compiler acquisition claim has no held work permit".to_string());
        }
        if let Some(flight) = self.flights.get(&key) {
            return Ok(if flight.leader_view == view {
                ClaimDisposition::Reject
            } else {
                ClaimDisposition::Wait
            });
        }
        self.flights.insert(
            key.clone(),
            Flight {
                leader: session,
                leader_view: view,
                outcome: FlightOutcome::Running,
                followers: Vec::new(),
            },
        );
        if self.leaders.insert(session, key).is_some() {
            return Err("compiler acquisition session already leads a flight".to_string());
        }
        self.validate()?;
        Ok(ClaimDisposition::Lead)
    }

    fn yield_leader(&mut self, session: u64, view: u32) -> Result<(), String> {
        let key = self
            .leaders
            .get(&session)
            .ok_or_else(|| "compiler acquisition yield has no leader flight".to_string())?;
        let flight = self
            .flights
            .get(key)
            .ok_or_else(|| "compiler acquisition leader flight disappeared".to_string())?;
        if flight.leader_view != view {
            return Err("compiler acquisition leader changed views".to_string());
        }
        self.yield_view(view)
    }

    fn resume_leader(&mut self, session: u64, view: u32, reply: mpsc::SyncSender<Result<(), String>>) {
        let result = self.validate_leader_resume(session, view);
        if let Err(error) = result {
            drop(reply.send(Err(error)));
            return;
        }
        if self
            .pending
            .iter()
            .any(|pending| matches!(pending, PendingPermit::Leader { session: pending, .. } if *pending == session))
        {
            drop(reply.send(Err("compiler acquisition leader resume is already pending".to_string())));
            return;
        }
        if self.available == 0 {
            self.pending.push_back(PendingPermit::Leader { session, view, reply });
            debug_assert!(self.validate().is_ok());
            return;
        }
        let result = self.resume_view(view);
        drop(reply.send(result));
    }

    fn validate_leader_resume(&self, session: u64, view: u32) -> Result<(), String> {
        let key = self
            .leaders
            .get(&session)
            .ok_or_else(|| "compiler acquisition resume has no leader flight".to_string())?;
        let flight = self
            .flights
            .get(key)
            .ok_or_else(|| "compiler acquisition leader flight disappeared".to_string())?;
        if flight.leader_view != view {
            return Err("compiler acquisition leader changed views".to_string());
        }
        if self.views.get(&view) != Some(&PermitState::Yielded) {
            return Err("compiler acquisition leader resumed without yielding its work permit".to_string());
        }
        Ok(())
    }

    fn yield_follower(
        &mut self,
        session: u64,
        view: u32,
        key: CandidateFlightKey,
        resume: mpsc::SyncSender<Result<CandidateResult, BrokerFailureClass>>,
    ) -> Result<(), String> {
        let follower = PendingFollower { session, view, resume };
        let immediate = {
            let flight = self
                .flights
                .get_mut(&key)
                .ok_or_else(|| "compiler acquisition follower flight disappeared".to_string())?;
            if flight.leader == session || flight.followers.iter().any(|follower| follower.session == session) {
                return Err("compiler acquisition follower session is duplicated".to_string());
            }
            match &flight.outcome {
                FlightOutcome::Running => {
                    flight.followers.push(follower);
                    None
                }
                FlightOutcome::Completed { candidate, .. } => Some(PendingFollowerResume {
                    follower,
                    outcome: Ok(candidate.clone()),
                }),
                FlightOutcome::Failed(class) => Some(PendingFollowerResume {
                    follower,
                    outcome: Err(*class),
                }),
            }
        };
        self.yield_view(view)?;
        if let Some(immediate) = immediate {
            self.pending.push_back(PendingPermit::Follower(immediate));
            self.resume_pending();
        }
        self.validate()
    }

    fn complete(&mut self, session: u64, candidate: CandidateResult, claim: ExecutionClaimGuard) -> Result<(), String> {
        let key = self
            .leaders
            .get(&session)
            .cloned()
            .ok_or_else(|| "compiler acquisition completion has no leader flight".to_string())?;
        if let Err(error) = key.validate_candidate(&candidate) {
            drop(self.fail(session, BrokerFailureClass::Protocol));
            return Err(error.to_string());
        }
        self.leaders.remove(&session);
        let flight = self
            .flights
            .get_mut(&key)
            .ok_or_else(|| "compiler acquisition completed flight disappeared".to_string())?;
        if flight.leader != session {
            return Err("compiler acquisition completion came from a follower".to_string());
        }
        if !matches!(flight.outcome, FlightOutcome::Running) {
            return Err("compiler acquisition leader completed twice".to_string());
        }
        flight.outcome = FlightOutcome::Completed {
            candidate,
            _claim: claim,
        };
        self.validate()
    }

    fn fail(&mut self, session: u64, class: BrokerFailureClass) -> Result<(), String> {
        let key = self
            .leaders
            .remove(&session)
            .ok_or_else(|| "compiler acquisition failure has no leader flight".to_string())?;
        let flight = self
            .flights
            .get_mut(&key)
            .ok_or_else(|| "failed compiler acquisition flight disappeared".to_string())?;
        if flight.leader != session {
            return Err("compiler acquisition failure came from a follower".to_string());
        }
        if !matches!(flight.outcome, FlightOutcome::Running) {
            return Err("compiler acquisition leader failed after a terminal outcome".to_string());
        }
        flight.outcome = FlightOutcome::Failed(class);
        let followers = std::mem::take(&mut flight.followers);
        self.queue_followers(followers, Err(class));
        self.resume_pending();
        self.validate()
    }

    fn queue_followers(
        &mut self,
        followers: Vec<PendingFollower>,
        outcome: Result<CandidateResult, BrokerFailureClass>,
    ) {
        self.pending.extend(followers.into_iter().map(|follower| {
            PendingPermit::Follower(PendingFollowerResume {
                follower,
                outcome: outcome.clone(),
            })
        }));
    }

    fn yield_view(&mut self, view: u32) -> Result<(), String> {
        let state = self
            .views
            .get_mut(&view)
            .ok_or_else(|| "compiler acquisition view is not registered".to_string())?;
        if *state != PermitState::Held {
            return Err("compiler acquisition work permit was yielded twice".to_string());
        }
        *state = PermitState::Yielded;
        self.available += 1;
        crate::instrumentation::record_compiler_acquisition_work_permit_yield();
        self.resume_pending();
        self.validate()
    }

    fn resume_view(&mut self, view: u32) -> Result<(), String> {
        if self.available == 0 {
            return Err("compiler acquisition work permit cannot resume yet".to_string());
        }
        let state = self
            .views
            .get_mut(&view)
            .ok_or_else(|| "compiler acquisition view is not registered".to_string())?;
        if *state != PermitState::Yielded {
            return Err("compiler acquisition work permit resumed without a yield".to_string());
        }
        *state = PermitState::Held;
        self.available -= 1;
        crate::instrumentation::record_compiler_acquisition_work_permit_resume();
        self.validate()
    }

    fn resume_pending(&mut self) {
        while self.available > 0 {
            if let Some(pending) = self.pending.pop_front() {
                match pending {
                    PendingPermit::Leader { session, view, reply } => {
                        if let Err(error) = self.validate_leader_resume(session, view) {
                            drop(reply.send(Err(error)));
                            continue;
                        }
                        if let Err(error) = self.resume_view(view) {
                            drop(reply.send(Err(error)));
                            continue;
                        }
                        if reply.send(Ok(())).is_err() {
                            self.revert_failed_resume(view);
                        }
                    }
                    PendingPermit::Follower(pending) => {
                        if self.resume_view(pending.follower.view).is_err() {
                            drop(pending.follower.resume.send(Err(BrokerFailureClass::Protocol)));
                            continue;
                        }
                        if pending.follower.resume.send(pending.outcome).is_err() {
                            self.revert_failed_resume(pending.follower.view);
                        }
                    }
                }
                continue;
            }
            let Some(pending) = self.pending_starts.pop_front() else {
                break;
            };
            let result = self.begin_view(pending.view);
            let started = result.is_ok();
            if pending.reply.send(result).is_err() && started {
                drop(self.abort_view(pending.view));
            }
        }
    }

    fn revert_failed_resume(&mut self, view: u32) {
        if let Some(state) = self.views.get_mut(&view)
            && *state == PermitState::Held
        {
            *state = PermitState::Yielded;
            self.available += 1;
        }
    }

    fn validate(&self) -> Result<(), String> {
        let held = self.views.values().filter(|state| **state == PermitState::Held).count();
        crate::instrumentation::record_compiler_acquisition_nonwaiting_views(held);
        if held.checked_add(self.available) != Some(self.capacity) || self.available > self.capacity {
            return Err("compiler acquisition work-permit ledger is inconsistent".to_string());
        }
        for (key, flight) in &self.flights {
            key.validate().map_err(|error| error.to_string())?;
            match &flight.outcome {
                FlightOutcome::Completed { candidate, .. } => {
                    key.validate_candidate(candidate).map_err(|error| error.to_string())?;
                    if self.leaders.contains_key(&flight.leader) {
                        return Err("completed compiler acquisition flight retained a live leader".to_string());
                    }
                }
                FlightOutcome::Failed(_) if !self.leaders.contains_key(&flight.leader) => {}
                FlightOutcome::Running if self.leaders.get(&flight.leader) == Some(key) => {}
                FlightOutcome::Failed(_) | FlightOutcome::Running => {
                    return Err("live compiler acquisition flight lost its leader".to_string());
                }
            }
            if !self.views.contains_key(&flight.leader_view)
                || flight
                    .followers
                    .iter()
                    .any(|follower| self.views.get(&follower.view) != Some(&PermitState::Yielded))
            {
                return Err("compiler acquisition flight references an invalid view state".to_string());
            }
        }
        if self.leaders.iter().any(|(session, key)| {
            self.flights
                .get(key)
                .is_none_or(|flight| flight.leader != *session || !matches!(flight.outcome, FlightOutcome::Running))
        }) || self
            .pending
            .iter()
            .any(|pending| self.views.get(&pending.view()) != Some(&PermitState::Yielded))
            || self
                .pending_starts
                .iter()
                .any(|pending| self.views.contains_key(&pending.view))
            || self
                .pending_starts
                .iter()
                .map(|pending| pending.view)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.pending_starts.len()
        {
            return Err("compiler acquisition actor references inconsistent live state".to_string());
        }
        Ok(())
    }
}

fn run_actor(receiver: mpsc::Receiver<ActorEvent>, work_permits: usize) -> RailResult<()> {
    let mut state = ActorState::new(work_permits);
    while let Ok(event) = receiver.recv() {
        match event {
            ActorEvent::BeginView { view, reply } => state.request_begin_view(view, reply),
            ActorEvent::FinishView { view, reply } => drop(reply.send(state.finish_view(view))),
            ActorEvent::AbortView { view, reply } => drop(reply.send(state.abort_view(view))),
            ActorEvent::Claim {
                session,
                view,
                key,
                follower_resume,
                reply,
            } => {
                let disposition = state.claim(session, view, key.clone()).and_then(|disposition| {
                    if matches!(disposition, ClaimDisposition::Wait) {
                        state.yield_follower(session, view, key, follower_resume)?;
                    }
                    Ok(disposition)
                });
                drop(reply.send(disposition));
            }
            ActorEvent::YieldLeader { session, view, reply } => drop(reply.send(state.yield_leader(session, view))),
            ActorEvent::ResumeLeader { session, view, reply } => state.resume_leader(session, view, reply),
            ActorEvent::Complete {
                session,
                candidate,
                claim,
                reply,
            } => drop(reply.send(state.complete(session, candidate, ExecutionClaimGuard::Held { _claim: claim }))),
            ActorEvent::Failed { session, class, reply } => {
                let result = state.fail(session, class);
                if let Some(reply) = reply {
                    drop(reply.send(result));
                }
            }
            ActorEvent::Shutdown { reply } => {
                let result = if state.views.is_empty()
                    && state.flights.is_empty()
                    && state.leaders.is_empty()
                    && state.pending.is_empty()
                    && state.pending_starts.is_empty()
                    && state.available == state.capacity
                {
                    Ok(())
                } else {
                    Err("compiler acquisition broker shut down with live state".to_string())
                };
                drop(reply.send(result.clone()));
                return result.map_err(RailError::message);
            }
        }
    }
    Err(RailError::message(
        "compiler acquisition broker event channel closed before shutdown",
    ))
}

fn run_accept_loop(
    mut listener: BrokerListener,
    accepted: mpsc::SyncSender<BrokerStream>,
    shutdown: &AtomicBool,
) -> RailResult<()> {
    loop {
        let mut stream = listener.accept()?;
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        stream.enter_handshake()?;
        accepted
            .send(stream)
            .map_err(|_| RailError::message("compiler acquisition broker workers stopped"))?;
    }
}

fn run_connection_worker(
    accepted: Arc<Mutex<mpsc::Receiver<BrokerStream>>>,
    event_tx: mpsc::SyncSender<ActorEvent>,
    capability: &str,
    sessions: &AtomicU64,
    cas: &crate::cache::cas::LocalCas,
) -> RailResult<()> {
    loop {
        let stream = accepted
            .lock()
            .map_err(|_| RailError::message("compiler acquisition broker connection queue was poisoned"))?
            .recv();
        let Ok(mut stream) = stream else {
            return Ok(());
        };
        let session = sessions
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |session| session.checked_add(1))
            .map_err(|_| RailError::message("compiler acquisition broker session space is exhausted"))?;
        drop(handle_connection(&mut stream, &event_tx, capability, session, cas));
    }
}

fn handle_connection(
    stream: &mut BrokerStream,
    event_tx: &mpsc::SyncSender<ActorEvent>,
    capability: &str,
    session: u64,
    cas: &crate::cache::cas::LocalCas,
) -> RailResult<()> {
    let first = read_client_transition(stream, capability)?;
    let ClientTransition::Claim { view, key } = first else {
        return Err(RailError::message(
            "compiler acquisition broker session did not begin with a claim",
        ));
    };
    key.validate()?;
    let (follower_resume, resumed) = mpsc::sync_channel(1);
    let (reply, response) = mpsc::sync_channel(1);
    event_tx
        .send(ActorEvent::Claim {
            session,
            view,
            key: key.clone(),
            follower_resume,
            reply,
        })
        .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped"))?;
    let disposition = actor_result(response.recv(), "claiming compiler acquisition flight")?;
    match disposition {
        ClaimDisposition::Lead => {
            let identity = key.claim_identity()?;
            let (claim, claim_contended) = match cas.try_native_execution_claim(&identity) {
                Ok(crate::cache::cas::NativeExecutionClaimAttempt::Acquired(claim)) => (Some(claim), false),
                Ok(crate::cache::cas::NativeExecutionClaimAttempt::Contended) => (None, true),
                Err(_) => {
                    send_unit_event(
                        event_tx,
                        |reply| ActorEvent::Failed {
                            session,
                            class: BrokerFailureClass::ExecutionClaim,
                            reply: Some(reply),
                        },
                        "failing unavailable compiler execution claim",
                    )?;
                    return write_server_transition(
                        stream,
                        capability,
                        ServerTransition::Failed {
                            class: BrokerFailureClass::ExecutionClaim,
                        },
                    );
                }
            };
            if let Err(error) = write_server_transition(stream, capability, ServerTransition::Lead { claim_contended })
                .and_then(|()| stream.enter_live())
            {
                drop(event_tx.send(ActorEvent::Failed {
                    session,
                    class: BrokerFailureClass::Protocol,
                    reply: None,
                }));
                return Err(error);
            }
            let claim = match claim {
                Some(claim) => claim,
                None => {
                    let Some(claim) =
                        acquire_contended_execution_claim(stream, event_tx, capability, session, view, cas, &identity)?
                    else {
                        return Ok(());
                    };
                    claim
                }
            };
            handle_leader(stream, event_tx, capability, session, claim)
        }
        ClaimDisposition::Wait => {
            // The actor registered the follower and yielded its view in the
            // same event that selected Wait, before this disposition became
            // observable to either participant.
            write_server_transition(stream, capability, ServerTransition::Wait)?;
            stream.enter_live()?;
            handle_follower(stream, capability, resumed)
        }
        ClaimDisposition::Reject => write_server_transition(
            stream,
            capability,
            ServerTransition::Failed {
                class: BrokerFailureClass::Protocol,
            },
        ),
    }
}

fn acquire_contended_execution_claim(
    stream: &mut BrokerStream,
    event_tx: &mpsc::SyncSender<ActorEvent>,
    capability: &str,
    session: u64,
    view: u32,
    cas: &crate::cache::cas::LocalCas,
    identity: &ContentDigest,
) -> RailResult<Option<crate::cache::cas::NativeExecutionClaim>> {
    if !matches!(read_client_transition(stream, capability)?, ClientTransition::Yielded) {
        drop(event_tx.send(ActorEvent::Failed {
            session,
            class: BrokerFailureClass::Protocol,
            reply: None,
        }));
        return Err(RailError::message(
            "contended compiler acquisition leader did not yield its work permit",
        ));
    }
    send_unit_event(
        event_tx,
        |reply| ActorEvent::YieldLeader { session, view, reply },
        "yielding contended compiler acquisition leader",
    )?;
    write_server_transition(stream, capability, ServerTransition::Ack)?;

    let claim = cas.native_execution_claim(identity);
    send_unit_event(
        event_tx,
        |reply| ActorEvent::ResumeLeader { session, view, reply },
        "resuming contended compiler acquisition leader",
    )?;
    match claim {
        Ok(claim) => {
            write_server_transition(stream, capability, ServerTransition::Resume { candidate: None })?;
            Ok(Some(claim))
        }
        Err(_) => {
            send_unit_event(
                event_tx,
                |reply| ActorEvent::Failed {
                    session,
                    class: BrokerFailureClass::ExecutionClaim,
                    reply: Some(reply),
                },
                "failing contended compiler execution claim",
            )?;
            write_server_transition(
                stream,
                capability,
                ServerTransition::Failed {
                    class: BrokerFailureClass::ExecutionClaim,
                },
            )?;
            Ok(None)
        }
    }
}

fn handle_leader(
    stream: &mut BrokerStream,
    event_tx: &mpsc::SyncSender<ActorEvent>,
    capability: &str,
    session: u64,
    claim: crate::cache::cas::NativeExecutionClaim,
) -> RailResult<()> {
    match read_client_transition(stream, capability) {
        Ok(ClientTransition::Complete { candidate }) => {
            candidate.validate()?;
            send_unit_event(
                event_tx,
                |reply| ActorEvent::Complete {
                    session,
                    candidate,
                    claim,
                    reply,
                },
                "completing compiler acquisition leader",
            )?;
            write_server_transition(stream, capability, ServerTransition::Ack)
        }
        Ok(ClientTransition::Failed { class }) => {
            send_unit_event(
                event_tx,
                |reply| ActorEvent::Failed {
                    session,
                    class,
                    reply: Some(reply),
                },
                "failing compiler acquisition leader",
            )?;
            write_server_transition(stream, capability, ServerTransition::Ack)
        }
        Ok(ClientTransition::Cancelled) => {
            send_unit_event(
                event_tx,
                |reply| ActorEvent::Failed {
                    session,
                    class: BrokerFailureClass::Cancelled,
                    reply: Some(reply),
                },
                "cancelling compiler acquisition leader",
            )?;
            write_server_transition(stream, capability, ServerTransition::Ack)
        }
        Ok(ClientTransition::Claim { .. } | ClientTransition::Yielded) => {
            drop(event_tx.send(ActorEvent::Failed {
                session,
                class: BrokerFailureClass::Protocol,
                reply: None,
            }));
            Err(RailError::message(
                "compiler acquisition leader sent an invalid transition",
            ))
        }
        Err(error) => {
            drop(event_tx.send(ActorEvent::Failed {
                session,
                class: BrokerFailureClass::Protocol,
                reply: None,
            }));
            Err(error)
        }
    }
}

fn handle_follower(
    stream: &mut BrokerStream,
    capability: &str,
    resumed: mpsc::Receiver<Result<CandidateResult, BrokerFailureClass>>,
) -> RailResult<()> {
    match resumed
        .recv()
        .map_err(|_| RailError::message("compiler acquisition follower resume disappeared"))?
    {
        Ok(candidate) => write_server_transition(
            stream,
            capability,
            ServerTransition::Resume {
                candidate: Some(candidate),
            },
        ),
        Err(class) => write_server_transition(stream, capability, ServerTransition::Failed { class }),
    }
}

fn send_unit_event(
    event_tx: &mpsc::SyncSender<ActorEvent>,
    event: impl FnOnce(mpsc::SyncSender<Result<(), String>>) -> ActorEvent,
    context: &str,
) -> RailResult<()> {
    let (reply, response) = mpsc::sync_channel(1);
    event_tx
        .send(event(reply))
        .map_err(|_| RailError::message("compiler acquisition broker coordinator stopped"))?;
    actor_result(response.recv(), context)
}

fn actor_result<T>(received: Result<Result<T, String>, mpsc::RecvError>, context: &str) -> RailResult<T> {
    received
        .map_err(|_| RailError::message(format!("{context}: coordinator response disappeared")))?
        .map_err(|error| RailError::message(format!("{context}: {error}")))
}

fn write_transition(stream: &mut BrokerStream, capability: &str, transition: ClientTransition) -> RailResult<()> {
    write_frame(
        stream,
        &ClientEnvelope {
            version: BROKER_PROTOCOL_VERSION,
            capability: capability.to_string(),
            transition,
        },
    )
}

fn write_server_transition(
    stream: &mut BrokerStream,
    capability: &str,
    transition: ServerTransition,
) -> RailResult<()> {
    write_frame(
        stream,
        &ServerEnvelope {
            version: BROKER_PROTOCOL_VERSION,
            capability: capability.to_string(),
            transition,
        },
    )
}

fn read_client_transition(stream: &mut BrokerStream, capability: &str) -> RailResult<ClientTransition> {
    let envelope: ClientEnvelope = read_frame(stream)?;
    if envelope.version != BROKER_PROTOCOL_VERSION || envelope.capability != capability {
        return Err(RailError::message(
            "compiler acquisition broker rejected the client protocol or capability",
        ));
    }
    Ok(envelope.transition)
}

fn read_server_transition(stream: &mut BrokerStream, capability: &str) -> RailResult<ServerTransition> {
    let envelope: ServerEnvelope = read_frame(stream)?;
    if envelope.version != BROKER_PROTOCOL_VERSION || envelope.capability != capability {
        return Err(RailError::message(
            "compiler acquisition broker rejected the server protocol or capability",
        ));
    }
    Ok(envelope.transition)
}

fn expect_ack(stream: &mut BrokerStream, capability: &str, context: &str) -> RailResult<()> {
    match read_server_transition(stream, capability)? {
        ServerTransition::Ack => Ok(()),
        ServerTransition::Failed { class } => Err(broker_failure(class)),
        ServerTransition::Lead { .. } | ServerTransition::Wait | ServerTransition::Resume { .. } => {
            Err(RailError::message(format!(
                "{context}: compiler acquisition broker did not acknowledge the transition"
            )))
        }
    }
}

fn write_frame<W: Write, T: Serialize>(stream: &mut W, value: &T) -> RailResult<()> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.is_empty() || encoded.len() > MAX_FRAME_BYTES {
        return Err(RailError::message(
            "compiler acquisition broker frame exceeds its bound",
        ));
    }
    let length = u32::try_from(encoded.len())
        .map_err(|_| RailError::message("compiler acquisition broker frame length exceeds 32 bits"))?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&encoded)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>, R: Read>(stream: &mut R) -> RailResult<T> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| RailError::message("compiler acquisition broker frame length exceeds usize"))?;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(RailError::message(
            "compiler acquisition broker frame exceeds its bound",
        ));
    }
    let mut encoded = vec![0_u8; length];
    stream.read_exact(&mut encoded)?;
    Ok(serde_json::from_slice(&encoded)?)
}

fn broker_failure(class: BrokerFailureClass) -> RailError {
    RailError::message(format!(
        "compiler acquisition broker flight failed: {}",
        match class {
            BrokerFailureClass::Cancelled => "cancelled",
            BrokerFailureClass::Compiler => "compiler",
            BrokerFailureClass::Evidence => "evidence",
            BrokerFailureClass::ExecutionClaim => "execution_claim",
            BrokerFailureClass::Protocol => "protocol",
            BrokerFailureClass::Publication => "publication",
        }
    ))
}

fn random_hex() -> RailResult<String> {
    let mut bytes = [0_u8; CAPABILITY_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        RailError::message(format!(
            "failed to create compiler acquisition broker capability: {error}"
        ))
    })?;
    let mut encoded = String::with_capacity(CAPABILITY_HEX_BYTES);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn validate_capability(capability: &str) -> RailResult<()> {
    if capability.len() != CAPABILITY_HEX_BYTES
        || !capability
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(
            "compiler acquisition broker capability is not canonical",
        ));
    }
    Ok(())
}

fn validate_identity(value: &str, prefix: &str, description: &str) -> RailResult<()> {
    let valid = value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(RailError::message(format!(
            "compiler acquisition {description} is not canonical"
        )))
    }
}

#[cfg(unix)]
fn validate_endpoint(endpoint: &str) -> RailResult<()> {
    let path = std::path::Path::new(endpoint);
    if !path.is_absolute() || endpoint.is_empty() || endpoint.len() > 4096 || endpoint.as_bytes().contains(&0) {
        return Err(RailError::message(
            "compiler acquisition Unix broker endpoint is invalid",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_endpoint(endpoint: &str) -> RailResult<()> {
    let suffix = endpoint.strip_prefix(r"\\.\pipe\cargo-rail-acquisition-");
    if !suffix.is_some_and(|suffix| {
        suffix.len() == CAPABILITY_HEX_BYTES
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(RailError::message(
            "compiler acquisition Windows broker endpoint is invalid",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_endpoint(_endpoint: &str) -> RailResult<()> {
    Err(RailError::message(
        "compiler acquisition broker is unavailable on this host",
    ))
}

fn join_thread(thread: JoinHandle<RailResult<()>>, description: &str) -> RailResult<()> {
    let result = thread
        .join()
        .map_err(|_| RailError::message(format!("{description} panicked")))?;
    result.map_err(|error| RailError::message(format!("{description} failed: {error}")))
}

#[cfg(unix)]
struct BrokerListener(std::os::unix::net::UnixListener);

#[cfg(unix)]
impl BrokerListener {
    fn bind(endpoint: &str, _max_connections: usize) -> RailResult<Self> {
        validate_endpoint(endpoint)?;
        Ok(Self(std::os::unix::net::UnixListener::bind(endpoint)?))
    }

    fn accept(&mut self) -> RailResult<BrokerStream> {
        let (stream, _) = self.0.accept()?;
        Ok(BrokerStream(stream, None))
    }
}

#[cfg(unix)]
struct BrokerStream(std::os::unix::net::UnixStream, Option<Instant>);

#[cfg(unix)]
impl BrokerStream {
    fn connect(endpoint: &str) -> RailResult<Self> {
        validate_endpoint(endpoint)?;
        let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
        let mut stream = Self(stream, None);
        stream.enter_handshake()?;
        Ok(stream)
    }

    fn enter_handshake(&mut self) -> RailResult<()> {
        self.1 = Some(Instant::now() + IO_TIMEOUT);
        self.0.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(())
    }

    fn enter_live(&mut self) -> RailResult<()> {
        self.1 = None;
        self.0.set_read_timeout(None)?;
        Ok(())
    }
}

#[cfg(windows)]
struct BrokerListener(crate::windows_fs::LocalNamedPipeListener);

#[cfg(windows)]
impl BrokerListener {
    fn bind(endpoint: &str, max_connections: usize) -> RailResult<Self> {
        validate_endpoint(endpoint)?;
        Ok(Self(crate::windows_fs::LocalNamedPipeListener::bind(
            endpoint,
            max_connections,
        )?))
    }

    fn accept(&mut self) -> RailResult<BrokerStream> {
        Ok(BrokerStream(self.0.accept()?, None))
    }
}

#[cfg(windows)]
struct BrokerStream(crate::windows_fs::LocalNamedPipe, Option<Instant>);

#[cfg(windows)]
impl BrokerStream {
    fn connect(endpoint: &str) -> RailResult<Self> {
        validate_endpoint(endpoint)?;
        Ok(Self(
            crate::windows_fs::connect_local_named_pipe(endpoint, IO_TIMEOUT)?,
            Some(Instant::now() + IO_TIMEOUT),
        ))
    }

    fn enter_handshake(&mut self) -> RailResult<()> {
        self.1 = Some(Instant::now() + IO_TIMEOUT);
        Ok(())
    }

    fn enter_live(&mut self) -> RailResult<()> {
        self.1 = None;
        Ok(())
    }
}

#[cfg(unix)]
impl Read for BrokerStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if let Some(deadline) = self.1 {
            self.0.set_read_timeout(Some(remaining_handshake_timeout(deadline)?))?;
        }
        self.0.read(buffer)
    }
}

#[cfg(windows)]
impl Read for BrokerStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .read_with_timeout(buffer, self.1.map(remaining_handshake_timeout).transpose()?)
    }
}

fn remaining_handshake_timeout(deadline: Instant) -> std::io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "compiler acquisition broker handshake timed out",
        ));
    }
    Ok(remaining)
}

#[cfg(unix)]
impl Write for BrokerStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(windows)]
impl Write for BrokerStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write_with_timeout(buffer, IO_TIMEOUT)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wake_listener(endpoint: &str) {
    if let Ok(mut stream) = BrokerStream::connect(endpoint) {
        drop(stream.write_all(&0_u32.to_le_bytes()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(prefix: &str, byte: char) -> String {
        format!("{prefix}{}", byte.to_string().repeat(64))
    }

    fn flight(byte: char) -> CandidateFlightKey {
        CandidateFlightKey::new(
            ExecutionClaimKey::Selected(identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, byte)),
            identity(crate::compiler::analysis::ANALYSIS_CONTRACT_ID_PREFIX, 'a'),
        )
        .expect("flight key")
    }

    fn candidate_for(byte: char, contract_byte: char) -> CandidateResult {
        let action = identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, byte);
        let result = identity(crate::compiler::native_cache::RESULT_KEY_PREFIX, byte);
        let contract = identity(crate::compiler::analysis::ANALYSIS_CONTRACT_ID_PREFIX, contract_byte);
        CandidateResult::new(
            action.clone(),
            result.clone(),
            crate::compiler::diagnostics_store::NativeEvidenceBindingValidation::candidate_key(
                &action, &result, &contract,
            ),
        )
        .expect("candidate")
    }

    fn candidate(byte: char) -> CandidateResult {
        candidate_for(byte, 'a')
    }

    fn test_broker(work_permits: usize) -> (tempfile::TempDir, AcquisitionBroker) {
        let cache = tempfile::tempdir().expect("broker cache base");
        let cas = crate::cache::cas::LocalCas::open_at(cache.path(), 1024 * 1024).expect("broker CAS");
        let broker = AcquisitionBroker::start(work_permits, cas).expect("broker");
        (cache, broker)
    }

    #[test]
    fn malformed_and_oversized_frames_fail_before_allocation_or_transition() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            &u32::try_from(MAX_FRAME_BYTES + 1)
                .expect("test frame length")
                .to_le_bytes(),
        );
        let mut stream = std::io::Cursor::new(bytes);
        assert!(read_frame::<ClientEnvelope, _>(&mut stream).is_err());

        let mut stream = std::io::Cursor::new(1_u32.to_le_bytes().into_iter().chain(*b"{").collect::<Vec<_>>());
        assert!(read_frame::<ClientEnvelope, _>(&mut stream).is_err());
    }

    #[test]
    fn permit_ledger_yields_and_resumes_without_capacity_drift() {
        let mut state = ActorState::new(2);
        state.begin_view(1).expect("first permit");
        state.begin_view(2).expect("second permit");
        let key = flight('b');
        assert!(matches!(state.claim(10, 1, key.clone()), Ok(ClaimDisposition::Lead)));
        assert!(matches!(state.claim(11, 2, key.clone()), Ok(ClaimDisposition::Wait)));
        let (resume, resumed) = mpsc::sync_channel(1);
        state.yield_follower(11, 2, key, resume).expect("follower yield");
        state
            .complete(10, candidate('b'), ExecutionClaimGuard::Simulated)
            .expect("leader completion");
        assert!(matches!(resumed.try_recv(), Err(mpsc::TryRecvError::Empty)));
        state.finish_view(1).expect("first finish");
        assert_eq!(resumed.recv().expect("follower resume"), Ok(candidate('b')));
        state.finish_view(2).expect("second finish");
        assert_eq!(state.available, state.capacity);
    }

    #[test]
    fn authenticated_broker_coalesces_only_one_exact_flight() {
        let (_cache, broker) = test_broker(2);
        let first = broker.begin_view_index(0).expect("first view");
        let second = broker.begin_view_index(1).expect("second view");
        let key = flight('c');
        let BrokerClaim::Leader(leader) = BrokerClaim::connect(first.environment(), key.clone()).expect("leader claim")
        else {
            panic!("first claimant must lead");
        };
        let BrokerClaim::Follower(follower) = BrokerClaim::connect(second.environment(), key).expect("follower claim")
        else {
            panic!("second claimant must follow");
        };
        let expected = candidate('c');
        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || follower.wait().expect("follower wait").expect("follower candidate"));
            leader.complete(expected.clone()).expect("leader completion");
            first.finish().expect("first finish");
            assert_eq!(waiter.join().expect("follower thread"), expected);
        });
        second.finish().expect("second finish");
        broker.close().expect("broker close");
    }

    #[test]
    fn independent_commands_hold_the_execution_claim_through_view_publication() {
        let cache = tempfile::tempdir().expect("shared broker cache base");
        let cas = crate::cache::cas::LocalCas::open_at(cache.path(), 1024 * 1024).expect("shared broker CAS");
        let first_broker = AcquisitionBroker::start(1, cas.clone()).expect("first broker");
        let second_broker = AcquisitionBroker::start(1, cas).expect("second broker");
        let first_view = first_broker.begin_view_index(0).expect("first view");
        let second_view = second_broker.begin_view_index(0).expect("second view");
        let key = flight('d');

        let BrokerClaim::Leader(mut first) =
            BrokerClaim::connect(first_view.environment(), key.clone()).expect("first claim")
        else {
            panic!("first command must lead");
        };
        assert!(first.acquire_execution_claim().expect("first execution claim"));
        let BrokerClaim::Leader(mut second) =
            BrokerClaim::connect(second_view.environment(), key).expect("second claim")
        else {
            panic!("an independent command must lead its own command-local flight");
        };

        std::thread::scope(|scope| {
            let (acquired_tx, acquired_rx) = mpsc::channel();
            let waiter = scope.spawn(move || {
                let acquired = second.acquire_execution_claim().expect("contended execution claim");
                acquired_tx.send(acquired).expect("acquisition signal");
                second
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let probe_view = loop {
                match second_broker.begin_view_index(1) {
                    Ok(view) => break view,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("contended leader never yielded its permit: {error}"),
                }
            };

            first.complete(candidate('d')).expect("first completion");
            assert!(
                acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "native publication alone must not release the cross-command claim"
            );
            first_view.finish().expect("first binding publication boundary");
            assert!(
                acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "an acquired claimant must not continue without a work permit"
            );
            probe_view.finish().expect("release probe permit");
            assert!(
                acquired_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("second acquisition"),
                "the second command must acquire after binding publication"
            );
            let second = waiter.join().expect("second leader");
            second.complete(candidate('d')).expect("second completion");
        });

        second_view.finish().expect("second binding publication boundary");
        first_broker.close().expect("first broker close");
        second_broker.close().expect("second broker close");
    }

    #[test]
    fn disconnected_leader_cancels_followers_and_restores_the_permit_ledger() {
        let (_cache, broker) = test_broker(2);
        let first_view = broker.begin_view_index(0).expect("first view");
        let second_view = broker.begin_view_index(1).expect("second view");
        let key = flight('f');
        let BrokerClaim::Leader(mut leader) =
            BrokerClaim::connect(first_view.environment(), key.clone()).expect("leader")
        else {
            panic!("first claimant must lead");
        };
        assert!(leader.acquire_execution_claim().expect("execution claim"));
        let BrokerClaim::Follower(follower) = BrokerClaim::connect(second_view.environment(), key).expect("follower")
        else {
            panic!("second claimant must follow");
        };

        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || follower.wait());
            drop(leader);
            let error = waiter
                .join()
                .expect("follower thread")
                .expect_err("cancelled leader must not publish success");
            assert!(error.to_string().contains("cancelled"));
        });
        first_view.finish().expect("first finish");
        second_view.finish().expect("second finish");
        broker.close().expect("broker close");
    }

    #[test]
    fn same_view_duplicate_fails_closed_instead_of_deadlocking_its_cargo_process() {
        let mut state = ActorState::new(1);
        state.begin_view(1).expect("view permit");
        let key = flight('d');
        assert!(matches!(state.claim(20, 1, key.clone()), Ok(ClaimDisposition::Lead)));
        assert!(matches!(state.claim(21, 1, key), Ok(ClaimDisposition::Reject)));
        state.fail(20, BrokerFailureClass::Cancelled).expect("cancel leader");
        state.finish_view(1).expect("finish view");
        assert_eq!(state.available, state.capacity);
    }

    #[test]
    fn pending_view_starts_are_bounded_and_resume_in_request_order() {
        let mut state = ActorState::new(1);
        state.begin_view(1).expect("first view");
        let (second_reply, second) = mpsc::sync_channel(1);
        let (third_reply, third) = mpsc::sync_channel(1);
        state.request_begin_view(2, second_reply);
        state.request_begin_view(3, third_reply);
        second.try_recv().expect_err("second view must remain queued");
        third.try_recv().expect_err("third view must remain queued");

        state.finish_view(1).expect("finish first");
        assert_eq!(second.recv().expect("second reply"), Ok(()));
        third.try_recv().expect_err("third view must remain queued");
        state.finish_view(2).expect("finish second");
        assert_eq!(third.recv().expect("third reply"), Ok(()));
        state.finish_view(3).expect("finish third");
        assert_eq!(state.available, state.capacity);
    }

    #[test]
    fn divergent_analysis_contracts_never_share_a_flight() {
        let mut state = ActorState::new(2);
        state.begin_view(1).expect("first permit");
        state.begin_view(2).expect("second permit");
        let first = flight('e');
        let second = CandidateFlightKey::new(
            ExecutionClaimKey::Selected(identity(crate::compiler::native_cache::ACTION_KEY_PREFIX, 'e')),
            identity(crate::compiler::analysis::ANALYSIS_CONTRACT_ID_PREFIX, 'b'),
        )
        .expect("divergent flight");
        assert!(matches!(state.claim(30, 1, first), Ok(ClaimDisposition::Lead)));
        assert!(matches!(state.claim(31, 2, second), Ok(ClaimDisposition::Lead)));
        state
            .complete(30, candidate_for('e', 'a'), ExecutionClaimGuard::Simulated)
            .expect("first completion");
        state
            .complete(31, candidate_for('e', 'b'), ExecutionClaimGuard::Simulated)
            .expect("second completion");
        state.finish_view(1).expect("first finish");
        state.finish_view(2).expect("second finish");
    }

    #[test]
    fn leader_failure_resumes_waiters_without_leaking_permits() {
        let mut state = ActorState::new(2);
        state.begin_view(1).expect("first permit");
        state.begin_view(2).expect("second permit");
        let key = flight('f');
        assert!(matches!(state.claim(40, 1, key.clone()), Ok(ClaimDisposition::Lead)));
        assert!(matches!(state.claim(41, 2, key.clone()), Ok(ClaimDisposition::Wait)));
        let (resume, resumed) = mpsc::sync_channel(1);
        state.yield_follower(41, 2, key, resume).expect("follower yield");
        state.fail(40, BrokerFailureClass::Compiler).expect("leader failure");
        assert_eq!(
            resumed.recv().expect("follower failure"),
            Err(BrokerFailureClass::Compiler)
        );
        state.finish_view(1).expect("first finish");
        state.finish_view(2).expect("second finish");
        assert_eq!(state.available, state.capacity);
    }

    #[test]
    fn contended_leader_reacquires_the_next_available_work_permit() {
        let mut state = ActorState::new(1);
        state.begin_view(1).expect("first permit");
        assert!(matches!(state.claim(45, 1, flight('4')), Ok(ClaimDisposition::Lead)));
        state.yield_leader(45, 1).expect("leader yield");
        state.begin_view(2).expect("second view takes yielded permit");

        let (reply, resumed) = mpsc::sync_channel(1);
        state.resume_leader(45, 1, reply);
        assert!(matches!(resumed.try_recv(), Err(mpsc::TryRecvError::Empty)));
        state.finish_view(2).expect("second view finish");
        resumed.recv().expect("leader resume reply").expect("leader resume");

        state
            .fail(45, BrokerFailureClass::ExecutionClaim)
            .expect("detached leader");
        state.finish_view(1).expect("first view finish");
        assert_eq!(state.available, state.capacity);
    }

    #[test]
    fn invalid_completion_is_failed_closed_and_cannot_escape_the_flight_contract() {
        let mut state = ActorState::new(1);
        state.begin_view(1).expect("view permit");
        assert!(matches!(state.claim(50, 1, flight('1')), Ok(ClaimDisposition::Lead)));
        assert!(
            state
                .complete(50, candidate_for('1', 'b'), ExecutionClaimGuard::Simulated)
                .is_err()
        );
        assert_eq!(state.flights.len(), 1);
        assert!(state.leaders.is_empty());
        state.finish_view(1).expect("finish view");
        assert!(state.flights.is_empty());
    }

    #[test]
    fn invalid_capability_never_reaches_the_coordination_actor() {
        let (_cache, broker) = test_broker(1);
        let view = broker.begin_view_index(0).expect("view");
        let mut forged = view.environment().clone();
        forged
            .capability
            .replace_range(..1, if forged.capability.starts_with('0') { "1" } else { "0" });
        assert!(BrokerClaim::connect(&forged, flight('2')).is_err());
        view.finish().expect("finish view");
        broker.close().expect("broker close");
    }

    #[test]
    fn execution_claim_failure_detaches_a_follower_after_reacquiring_its_permit() {
        let (_cache, broker) = test_broker(2);
        let first = broker.begin_view_index(0).expect("first view");
        let second = broker.begin_view_index(1).expect("second view");
        let key = flight('3');
        let BrokerClaim::Leader(leader) = BrokerClaim::connect(first.environment(), key.clone()).expect("leader claim")
        else {
            panic!("first claimant must lead");
        };
        let BrokerClaim::Follower(follower) = BrokerClaim::connect(second.environment(), key).expect("follower claim")
        else {
            panic!("second claimant must follow");
        };
        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || follower.wait().expect("follower wait"));
            leader
                .fail(BrokerFailureClass::ExecutionClaim)
                .expect("execution claim failure");
            assert!(waiter.join().expect("follower thread").is_none());
        });
        first.finish().expect("first finish");
        second.finish().expect("second finish");
        broker.close().expect("broker close");
    }
}
