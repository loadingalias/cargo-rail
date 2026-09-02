//! Machine-owned remote compiler-cache authority.
//!
//! A remote destination is selected by transient machine environment or the
//! private transparent-cache setup receipt. Repository configuration cannot
//! name, enable, or promote remote storage.

mod azure;
mod coordinator;
mod evidence;
mod object;
mod s3;
mod url;

use std::fmt;
use std::io::Read;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::compiler::native_cache::RemoteAuthorityId;

const MAX_APPROVED_ENVIRONMENT_NAMES: usize = 512;
const MAX_APPROVED_ENVIRONMENT_NAME_BYTES: usize = 256;
const MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES: usize = 32 * 1024;

/// Machine-owned normalized remote URL.
pub(crate) const REMOTE_URL_ENV: &str = "CARGO_RAIL_CACHE_REMOTE";
/// Machine-owned remote publication mode.
pub(crate) const REMOTE_MODE_ENV: &str = "CARGO_RAIL_CACHE_MODE";
/// Additional reviewed compiler environment names admitted to L2 identity.
pub(crate) const REMOTE_ENVIRONMENT_ENV: &str = "CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT";

const BUILT_IN_ENVIRONMENT_NAMES: &[&str] = &["CARGO_PKG_NAME", "CARGO_PKG_VERSION_PATCH", "OUT_DIR"];

const PRIVATE_REMOTE_ENVIRONMENT: &[&str] = &[
    REMOTE_URL_ENV,
    REMOTE_MODE_ENV,
    REMOTE_ENVIRONMENT_ENV,
    coordinator::MARKER_ENV,
    "AWS_ACCESS_KEY_ID",
    "AWS_ACCOUNT_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_ROLE_ARN",
    "AWS_ROLE_SESSION_NAME",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_PROFILE",
    "AWS_DEFAULT_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_METADATA_SERVICE_TIMEOUT",
    "AWS_METADATA_SERVICE_NUM_ATTEMPTS",
    "AWS_SDK_LOAD_CONFIG",
    "AWS_LOGIN_CACHE_DIRECTORY",
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_S3",
    "AWS_ENDPOINT_URL_STS",
    "AWS_ENDPOINT_URL_SSO",
    "AWS_ENDPOINT_URL_SSO_OIDC",
    "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS",
    "AZURE_AUTHORITY_HOST",
    "AZURE_CLIENT_ID",
    "AZURE_CONFIG_DIR",
    "AZURE_FEDERATED_TOKEN_FILE",
    "AZURE_SUBSCRIPTION_ID",
    "AZURE_TENANT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_CLIENT_CERTIFICATE_PATH",
    "AZURE_CLIENT_CERTIFICATE_PASSWORD",
    "IDENTITY_ENDPOINT",
    "IDENTITY_HEADER",
    "IDENTITY_SERVER_THUMBPRINT",
    "IMDS_ENDPOINT",
    "MSI_ENDPOINT",
    "MSI_SECRET",
];

/// Remove remote selection and credentials from compiler children.
pub(crate) fn scrub_child_environment(command: &mut std::process::Command) {
    for name in PRIVATE_REMOTE_ENVIRONMENT {
        command.env_remove(name);
    }
}

/// One redacted remote-cache failure.
#[derive(Debug, Clone)]
pub(crate) struct RemoteStoreError {
    kind: RemoteStoreErrorKind,
    cause: Option<RemoteProbeFailureCause>,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteStoreErrorKind {
    Integrity,
    Configuration,
    Unavailable,
    Authentication,
}

/// One secret-safe cause retained for an actionable remote-cache probe failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteProbeFailureCause {
    Dns,
    Tls,
    RootStore,
    Connection,
    Timeout,
    Http,
    CredentialProvider,
}

impl RemoteProbeFailureCause {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Tls => "tls",
            Self::RootStore => "root_store",
            Self::Connection => "connection",
            Self::Timeout => "timeout",
            Self::Http => "http",
            Self::CredentialProvider => "credential_provider",
        }
    }

    pub(crate) const fn retry_guidance(self) -> &'static str {
        match self {
            Self::Dns => "verify DNS resolution for the selected remote host, then retry the probe",
            Self::Tls => "verify system time and TLS interception policy, then retry the probe",
            Self::RootStore => {
                "repair the host root certificate store or server certificate chain, then retry the probe"
            }
            Self::Connection => {
                "verify outbound HTTPS connectivity, proxy policy, and firewall rules, then retry the probe"
            }
            Self::Timeout => "verify network reachability and service health, then retry the probe",
            Self::Http => "retry the probe after the remote service recovers",
            Self::CredentialProvider => "refresh the selected machine credential provider, then retry the probe",
        }
    }
}

impl RemoteStoreError {
    pub(super) fn integrity(message: impl Into<String>) -> Self {
        Self::new(RemoteStoreErrorKind::Integrity, message)
    }

    pub(super) fn integrity_with_cause(cause: RemoteProbeFailureCause, message: impl Into<String>) -> Self {
        Self::new_with_cause(RemoteStoreErrorKind::Integrity, cause, message)
    }

    pub(super) fn configuration(message: impl Into<String>) -> Self {
        Self::new(RemoteStoreErrorKind::Configuration, message)
    }

    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(RemoteStoreErrorKind::Unavailable, message)
    }

    pub(super) fn unavailable_with_cause(cause: RemoteProbeFailureCause, message: impl Into<String>) -> Self {
        Self::new_with_cause(RemoteStoreErrorKind::Unavailable, cause, message)
    }

    pub(super) fn authentication(message: impl Into<String>) -> Self {
        Self::new(RemoteStoreErrorKind::Authentication, message)
    }

    pub(super) fn authentication_with_cause(cause: RemoteProbeFailureCause, message: impl Into<String>) -> Self {
        Self::new_with_cause(RemoteStoreErrorKind::Authentication, cause, message)
    }

    fn new(kind: RemoteStoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            cause: None,
            message: message.into(),
        }
    }

    fn new_with_cause(kind: RemoteStoreErrorKind, cause: RemoteProbeFailureCause, message: impl Into<String>) -> Self {
        Self {
            kind,
            cause: Some(cause),
            message: message.into(),
        }
    }

    pub(crate) fn cold_reason(&self) -> &'static str {
        match self.kind {
            RemoteStoreErrorKind::Integrity => "remote_entry_rejected",
            RemoteStoreErrorKind::Configuration
            | RemoteStoreErrorKind::Unavailable
            | RemoteStoreErrorKind::Authentication => "remote_cache_unavailable",
        }
    }

    pub(crate) const fn probe_failure(&self) -> &'static str {
        match self.kind {
            RemoteStoreErrorKind::Integrity => "integrity_failure",
            RemoteStoreErrorKind::Configuration => "configuration_failure",
            RemoteStoreErrorKind::Unavailable => "transport_failure",
            RemoteStoreErrorKind::Authentication => "authentication_failure",
        }
    }

    pub(crate) const fn probe_failure_cause(&self) -> Option<RemoteProbeFailureCause> {
        self.cause
    }
}

impl fmt::Display for RemoteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RemoteStoreError {}

pub(super) type RemoteStoreResult<T> = Result<T, RemoteStoreError>;

/// Maximum remote authority selected for one process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RemoteCacheMode {
    Read,
    ReadWrite,
}

/// Result of authenticating the selected object store and validating its protocol marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteProtocolMarkerState {
    Existing,
    Initialized,
}

impl RemoteProtocolMarkerState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Existing => "existing",
            Self::Initialized => "initialized",
        }
    }
}

/// Redacted product-level remote-cache readiness report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteCacheProbeStatus {
    pub(crate) remote: RemoteCacheConfigurationStatus,
    pub(crate) protocol_marker: RemoteProtocolMarkerState,
}

/// Canonical non-secret remote policy persisted in private machine setup state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledRemoteCache {
    version: u32,
    normalized_url: String,
    mode: RemoteCacheMode,
    additional_environment_names: Vec<String>,
}

impl InstalledRemoteCache {
    const VERSION: u32 = 1;

    pub(crate) fn from_selection(selection: &RemoteCacheSelection) -> Self {
        let additional_environment_names = selection
            .approved_environment_names
            .iter()
            .filter(|name| !BUILT_IN_ENVIRONMENT_NAMES.contains(&name.as_str()))
            .cloned()
            .collect();
        Self {
            version: Self::VERSION,
            normalized_url: selection.normalized_url().to_string(),
            mode: selection.mode,
            additional_environment_names,
        }
    }

    pub(crate) fn selection(&self) -> RemoteStoreResult<RemoteCacheSelection> {
        if self.version != Self::VERSION {
            return Err(RemoteStoreError::configuration(
                "installed remote cache policy has an incompatible version",
            ));
        }
        let selection = RemoteCacheSelection::parse(
            &self.normalized_url,
            Some(self.mode.as_str()),
            &self.additional_environment_names,
        )?;
        if selection.normalized_url() != self.normalized_url {
            return Err(RemoteStoreError::configuration(
                "installed remote cache URL is not canonical",
            ));
        }
        Ok(selection)
    }
}

impl RemoteCacheMode {
    fn parse(value: &str) -> RemoteStoreResult<Self> {
        match value {
            "read" => Ok(Self::Read),
            "read-write" => Ok(Self::ReadWrite),
            _ => Err(RemoteStoreError::configuration(
                "remote cache mode must be 'read' or 'read-write'",
            )),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read-write",
        }
    }
}

/// Exact normalized remote authority selected by machine state.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteCacheSelection {
    authority: url::RemoteCacheAuthority,
    mode: RemoteCacheMode,
    approved_environment_names: Vec<String>,
}

impl fmt::Debug for RemoteCacheSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCacheSelection")
            .field("authority", &self.authority.identity().as_str())
            .field("provider", &self.authority.provider_name())
            .field("mode", &self.mode)
            .field("approved_environment_names", &self.approved_environment_names.len())
            .finish()
    }
}

impl RemoteCacheSelection {
    /// Parse one explicit URL and policy without consulting ambient state.
    pub(crate) fn parse(
        remote_url: &str,
        mode: Option<&str>,
        additional_environment: &[String],
    ) -> RemoteStoreResult<Self> {
        let authority = url::RemoteCacheAuthority::parse(remote_url)?;
        let mode = mode
            .map(RemoteCacheMode::parse)
            .transpose()?
            .unwrap_or(RemoteCacheMode::ReadWrite);
        validate_environment_names(additional_environment)?;
        if additional_environment
            .iter()
            .any(|name| environment_name_may_be_secret(name))
        {
            return Err(RemoteStoreError::configuration(
                "remote cache environment policy contains a credential-like name",
            ));
        }
        let mut approved_environment_names = BUILT_IN_ENVIRONMENT_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .chain(additional_environment.iter().cloned())
            .collect::<Vec<_>>();
        approved_environment_names.sort_unstable();
        approved_environment_names.dedup();
        validate_environment_names(&approved_environment_names)?;
        Ok(Self {
            authority,
            mode,
            approved_environment_names,
        })
    }

    /// Load the one optional machine-owned selection.
    pub(crate) fn from_environment() -> RemoteStoreResult<Option<Self>> {
        let remote_url = std::env::var_os(REMOTE_URL_ENV).filter(|value| !value.is_empty());
        let mode = std::env::var_os(REMOTE_MODE_ENV).filter(|value| !value.is_empty());
        let additional = std::env::var_os(REMOTE_ENVIRONMENT_ENV).filter(|value| !value.is_empty());
        let Some(remote_url) = remote_url else {
            if mode.is_some() || additional.is_some() {
                return Err(RemoteStoreError::configuration(
                    "remote cache policy is set without CARGO_RAIL_CACHE_REMOTE",
                ));
            }
            return Ok(None);
        };
        let remote_url = remote_url
            .to_str()
            .ok_or_else(|| RemoteStoreError::configuration("remote cache URL is not valid UTF-8"))?;
        let mode = mode
            .as_deref()
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| RemoteStoreError::configuration("remote cache mode is not valid UTF-8"))
            })
            .transpose()?;
        let additional = additional
            .as_deref()
            .map(parse_environment_policy)
            .transpose()?
            .unwrap_or_default();
        Self::parse(remote_url, mode, &additional).map(Some)
    }

    /// Prefer an explicit transient selection, then one receipt-owned policy.
    pub(crate) fn from_environment_or_installed(
        installed: Option<&InstalledRemoteCache>,
    ) -> RemoteStoreResult<Option<Self>> {
        match Self::from_environment()? {
            Some(selection) => Ok(Some(selection)),
            None => installed.map(InstalledRemoteCache::selection).transpose(),
        }
    }

    pub(crate) fn authority(&self) -> &RemoteAuthorityId {
        self.authority.identity()
    }

    pub(crate) const fn mode(&self) -> RemoteCacheMode {
        self.mode
    }

    pub(crate) fn approved_environment_names(&self) -> &[String] {
        &self.approved_environment_names
    }

    pub(crate) fn normalized_url(&self) -> &str {
        self.authority.normalized_url()
    }

    pub(crate) fn provider_name(&self) -> &'static str {
        self.authority.provider_name()
    }

    pub(crate) fn protocol_name(&self) -> &'static str {
        self.authority.protocol_name()
    }

    pub(crate) fn direct_transport_supported(&self) -> bool {
        self.authority.supports_s3_transport() || self.authority.is_azure_blob()
    }

    pub(crate) fn approves_environment_names(&self, names: &[String]) -> bool {
        names
            .iter()
            .all(|name| self.approved_environment_names.binary_search(name).is_ok())
    }
}

pub(crate) use object::Publication as RemotePublication;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteTransferMetrics {
    pub(crate) request_attempts: u64,
    pub(crate) coordinator_requests: u64,
    pub(crate) payload_bytes_read: u64,
    pub(crate) payload_bytes_written: u64,
    pub(crate) service_elapsed_ns: u64,
}

pub(crate) enum RemoteBody {
    Direct(object::EntryBody),
    Coordinated(coordinator::PackReader),
}

impl Read for RemoteBody {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Direct(body) => body.read(output),
            Self::Coordinated(body) => body.read(output),
        }
    }
}

impl RemoteBody {
    pub(crate) fn copy_compressed_to<W: std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<u64> {
        match self {
            Self::Direct(body) => body.copy_compressed_to(writer),
            Self::Coordinated(body) => {
                let copied = std::io::copy(&mut *body, writer)?;
                body.finish()?;
                Ok(copied)
            }
        }
    }
}

pub(crate) enum RemoteLookup {
    Miss,
    Conflict,
    Unique {
        selector: crate::compiler::native_cache::NativeDynamicInputSelector,
        action_key: String,
        result_key: String,
        body: RemoteBody,
        bytes: u64,
        compressed_bytes: u64,
    },
}

/// One coordinator-preferred store with a lazy direct fallback.
pub(crate) struct RemoteStore {
    selection: RemoteCacheSelection,
    coordinator: Option<coordinator::Client>,
    coordinator_connect_error: Option<RemoteStoreError>,
    coordinator_failed: AtomicBool,
    direct: OnceLock<RemoteStoreResult<object::ObjectStore>>,
    evidence: OnceLock<RemoteStoreResult<evidence::EvidenceStore>>,
}

impl fmt::Debug for RemoteStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteStore")
            .field("provider", &self.selection.provider_name())
            .field("mode", &self.selection.mode().as_str())
            .finish_non_exhaustive()
    }
}

impl RemoteStore {
    pub(crate) fn connect(
        selection: &RemoteCacheSelection,
        installation: Option<&crate::cache::installation::InstallationReceipt>,
    ) -> RemoteStoreResult<Self> {
        let (coordinator, coordinator_connect_error) =
            match installation.map(|receipt| coordinator::connect(selection, receipt)) {
                Some(Ok(coordinator)) => (coordinator, None),
                Some(Err(error)) => (None, Some(error)),
                None => (None, None),
            };
        Ok(Self {
            selection: selection.clone(),
            coordinator,
            coordinator_connect_error,
            coordinator_failed: AtomicBool::new(false),
            direct: OnceLock::new(),
            evidence: OnceLock::new(),
        })
    }

    fn direct(&self) -> RemoteStoreResult<&object::ObjectStore> {
        match self.direct.get_or_init(|| object::connect(&self.selection)) {
            Ok(store) => Ok(store),
            Err(error) => Err(error.clone()),
        }
    }

    fn evidence(&self) -> RemoteStoreResult<&evidence::EvidenceStore> {
        match self
            .evidence
            .get_or_init(|| evidence::EvidenceStore::connect(&self.selection))
        {
            Ok(store) => Ok(store),
            Err(error) => Err(error.clone()),
        }
    }

    pub(crate) fn metrics(&self) -> RemoteTransferMetrics {
        let direct = self
            .direct
            .get()
            .and_then(|store| store.as_ref().ok())
            .map_or_else(object::TransferMetrics::default, object::ObjectStore::metrics);
        let evidence = self
            .evidence
            .get()
            .and_then(|store| store.as_ref().ok())
            .map_or_else(object::TransferMetrics::default, evidence::EvidenceStore::metrics);
        let coordinated = self
            .coordinator
            .as_ref()
            .map_or_else(coordinator::Metrics::default, coordinator::Client::metrics);
        RemoteTransferMetrics {
            request_attempts: direct
                .request_attempts
                .saturating_add(evidence.request_attempts)
                .saturating_add(coordinated.request_attempts),
            coordinator_requests: coordinated.requests,
            payload_bytes_read: direct
                .payload_bytes_read
                .saturating_add(evidence.payload_bytes_read)
                .saturating_add(coordinated.payload_bytes_read),
            payload_bytes_written: direct
                .payload_bytes_written
                .saturating_add(evidence.payload_bytes_written)
                .saturating_add(coordinated.payload_bytes_written),
            service_elapsed_ns: direct
                .service_elapsed_ns
                .saturating_add(evidence.service_elapsed_ns)
                .saturating_add(coordinated.service_elapsed_ns),
        }
    }

    pub(crate) fn coordinator_connect_error(&self) -> Option<&RemoteStoreError> {
        self.coordinator_connect_error.as_ref()
    }

    pub(crate) fn lookup(&self, base_action_key: &str) -> RemoteStoreResult<RemoteLookup> {
        if !self.coordinator_failed.load(Ordering::Acquire)
            && let Some(coordinator) = &self.coordinator
        {
            match coordinator.lookup(base_action_key) {
                Ok(coordinator::Lookup::Miss) => return Ok(RemoteLookup::Miss),
                Ok(coordinator::Lookup::Conflict) => return Ok(RemoteLookup::Conflict),
                Ok(coordinator::Lookup::Unique {
                    selector,
                    action_key,
                    result_key,
                    body,
                    bytes,
                    compressed_bytes,
                }) => {
                    return Ok(RemoteLookup::Unique {
                        selector,
                        action_key,
                        result_key,
                        body: RemoteBody::Coordinated(body),
                        bytes,
                        compressed_bytes,
                    });
                }
                Err(_) => self.coordinator_failed.store(true, Ordering::Release),
            }
        }
        match self.direct()?.lookup(base_action_key)? {
            object::Lookup::Miss => Ok(RemoteLookup::Miss),
            object::Lookup::Conflict => Ok(RemoteLookup::Conflict),
            object::Lookup::Unique {
                selector,
                action_key,
                result_key,
                body,
                bytes,
                compressed_bytes,
            } => Ok(RemoteLookup::Unique {
                selector,
                action_key,
                result_key,
                body: RemoteBody::Direct(body),
                bytes,
                compressed_bytes,
            }),
        }
    }

    pub(crate) fn publish(
        &self,
        association: &crate::compiler::native_cache::pack::NativeAssociation,
        base_action_key: &str,
        selector: &crate::compiler::native_cache::NativeDynamicInputSelector,
        pack: std::fs::File,
    ) -> RemoteStoreResult<RemotePublication> {
        if !self.coordinator_failed.load(Ordering::Acquire)
            && let Some(coordinator) = &self.coordinator
        {
            match coordinator.publish(
                association,
                base_action_key,
                selector,
                pack.try_clone()
                    .map_err(|error| RemoteStoreError::unavailable(error.to_string()))?,
            ) {
                Ok(publication) => return Ok(publication),
                Err(_) => self.coordinator_failed.store(true, Ordering::Release),
            }
        }
        self.direct()?.publish(association, base_action_key, selector, pack)
    }

    /// Import every exact remote candidate through the ordinary local CAS
    /// admission path. High-level evidence stores still decide whether the
    /// imported candidates form one complete reusable result.
    pub(crate) fn import_compiler_evidence(
        &self,
        cas: &crate::cache::cas::LocalCas,
        candidate_key: &str,
    ) -> RemoteStoreResult<usize> {
        self.evidence()?.import(cas, candidate_key)
    }

    /// Publish one already-validated local evidence result. Callers own
    /// dependency ordering: objects before sets, and all referenced evidence
    /// before native bindings.
    pub(crate) fn publish_compiler_evidence(
        &self,
        validation: &crate::compiler::diagnostics_store::CompilerEvidenceValidation,
        evidence: &crate::compiler::diagnostics_store::CompilerEvidenceObject,
    ) -> RemoteStoreResult<()> {
        self.evidence()?.publish(validation, evidence)
    }
}

pub(crate) fn run_coordinator_if_requested() -> Option<i32> {
    coordinator::run_if_requested()
}

pub(crate) fn stop_installed_coordinators(receipt: &crate::cache::installation::InstallationReceipt) {
    coordinator::stop_all(receipt);
}

/// Redacted projection of one selected machine-owned target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteCacheConfigurationStatus {
    pub(crate) provider: &'static str,
    pub(crate) protocol: &'static str,
    pub(crate) authority: String,
    pub(crate) mode: &'static str,
    pub(crate) shared_environment_names: usize,
    pub(crate) activation: &'static str,
    pub(crate) selection_source: &'static str,
}

impl RemoteCacheConfigurationStatus {
    pub(crate) fn from_selection(selection: &RemoteCacheSelection) -> Self {
        Self::from_selection_source(selection, "explicit_process")
    }

    pub(crate) fn from_selection_source(selection: &RemoteCacheSelection, selection_source: &'static str) -> Self {
        Self {
            provider: selection.provider_name(),
            protocol: selection.protocol_name(),
            authority: selection.authority().as_str().to_string(),
            mode: selection.mode().as_str(),
            shared_environment_names: selection.approved_environment_names().len(),
            activation: if selection.direct_transport_supported() {
                "direct_transport_selected"
            } else {
                "authority_selected_transport_inactive"
            },
            selection_source,
        }
    }
}

pub(crate) fn configuration_status(
    current_dir: &std::path::Path,
) -> RemoteStoreResult<Option<RemoteCacheConfigurationStatus>> {
    if let Some(selection) = RemoteCacheSelection::from_environment()? {
        return Ok(Some(RemoteCacheConfigurationStatus::from_selection_source(
            &selection,
            "transient_environment",
        )));
    }
    let installed = crate::cache::installation::installed_remote(current_dir)
        .map_err(|_| RemoteStoreError::configuration("installed remote cache policy is unavailable"))?;
    installed
        .as_ref()
        .and_then(crate::cache::installation::InstalledRemoteSelection::remote)
        .map(InstalledRemoteCache::selection)
        .transpose()
        .map(|selection| {
            selection
                .as_ref()
                .map(|selection| RemoteCacheConfigurationStatus::from_selection_source(selection, "installed_profile"))
        })
}

/// Authenticate the selected direct object store and validate its exact protocol marker.
pub(crate) fn probe(current_dir: &std::path::Path) -> RemoteStoreResult<RemoteCacheProbeStatus> {
    let transient = RemoteCacheSelection::from_environment()?;
    let installed = if transient.is_none() {
        crate::cache::installation::installed_remote(current_dir)
            .map_err(|_| RemoteStoreError::configuration("installed remote cache policy is unavailable"))?
    } else {
        None
    };
    let (selection, source) = if let Some(selection) = transient {
        (selection, "transient_environment")
    } else {
        let installed = installed
            .as_ref()
            .and_then(crate::cache::installation::InstalledRemoteSelection::remote)
            .ok_or_else(|| RemoteStoreError::configuration("no remote cache authority is selected"))?;
        (installed.selection()?, "installed_profile")
    };
    let remote = RemoteCacheConfigurationStatus::from_selection_source(&selection, source);
    let protocol_marker = object::probe(&selection)?;
    Ok(RemoteCacheProbeStatus {
        remote,
        protocol_marker,
    })
}

fn parse_environment_policy(value: &std::ffi::OsStr) -> RemoteStoreResult<Vec<String>> {
    let value = value
        .to_str()
        .ok_or_else(|| RemoteStoreError::configuration("remote cache environment policy is not valid UTF-8"))?;
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let names = value.split(',').map(str::to_string).collect::<Vec<_>>();
    validate_environment_names(&names)?;
    Ok(names)
}

fn strictly_sorted_unique(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_APPROVED_ENVIRONMENT_NAME_BYTES
        && !name.as_bytes().contains(&0)
        && !name.contains('=')
        && !name.chars().any(char::is_control)
}

fn environment_name_may_be_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "SSH_AUTH_SOCK"
            | "GPG_AGENT_INFO"
            | "DOCKER_AUTH_CONFIG"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "AWS_ACCESS_KEY_ID"
            | "AWS_SECRET_ACCESS_KEY"
            | "AWS_SESSION_TOKEN"
    ) || [
        "_TOKEN",
        "_SECRET",
        "_PASSWORD",
        "_PASSWD",
        "_PRIVATE_KEY",
        "_ACCESS_KEY",
        "_CREDENTIAL",
        "_CREDENTIALS",
        "_AUTHORIZATION",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

fn validate_environment_names(names: &[String]) -> RemoteStoreResult<()> {
    if names.len() > MAX_APPROVED_ENVIRONMENT_NAMES
        || !strictly_sorted_unique(names)
        || names.iter().any(|name| !valid_environment_name(name))
        || names
            .iter()
            .try_fold(0_usize, |total, name| total.checked_add(name.len()))
            .is_none_or(|bytes| bytes > MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES)
    {
        return Err(RemoteStoreError::configuration(
            "remote cache environment policy is invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_defaults_to_write_and_keeps_identity_independent_of_mode() {
        let url = "s3://rail-cache/team?owner=123456789012&region=us-east-1";
        let read = RemoteCacheSelection::parse(url, Some("read"), &[]).expect("read selection");
        let write = RemoteCacheSelection::parse(url, None, &[]).expect("write selection");
        assert_eq!(read.authority(), write.authority());
        assert_eq!(read.mode(), RemoteCacheMode::Read);
        assert_eq!(write.mode(), RemoteCacheMode::ReadWrite);
    }

    #[test]
    fn every_accepted_provider_selects_a_bounded_direct_transport() {
        for url in [
            "s3://rail-cache/team?owner=123456789012&region=us-east-1",
            "azure://railcache/cargo-rail/team",
            "r2://0123456789abcdef0123456789abcdef/rail-cache/team",
            "s3+http://127.0.0.1:9000/rail-cache/team?region=test-1",
        ] {
            let selection = RemoteCacheSelection::parse(url, Some("read"), &[]).expect("provider selection");
            assert!(selection.direct_transport_supported(), "inactive provider: {url}");
        }
    }

    #[test]
    fn selection_rejects_secret_like_or_noncanonical_environment_policy() {
        let url = "s3://rail-cache/team?owner=123456789012&region=us-east-1";
        RemoteCacheSelection::parse(url, None, &["CI_TOKEN".to_string()]).unwrap_err();
        RemoteCacheSelection::parse(url, None, &["Z_FLAG".to_string(), "A_FLAG".to_string()]).unwrap_err();
    }

    #[test]
    fn selection_approves_observed_portable_cargo_package_metadata() {
        let selection =
            RemoteCacheSelection::parse("s3://rail-cache/team?owner=123456789012&region=us-east-1", None, &[])
                .expect("selection");
        assert!(
            selection
                .approves_environment_names(&["CARGO_PKG_NAME".to_string(), "CARGO_PKG_VERSION_PATCH".to_string(),])
        );
    }

    #[test]
    fn every_remote_authority_variable_is_explicitly_removed_from_compiler_children() {
        let mut command = std::process::Command::new("rustc");
        for name in PRIVATE_REMOTE_ENVIRONMENT {
            command.env(name, "private");
        }
        scrub_child_environment(&mut command);

        let overrides = command
            .get_envs()
            .map(|(name, value)| (name.to_string_lossy().into_owned(), value.is_none()))
            .collect::<std::collections::BTreeMap<_, _>>();
        for name in PRIVATE_REMOTE_ENVIRONMENT {
            assert_eq!(overrides.get(*name), Some(&true), "{name} was not removed");
        }
    }
}
