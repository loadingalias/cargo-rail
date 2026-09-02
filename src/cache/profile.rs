//! Workspace-bound machine cache authority behind one global compiler wrapper.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::cache::cas::{DEFAULT_CACHE_MAX_BYTES, LocalCacheSelection};
use crate::error::{RailError, RailResult};
use crate::remote_cache::{InstalledRemoteCache, RemoteCacheSelection};

const STORE_DIRECTORY: &str = "cache-profiles-v1";
const BINDINGS_DIRECTORY: &str = "bindings";
const PROFILES_DIRECTORY: &str = "profiles";
const STATE_DIRECTORY: &str = "state";
const STORE_LOCK_FILE: &str = "registry.lock";
const TRANSACTION_FILE: &str = "transaction.json";
const UNBOUND_PRE_PROFILE_STATE_FILE: &str = "unbound-v0.25.json";
const LIFECYCLE_LOCK_FILE: &str = "profile.lock";
const PROFILE_VERSION: u32 = 1;
const BINDING_VERSION: u32 = 1;
const TRANSACTION_VERSION: u32 = 1;
const MAX_PROFILE_BYTES: u64 = 64 * 1024;
const MAX_BINDING_BYTES: u64 = 16 * 1024;
const MAX_TRANSACTION_BYTES: u64 = 256 * 1024;
const MAX_PROFILE_ROOTS: usize = 64;
const MAX_PROFILES: usize = 4096;
const MAX_TRANSACTION_MUTATIONS: usize = MAX_PROFILE_ROOTS + 4;
pub(crate) const COORDINATOR_PROFILE_ENV: &str = "CARGO_RAIL_CACHE_PROFILE";

/// Explicit machine authority for sharing verified results across checkout roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootPortability {
    #[default]
    Physical,
    Remap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ProfileState {
    #[default]
    Active,
    Detached,
}

impl RootPortability {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Physical => "physical",
            Self::Remap => "remap",
        }
    }
}

/// One exact physical Cargo workspace enrollment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceBinding {
    canonical_root: PathBuf,
    physical_identity: String,
}

/// One independently replaceable workspace cache authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledCacheProfile {
    version: u32,
    #[serde(default)]
    state: ProfileState,
    profile_id: String,
    generation: String,
    roots: Vec<WorkspaceBinding>,
    cache: LocalCacheSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<InstalledRemoteCache>,
    #[serde(default)]
    root_portability: RootPortability,
    owner_installation: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProfileLifecycleLock {
    _file: Arc<fs::File>,
}

/// Exclusive authority over the registry and every retained profile lifecycle.
pub(crate) struct ProfileRegistryWriteGuard {
    _registry: fs::File,
    _profiles: Vec<fs::File>,
}

impl InstalledCacheProfile {
    pub(crate) fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }

    pub(crate) fn cache(&self) -> &LocalCacheSelection {
        &self.cache
    }

    pub(crate) fn remote(&self) -> Option<&InstalledRemoteCache> {
        self.remote.as_ref()
    }

    pub(crate) const fn root_portability(&self) -> RootPortability {
        self.root_portability
    }

    pub(crate) fn selected_root(&self) -> &Path {
        &self.roots[0].canonical_root
    }

    pub(crate) fn selected_root_identity(&self) -> &str {
        &self.roots[0].physical_identity
    }

    pub(crate) fn coordinator_capability(&self) -> String {
        format!("{}:{}", self.profile_id, self.generation)
    }

    pub(crate) fn remote_selection(&self) -> RailResult<Option<RemoteCacheSelection>> {
        self.remote
            .as_ref()
            .map(InstalledRemoteCache::selection)
            .transpose()
            .map_err(|error| RailError::message(format!("installed profile remote policy is invalid: {error}")))
    }

    fn select_root(mut self, identity: &WorkspaceIdentity) -> RailResult<Self> {
        let selected = self
            .roots
            .iter()
            .position(|root| root.physical_identity == identity.physical_identity)
            .ok_or_else(|| RailError::message("cache profile does not own the selected workspace identity"))?;
        if self.roots[selected].canonical_root != identity.canonical_root {
            return Err(RailError::with_help(
                "the enrolled workspace moved or its canonical spelling changed",
                "explicitly re-enroll the root with `cargo rail cache setup --profile <PROFILE_ID>`",
            ));
        }
        self.roots.swap(0, selected);
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> RailResult<()> {
        if self.version != PROFILE_VERSION
            || !valid_identity(&self.profile_id)
            || !valid_identity(&self.generation)
            || !valid_identity(&self.owner_installation)
            || self.roots.len() > MAX_PROFILE_ROOTS
            || (self.roots.is_empty() != (self.state == ProfileState::Detached))
        {
            return Err(RailError::message("installed cache profile is invalid"));
        }
        let mut physical = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for root in &self.roots {
            if !root.canonical_root.is_absolute()
                || !valid_identity(&root.physical_identity)
                || !physical.insert(root.physical_identity.as_str())
                || !paths.insert(root.canonical_root.as_path())
            {
                return Err(RailError::message(
                    "installed cache profile workspace bindings are invalid",
                ));
            }
        }
        LocalCacheSelection::new(
            self.cache.base().to_path_buf(),
            self.cache.max_bytes(),
            self.cache.trust_domain().map(str::to_string),
        )?;
        if self.cache.trust_domain().is_none() {
            return Err(RailError::message(
                "installed cache profile has no isolated local trust domain",
            ));
        }
        if let Some(remote) = &self.remote {
            remote
                .selection()
                .map_err(|error| RailError::message(format!("installed profile remote policy is invalid: {error}")))?;
        }
        if self.root_portability == RootPortability::Remap && self.remote.is_none() {
            return Err(RailError::message(
                "installed cache profile enables root remapping without a remote authority",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileBindingRecord {
    version: u32,
    physical_identity: String,
    canonical_root: PathBuf,
    profile_id: String,
}

impl ProfileBindingRecord {
    fn validate(&self) -> RailResult<()> {
        if self.version != BINDING_VERSION
            || !valid_identity(&self.physical_identity)
            || !valid_identity(&self.profile_id)
            || !self.canonical_root.is_absolute()
        {
            return Err(RailError::message("workspace cache profile binding is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct WorkspaceIdentity {
    canonical_root: PathBuf,
    physical_identity: String,
}

/// Cache-policy inputs for one explicit workspace enrollment.
pub(crate) struct ProfileSetupRequest<'a> {
    pub(crate) requested_profile: Option<&'a str>,
    pub(crate) local_dir: Option<&'a Path>,
    pub(crate) max_bytes: Option<u64>,
    pub(crate) remote_url: Option<&'a str>,
    pub(crate) remote_mode: Option<&'a str>,
    pub(crate) remote_environment: &'a [String],
    pub(crate) root_portability: Option<&'a str>,
    pub(crate) local_only: bool,
}

/// Policy retained from the global v0.25 receipt without assigning it to a workspace.
pub(crate) struct PreProfileSetupInput {
    pub(crate) installation_authority: String,
    pub(crate) cache: LocalCacheSelection,
    pub(crate) remote: Option<InstalledRemoteCache>,
    pub(crate) root_portability: RootPortability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnboundPreProfileState {
    version: u32,
    installation_authority: String,
    cache: LocalCacheSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<InstalledRemoteCache>,
    root_portability: RootPortability,
}

impl UnboundPreProfileState {
    fn validate(&self) -> RailResult<()> {
        if self.version != 1 || !valid_identity(&self.installation_authority) {
            return Err(RailError::message("unbound pre-profile cache state is invalid"));
        }
        LocalCacheSelection::new(
            self.cache.base().to_path_buf(),
            self.cache.max_bytes(),
            self.cache.trust_domain().map(str::to_string),
        )?;
        if let Some(remote) = &self.remote {
            remote.selection().map_err(|error| {
                RailError::message(format!("unbound pre-profile remote policy is invalid: {error}"))
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreProfileStateStatus {
    pub(crate) state: &'static str,
    pub(crate) cache_base: String,
    pub(crate) max_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trust_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remote_authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remote_mode: Option<&'static str>,
    pub(crate) root_portability: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProfileStatus {
    pub(crate) profile_id: String,
    pub(crate) generation: String,
    pub(crate) state: &'static str,
    pub(crate) roots: Vec<String>,
    pub(crate) cache_base: String,
    pub(crate) trust_domain: String,
    pub(crate) max_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remote_authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remote_mode: Option<&'static str>,
    pub(crate) root_portability: &'static str,
}

/// No-write preview of one profile enrollment or policy repair.
pub(crate) struct ProfileSetupPlan {
    store: ProfileStore,
    transaction_before: Option<Vec<u8>>,
    pre_profile_before: Option<Vec<u8>>,
    pre_profile_after: Option<Vec<u8>>,
    mutations: Vec<ProfileFileMutation>,
    desired: InstalledCacheProfile,
    pending: bool,
}

pub(crate) struct ProfileDetachPlan {
    store: ProfileStore,
    transaction_before: Option<Vec<u8>>,
    mutations: Vec<ProfileFileMutation>,
    profile_id: String,
    pending: bool,
}

pub(crate) struct ProfileRemovalPlan {
    store: ProfileStore,
    profile_id: String,
    profile_before: Option<Vec<u8>>,
    cache_root: Option<PathBuf>,
    state_root: PathBuf,
    bytes: u64,
}

pub(crate) struct PreProfileStateRemovalPlan {
    store: ProfileStore,
    state_before: Option<Vec<u8>>,
    cache_root: Option<PathBuf>,
    bytes: u64,
}

impl PreProfileStateRemovalPlan {
    pub(crate) fn pending(&self) -> bool {
        self.state_before.is_some()
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn cache_root(&self) -> Option<&Path> {
        self.cache_root.as_deref()
    }
}

impl ProfileRemovalPlan {
    pub(crate) fn pending(&self) -> bool {
        self.profile_before.is_some()
    }

    pub(crate) fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn cache_root(&self) -> Option<&Path> {
        self.cache_root.as_deref()
    }

    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }
}

impl ProfileDetachPlan {
    pub(crate) const fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn profile_id(&self) -> &str {
        &self.profile_id
    }
}

impl ProfileSetupPlan {
    pub(crate) const fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn profile(&self) -> &InstalledCacheProfile {
        &self.desired
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFileMutation {
    relative_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileTransaction {
    version: u32,
    transaction_id: String,
    mutations: Vec<ProfileFileMutation>,
}

impl ProfileTransaction {
    fn validate(&self) -> RailResult<()> {
        if self.version != TRANSACTION_VERSION
            || !valid_identity(&self.transaction_id)
            || self.mutations.is_empty()
            || self.mutations.len() > MAX_TRANSACTION_MUTATIONS
        {
            return Err(RailError::message("cache profile transaction is invalid"));
        }
        let mut paths = BTreeSet::new();
        let mut selected_profile = None::<String>;
        for mutation in &self.mutations {
            if mutation.before == mutation.after
                || !valid_relative_store_path(&mutation.relative_path)
                || !paths.insert(mutation.relative_path.as_path())
                || mutation
                    .before
                    .as_ref()
                    .is_some_and(|bytes| bytes.len() as u64 > MAX_PROFILE_BYTES)
                || mutation
                    .after
                    .as_ref()
                    .is_some_and(|bytes| bytes.len() as u64 > MAX_PROFILE_BYTES)
            {
                return Err(RailError::message("cache profile transaction mutation is invalid"));
            }
            let (directory, identity) = relative_store_identity(&mutation.relative_path)?;
            match directory {
                PROFILES_DIRECTORY => {
                    if selected_profile.replace(identity.to_string()).is_some() || mutation.after.is_none() {
                        return Err(RailError::message(
                            "cache profile transaction must replace exactly one profile record",
                        ));
                    }
                    for bytes in [mutation.before.as_deref(), mutation.after.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        if decode_profile(bytes)?.profile_id != identity {
                            return Err(RailError::message(
                                "cache profile transaction record does not match its path",
                            ));
                        }
                    }
                }
                BINDINGS_DIRECTORY => {
                    for bytes in [mutation.before.as_deref(), mutation.after.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        let binding = decode_binding(bytes)?;
                        if binding.physical_identity != identity {
                            return Err(RailError::message(
                                "cache profile transaction binding does not match its path",
                            ));
                        }
                    }
                }
                _ => {
                    return Err(RailError::message(
                        "cache profile transaction path is outside its store",
                    ));
                }
            }
        }
        let profile_id = selected_profile
            .as_deref()
            .ok_or_else(|| RailError::message("cache profile transaction has no profile record"))?;
        for mutation in &self.mutations {
            let (directory, _) = relative_store_identity(&mutation.relative_path)?;
            if directory != BINDINGS_DIRECTORY {
                continue;
            }
            for bytes in [mutation.before.as_deref(), mutation.after.as_deref()]
                .into_iter()
                .flatten()
            {
                if decode_binding(bytes)?.profile_id != profile_id {
                    return Err(RailError::message(
                        "cache profile transaction crosses profile authorities",
                    ));
                }
            }
        }
        Ok(())
    }

    fn profile_id(&self) -> RailResult<&str> {
        self.mutations
            .iter()
            .find_map(|mutation| {
                let (directory, identity) = relative_store_identity(&mutation.relative_path).ok()?;
                (directory == PROFILES_DIRECTORY).then_some(identity)
            })
            .ok_or_else(|| RailError::message("cache profile transaction has no profile record"))
    }
}

#[derive(Debug, Clone)]
struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    fn new(cargo_home: &Path) -> RailResult<Self> {
        let cargo_home = crate::utils::canonicalize_existing(cargo_home)?;
        Ok(Self {
            root: cargo_home.join("cargo-rail").join(STORE_DIRECTORY),
        })
    }

    fn binding_relative(identity: &str) -> PathBuf {
        PathBuf::from(BINDINGS_DIRECTORY).join(format!("{identity}.json"))
    }

    fn profile_relative(profile_id: &str) -> PathBuf {
        PathBuf::from(PROFILES_DIRECTORY).join(format!("{profile_id}.json"))
    }

    fn profile_state_directory(&self, profile_id: &str) -> PathBuf {
        self.root.join(STATE_DIRECTORY).join(profile_id)
    }

    fn lifecycle_lock(&self, profile_id: &str, create: bool, exclusive: bool) -> RailResult<fs::File> {
        validate_identity(profile_id)?;
        let directory = self.profile_state_directory(profile_id);
        if create {
            super::installation::create_private_directory(&directory)?;
        } else {
            let metadata = fs::symlink_metadata(&directory)?;
            if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
                return Err(RailError::message(
                    "cache profile lifecycle directory is not a real directory",
                ));
            }
        }
        let path = directory.join(LIFECYCLE_LOCK_FILE);
        let file = crate::utils::open_cache_lock_file(&path, create)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if create {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
        }
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "cache profile lifecycle lock is not a private regular file",
            ));
        }
        if exclusive {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "cache profile lifecycle lock changed while it was acquired",
            ));
        }
        Ok(file)
    }

    fn path(&self, relative: &Path) -> RailResult<PathBuf> {
        if !valid_relative_store_path(relative) {
            return Err(RailError::message("cache profile private-state path is invalid"));
        }
        Ok(self.root.join(relative))
    }

    fn read(&self, relative: &Path, maximum: u64) -> RailResult<Option<Vec<u8>>> {
        validate_existing_layout(&self.root)?;
        super::installation::read_optional_regular(&self.path(relative)?, maximum)
    }

    fn read_effective(
        &self,
        relative: &Path,
        maximum: u64,
        recovery: Option<&ProfileTransaction>,
    ) -> RailResult<Option<Vec<u8>>> {
        let current = self.read(relative, maximum)?;
        let Some(mutation) = recovery.and_then(|transaction| {
            transaction
                .mutations
                .iter()
                .find(|mutation| mutation.relative_path == relative)
        }) else {
            return Ok(current);
        };
        if current == mutation.before || current == mutation.after {
            Ok(mutation.after.clone())
        } else {
            Err(RailError::message(
                "cache profile state diverged from its interrupted transaction",
            ))
        }
    }

    fn transaction_path(&self) -> PathBuf {
        self.root.join(TRANSACTION_FILE)
    }

    fn pre_profile_state_path(&self) -> PathBuf {
        self.root.join(UNBOUND_PRE_PROFILE_STATE_FILE)
    }

    fn read_pre_profile_state(&self) -> RailResult<Option<Vec<u8>>> {
        validate_existing_layout(&self.root)?;
        super::installation::read_optional_regular(&self.pre_profile_state_path(), MAX_PROFILE_BYTES)
    }

    fn remove_pre_profile_state(&self, expected: &[u8]) -> RailResult<()> {
        let path = self.pre_profile_state_path();
        if super::installation::read_optional_regular(&path, expected.len() as u64)?.as_deref() != Some(expected) {
            return Err(RailError::message(
                "unbound pre-profile cache state changed before its authorized removal",
            ));
        }
        remove_file_durable(&path)
    }

    fn load_transaction(&self) -> RailResult<Option<(Vec<u8>, ProfileTransaction)>> {
        validate_existing_layout(&self.root)?;
        let Some(bytes) = super::installation::read_optional_regular(&self.transaction_path(), MAX_TRANSACTION_BYTES)?
        else {
            return Ok(None);
        };
        let transaction: ProfileTransaction =
            serde_json::from_slice(&bytes).map_err(|_| RailError::message("cache profile transaction is malformed"))?;
        transaction.validate()?;
        if encode_canonical(&transaction, MAX_TRANSACTION_BYTES)? != bytes {
            return Err(RailError::message(
                "cache profile transaction is not canonically encoded",
            ));
        }
        Ok(Some((bytes, transaction)))
    }

    fn create_layout(&self) -> RailResult<()> {
        let owner = self
            .root
            .parent()
            .ok_or_else(|| RailError::message("cache profile store has no owner directory"))?;
        super::installation::create_private_directory(owner)?;
        super::installation::create_private_directory(&self.root)?;
        for directory in [BINDINGS_DIRECTORY, PROFILES_DIRECTORY, STATE_DIRECTORY] {
            super::installation::create_private_directory(&self.root.join(directory))?;
        }
        validate_existing_layout(&self.root)
    }

    fn lock(&self) -> RailResult<fs::File> {
        self.create_layout()?;
        let path = self.root.join(STORE_LOCK_FILE);
        let file = crate::utils::open_cache_lock_file(&path, true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "cache profile registry lock is not a private regular file",
            ));
        }
        file.lock()?;
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "cache profile registry lock changed while it was acquired",
            ));
        }
        Ok(file)
    }

    fn write(&self, relative: &Path, bytes: &[u8]) -> RailResult<()> {
        super::installation::write_private_atomic(&self.path(relative)?, bytes)
    }

    fn remove(&self, relative: &Path, expected: &[u8]) -> RailResult<()> {
        let path = self.path(relative)?;
        if super::installation::read_optional_regular(&path, expected.len() as u64)?.as_deref() != Some(expected) {
            return Err(RailError::message(
                "cache profile state changed before its authorized removal",
            ));
        }
        remove_file_durable(&path)
    }

    fn reconcile(&self, transaction: &ProfileTransaction) -> RailResult<()> {
        transaction.validate()?;
        for mutation in &transaction.mutations {
            let current = self.read(&mutation.relative_path, MAX_PROFILE_BYTES)?;
            if current != mutation.before && current != mutation.after {
                return Err(RailError::message(
                    "cache profile state diverged from its interrupted transaction",
                ));
            }
        }
        for mutation in &transaction.mutations {
            let current = self.read(&mutation.relative_path, MAX_PROFILE_BYTES)?;
            if current == mutation.after {
                continue;
            }
            match &mutation.after {
                Some(bytes) => self.write(&mutation.relative_path, bytes)?,
                None => self.remove(
                    &mutation.relative_path,
                    mutation
                        .before
                        .as_deref()
                        .ok_or_else(|| RailError::message("cache profile removal has no prior state"))?,
                )?,
            }
        }
        Ok(())
    }
}

/// Plan one exact enrollment without mutating machine state.
pub(crate) fn plan_setup(
    cargo_home: &Path,
    workspace_root: &Path,
    installation_authority: &str,
    request: ProfileSetupRequest<'_>,
    pre_profile: Option<PreProfileSetupInput>,
) -> RailResult<ProfileSetupPlan> {
    let identity = capture_workspace_identity(workspace_root)?;
    let store = ProfileStore::new(cargo_home)?;
    let loaded_transaction = store.load_transaction()?;
    let (transaction_before, recovery) = loaded_transaction
        .map(|(bytes, transaction)| (Some(bytes), Some(transaction)))
        .unwrap_or_default();
    let pre_profile_before = store.read_pre_profile_state()?;
    let pre_profile_after = match pre_profile {
        Some(pre_profile) => {
            let desired = UnboundPreProfileState {
                version: 1,
                installation_authority: pre_profile.installation_authority,
                cache: pre_profile.cache,
                remote: pre_profile.remote,
                root_portability: pre_profile.root_portability,
            };
            desired.validate()?;
            let bytes = encode_canonical(&desired, MAX_PROFILE_BYTES)?;
            if let Some(existing) = pre_profile_before.as_deref() {
                let retained = decode_pre_profile_state(existing)?;
                if retained != desired {
                    return Err(RailError::message(
                        "retained pre-profile cache state conflicts with the v0.25 installation receipt",
                    ));
                }
            }
            Some(bytes)
        }
        None => pre_profile_before.clone(),
    };
    let binding_relative = ProfileStore::binding_relative(&identity.physical_identity);
    let binding_before = store.read_effective(&binding_relative, MAX_BINDING_BYTES, recovery.as_ref())?;
    let existing_binding = binding_before.as_deref().map(decode_binding).transpose()?;

    if let Some(binding) = &existing_binding
        && binding.canonical_root != identity.canonical_root
        && request.requested_profile.is_none()
    {
        return Err(RailError::with_help(
            "the enrolled workspace moved or its canonical spelling changed",
            format!(
                "rerun with `cargo rail cache setup --profile {}` to explicitly rebind it",
                binding.profile_id
            ),
        ));
    }
    let requested_profile = request.requested_profile.map(validate_identity).transpose()?;
    if let (Some(binding), Some(requested)) = (&existing_binding, requested_profile)
        && binding.profile_id != requested
    {
        return Err(RailError::with_help(
            "the workspace is already bound to another cache profile",
            "detach the current workspace profile before explicitly binding a different profile",
        ));
    }
    let profile_id = existing_binding
        .as_ref()
        .map(|binding| binding.profile_id.clone())
        .or_else(|| requested_profile.map(str::to_string))
        .map_or_else(super::installation::random_authority, Ok)?;
    let profile_relative = ProfileStore::profile_relative(&profile_id);
    let profile_before = store.read_effective(&profile_relative, MAX_PROFILE_BYTES, recovery.as_ref())?;
    let existing_profile = profile_before.as_deref().map(decode_profile).transpose()?;
    if requested_profile.is_some() && existing_profile.is_none() {
        return Err(RailError::message(
            "the explicitly selected cache profile does not exist",
        ));
    }
    if let Some(profile) = &existing_profile
        && profile.owner_installation != installation_authority
    {
        return Err(RailError::message(
            "the selected cache profile belongs to another installation authority",
        ));
    }

    let cache_base = request
        .local_dir
        .map(|path| super::installation::resolve_requested_path(workspace_root, path))
        .transpose()?
        .or_else(|| {
            existing_profile
                .as_ref()
                .map(|profile| profile.cache.base().to_path_buf())
        })
        .unwrap_or_else(|| cargo_home.to_path_buf());
    let max_bytes = request
        .max_bytes
        .or_else(|| existing_profile.as_ref().map(|profile| profile.cache.max_bytes()))
        .unwrap_or(DEFAULT_CACHE_MAX_BYTES);
    let trust_domain = existing_profile
        .as_ref()
        .and_then(|profile| profile.cache.trust_domain().map(str::to_string))
        .map_or_else(super::installation::random_authority, Ok)?;
    let cache = LocalCacheSelection::new(cache_base, max_bytes, Some(trust_domain))?;
    let remote = if request.local_only {
        None
    } else if let Some(remote_url) = request.remote_url {
        let selection = RemoteCacheSelection::parse(remote_url, request.remote_mode, request.remote_environment)
            .map_err(|error| RailError::message(format!("remote cache URL is invalid: {error}")))?;
        Some(InstalledRemoteCache::from_selection(&selection))
    } else if request.remote_mode.is_some() || !request.remote_environment.is_empty() {
        return Err(RailError::message(
            "remote cache mode and environment policy require --remote URL",
        ));
    } else {
        existing_profile.as_ref().and_then(|profile| profile.remote.clone())
    };
    let root_portability = match request.root_portability {
        Some("physical") => RootPortability::Physical,
        Some("remap") if remote.is_some() => RootPortability::Remap,
        Some("remap") => {
            return Err(RailError::message(
                "root portability remapping requires an installed remote cache authority",
            ));
        }
        Some(mode) => {
            return Err(RailError::message(format!(
                "unsupported root portability mode '{mode}'"
            )));
        }
        None if request.local_only => RootPortability::Physical,
        None => existing_profile
            .as_ref()
            .map_or(RootPortability::Physical, |profile| profile.root_portability),
    };

    let mut roots = existing_profile
        .as_ref()
        .map_or_else(Vec::new, |profile| profile.roots.clone());
    if let Some(root) = roots
        .iter_mut()
        .find(|root| root.physical_identity == identity.physical_identity)
    {
        root.canonical_root.clone_from(&identity.canonical_root);
    } else {
        if roots.len() >= MAX_PROFILE_ROOTS {
            return Err(RailError::message("cache profile workspace binding limit was reached"));
        }
        roots.push(WorkspaceBinding {
            canonical_root: identity.canonical_root.clone(),
            physical_identity: identity.physical_identity.clone(),
        });
    }
    roots.sort_by(|left, right| left.physical_identity.cmp(&right.physical_identity));
    let mut desired = InstalledCacheProfile {
        version: PROFILE_VERSION,
        state: ProfileState::Active,
        profile_id: profile_id.clone(),
        generation: existing_profile
            .as_ref()
            .map(|profile| profile.generation.clone())
            .map_or_else(super::installation::random_authority, Ok)?,
        roots,
        cache,
        remote,
        root_portability,
        owner_installation: installation_authority.to_string(),
    };
    if existing_profile.as_ref().is_some_and(|existing| {
        let mut without_generation = desired.clone();
        without_generation.generation.clone_from(&existing.generation);
        &without_generation != existing
    }) {
        desired.generation = super::installation::random_authority()?;
    }
    desired.validate()?;
    let desired_binding = ProfileBindingRecord {
        version: BINDING_VERSION,
        physical_identity: identity.physical_identity.clone(),
        canonical_root: identity.canonical_root.clone(),
        profile_id,
    };
    desired_binding.validate()?;
    let profile_after = encode_canonical(&desired, MAX_PROFILE_BYTES)?;
    let binding_after = encode_canonical(&desired_binding, MAX_BINDING_BYTES)?;
    let mut mutations = Vec::new();
    if profile_before.as_deref() != Some(profile_after.as_slice()) {
        mutations.push(ProfileFileMutation {
            relative_path: profile_relative,
            before: profile_before,
            after: Some(profile_after),
        });
    }
    if binding_before.as_deref() != Some(binding_after.as_slice()) {
        mutations.push(ProfileFileMutation {
            relative_path: binding_relative,
            before: binding_before,
            after: Some(binding_after),
        });
    }
    let pending = recovery.is_some() || !mutations.is_empty();
    Ok(ProfileSetupPlan {
        store,
        transaction_before,
        pre_profile_before: pre_profile_before.clone(),
        pre_profile_after: pre_profile_after.clone(),
        pending: pending || pre_profile_before != pre_profile_after,
        mutations,
        desired: desired.select_root(&identity)?,
    })
}

/// Apply one profile plan under the registry transaction lock.
pub(crate) fn apply_setup(plan: &ProfileSetupPlan) -> RailResult<InstalledCacheProfile> {
    let _lock = plan.store.lock()?;
    let live_transaction = plan.store.load_transaction()?;
    if live_transaction.as_ref().map(|(bytes, _)| bytes.as_slice()) != plan.transaction_before.as_deref() {
        return Err(RailError::message(
            "cache profile transaction changed after setup planning",
        ));
    }
    let _lifecycle = lock_mutation_profiles(
        &plan.store,
        live_transaction.as_ref().map(|(_, transaction)| transaction),
        Some(plan.desired.profile_id()),
        true,
    )?;
    if let Some((_, transaction)) = live_transaction {
        plan.store.reconcile(&transaction)?;
        remove_file_durable(&plan.store.transaction_path())?;
    }
    if plan.store.read_pre_profile_state()? != plan.pre_profile_before {
        return Err(RailError::message(
            "unbound pre-profile cache state changed after setup planning",
        ));
    }
    if plan.pre_profile_before != plan.pre_profile_after
        && let Some(bytes) = &plan.pre_profile_after
    {
        super::installation::write_private_atomic(&plan.store.pre_profile_state_path(), bytes)?;
    }
    for mutation in &plan.mutations {
        if plan.store.read(&mutation.relative_path, MAX_PROFILE_BYTES)? != mutation.before {
            return Err(RailError::message("cache profile state changed after setup planning"));
        }
    }
    if !plan.mutations.is_empty() {
        let transaction = ProfileTransaction {
            version: TRANSACTION_VERSION,
            transaction_id: super::installation::random_authority()?,
            mutations: plan.mutations.clone(),
        };
        transaction.validate()?;
        let bytes = encode_canonical(&transaction, MAX_TRANSACTION_BYTES)?;
        super::installation::write_private_atomic(&plan.store.transaction_path(), &bytes)?;
        #[cfg(debug_assertions)]
        if std::env::var_os("CARGO_RAIL_TEST_PROFILE_TRANSACTION_FAULT").as_deref()
            == Some(std::ffi::OsStr::new("after_journal"))
        {
            return Err(RailError::message(
                "injected cache profile interruption after transaction journal",
            ));
        }
        plan.store.reconcile(&transaction)?;
        remove_file_durable(&plan.store.transaction_path())?;
    }
    let selected = select(&plan.store, plan.desired.selected_root())?
        .ok_or_else(|| RailError::message("cache profile enrollment was not materialized"))?;
    if selected != plan.desired {
        return Err(RailError::message(
            "materialized cache profile does not match the setup plan",
        ));
    }
    Ok(selected)
}

pub(crate) fn plan_detach(cargo_home: &Path, workspace_root: &Path) -> RailResult<ProfileDetachPlan> {
    let identity = capture_workspace_identity(workspace_root)?;
    let store = ProfileStore::new(cargo_home)?;
    let loaded_transaction = store.load_transaction()?;
    let (transaction_before, recovery) = loaded_transaction
        .map(|(bytes, transaction)| (Some(bytes), Some(transaction)))
        .unwrap_or_default();
    let binding_relative = ProfileStore::binding_relative(&identity.physical_identity);
    let binding_before = store.read_effective(&binding_relative, MAX_BINDING_BYTES, recovery.as_ref())?;
    let Some(binding) = binding_before.as_deref().map(decode_binding).transpose()? else {
        return Ok(ProfileDetachPlan {
            store,
            transaction_before,
            mutations: Vec::new(),
            profile_id: String::new(),
            pending: recovery.is_some(),
        });
    };
    if binding.canonical_root != identity.canonical_root || binding.physical_identity != identity.physical_identity {
        return Err(RailError::message(
            "workspace cache profile binding does not match the selected root",
        ));
    }
    let profile_relative = ProfileStore::profile_relative(&binding.profile_id);
    let profile_before = store
        .read_effective(&profile_relative, MAX_PROFILE_BYTES, recovery.as_ref())?
        .ok_or_else(|| RailError::message("workspace cache profile record is missing"))?;
    let mut profile = decode_profile(&profile_before)?;
    let root_index = profile
        .roots
        .iter()
        .position(|root| root.physical_identity == identity.physical_identity)
        .ok_or_else(|| RailError::message("cache profile does not own the selected workspace"))?;
    profile.roots.remove(root_index);
    profile.state = if profile.roots.is_empty() {
        ProfileState::Detached
    } else {
        ProfileState::Active
    };
    profile.generation = super::installation::random_authority()?;
    profile.validate()?;
    let profile_after = encode_canonical(&profile, MAX_PROFILE_BYTES)?;
    let mutations = vec![
        ProfileFileMutation {
            relative_path: profile_relative,
            before: Some(profile_before),
            after: Some(profile_after),
        },
        ProfileFileMutation {
            relative_path: binding_relative,
            before: binding_before,
            after: None,
        },
    ];
    Ok(ProfileDetachPlan {
        store,
        transaction_before,
        mutations,
        profile_id: binding.profile_id,
        pending: true,
    })
}

pub(crate) fn apply_detach(plan: &ProfileDetachPlan) -> RailResult<()> {
    let _lock = plan.store.lock()?;
    let live_transaction = plan.store.load_transaction()?;
    if live_transaction.as_ref().map(|(bytes, _)| bytes.as_slice()) != plan.transaction_before.as_deref() {
        return Err(RailError::message(
            "cache profile transaction changed after detach planning",
        ));
    }
    let selected_profile = (!plan.profile_id.is_empty()).then_some(plan.profile_id.as_str());
    let _lifecycle = lock_mutation_profiles(
        &plan.store,
        live_transaction.as_ref().map(|(_, transaction)| transaction),
        selected_profile,
        false,
    )?;
    if let Some((_, transaction)) = live_transaction {
        plan.store.reconcile(&transaction)?;
        remove_file_durable(&plan.store.transaction_path())?;
    }
    if plan.mutations.is_empty() {
        return Ok(());
    }
    for mutation in &plan.mutations {
        if plan.store.read(&mutation.relative_path, MAX_PROFILE_BYTES)? != mutation.before {
            return Err(RailError::message("cache profile state changed after detach planning"));
        }
    }
    let transaction = ProfileTransaction {
        version: TRANSACTION_VERSION,
        transaction_id: super::installation::random_authority()?,
        mutations: plan.mutations.clone(),
    };
    transaction.validate()?;
    let bytes = encode_canonical(&transaction, MAX_TRANSACTION_BYTES)?;
    super::installation::write_private_atomic(&plan.store.transaction_path(), &bytes)?;
    plan.store.reconcile(&transaction)?;
    remove_file_durable(&plan.store.transaction_path())?;
    Ok(())
}

pub(crate) fn plan_removal(cargo_home: &Path, profile_id: &str) -> RailResult<ProfileRemovalPlan> {
    validate_identity(profile_id)?;
    let store = ProfileStore::new(cargo_home)?;
    if store.load_transaction()?.is_some() {
        return Err(RailError::with_help(
            "cache profile registry has an interrupted transaction",
            "rerun `cargo rail cache setup` for the affected workspace before removing a profile",
        ));
    }
    let relative = ProfileStore::profile_relative(profile_id);
    let Some(profile_before) = store.read(&relative, MAX_PROFILE_BYTES)? else {
        return Ok(ProfileRemovalPlan {
            state_root: store.profile_state_directory(profile_id),
            store,
            profile_id: profile_id.to_string(),
            profile_before: None,
            cache_root: None,
            bytes: 0,
        });
    };
    let profile = decode_profile(&profile_before)?;
    if profile.state != ProfileState::Detached || !profile.roots.is_empty() {
        return Err(RailError::with_help(
            "cache profile removal requires a detached profile with no enrolled roots",
            "detach every enrolled workspace before removing the profile",
        ));
    }
    ensure_no_profile_bindings(&store, profile_id)?;
    let cache_root = profile.cache.configured_root()?;
    let cache_bytes = cache_root
        .as_deref()
        .map(|root| crate::cache::cas::status_at_with_max(root, profile.cache.max_bytes()))
        .transpose()?
        .flatten()
        .map_or(0, |status| status.bytes);
    let state_root = store.profile_state_directory(profile_id);
    let state_bytes = super::path_status(&state_root)?.map_or(0, |status| status.0);
    Ok(ProfileRemovalPlan {
        store,
        profile_id: profile_id.to_string(),
        profile_before: Some(profile_before),
        cache_root,
        state_root,
        bytes: cache_bytes
            .checked_add(state_bytes)
            .ok_or_else(|| RailError::message("cache profile removal byte count overflow"))?,
    })
}

pub(crate) fn apply_removal(plan: &ProfileRemovalPlan) -> RailResult<()> {
    let Some(profile_before) = plan.profile_before.as_deref() else {
        return Ok(());
    };
    let _lock = plan.store.lock()?;
    if plan.store.load_transaction()?.is_some() {
        return Err(RailError::message(
            "cache profile registry transaction changed after removal planning",
        ));
    }
    let relative = ProfileStore::profile_relative(&plan.profile_id);
    if plan.store.read(&relative, MAX_PROFILE_BYTES)?.as_deref() != Some(profile_before) {
        return Err(RailError::message("cache profile changed after removal planning"));
    }
    ensure_no_profile_bindings(&plan.store, &plan.profile_id)?;
    if let Some(root) = &plan.cache_root {
        crate::cache::cas::remove_owned_root_at(root)?;
    }
    if super::remove_owned_tree(&plan.state_root)?
        && let Some(parent) = plan.state_root.parent()
    {
        sync_directory(parent)?;
    }
    plan.store.remove(&relative, profile_before)?;
    Ok(())
}

/// Select only the profile bound to this exact physical workspace.
pub(crate) fn load(cargo_home: &Path, workspace_root: &Path) -> RailResult<Option<InstalledCacheProfile>> {
    let store = ProfileStore::new(cargo_home)?;
    select(&store, workspace_root)
}

pub(crate) fn load_locked(
    cargo_home: &Path,
    workspace_root: &Path,
) -> RailResult<Option<(InstalledCacheProfile, ProfileLifecycleLock)>> {
    let store = ProfileStore::new(cargo_home)?;
    let Some(profile) = select(&store, workspace_root)? else {
        return Ok(None);
    };
    let lock = ProfileLifecycleLock {
        _file: Arc::new(store.lifecycle_lock(profile.profile_id(), false, false)?),
    };
    let repeated = select(&store, workspace_root)?
        .ok_or_else(|| RailError::message("cache profile was detached while runtime selection was acquired"))?;
    if repeated != profile {
        return Err(RailError::message(
            "cache profile changed while runtime selection was acquired",
        ));
    }
    Ok(Some((repeated, lock)))
}

/// Select one profile and exclude compiler, coordinator, cleanup, and policy work.
pub(crate) fn load_exclusive(
    cargo_home: &Path,
    workspace_root: &Path,
) -> RailResult<Option<(InstalledCacheProfile, ProfileLifecycleLock)>> {
    let store = ProfileStore::new(cargo_home)?;
    let Some(profile) = select(&store, workspace_root)? else {
        return Ok(None);
    };
    let lock = ProfileLifecycleLock {
        _file: Arc::new(store.lifecycle_lock(profile.profile_id(), false, true)?),
    };
    let repeated = select(&store, workspace_root)?
        .ok_or_else(|| RailError::message("cache profile was detached while exclusive authority was acquired"))?;
    if repeated != profile {
        return Err(RailError::message(
            "cache profile changed while exclusive authority was acquired",
        ));
    }
    Ok(Some((repeated, lock)))
}

/// Exclude runtime and lifecycle work for every profile while global components change.
pub(crate) fn lock_all_exclusive(cargo_home: &Path) -> RailResult<ProfileRegistryWriteGuard> {
    let store = ProfileStore::new(cargo_home)?;
    let registry = store.lock()?;
    if store.load_transaction()?.is_some() {
        return Err(RailError::with_help(
            "cache profile registry has an interrupted transaction",
            "rerun `cargo rail cache setup` for the affected workspace before changing the global installation",
        ));
    }
    let profiles = load_all(cargo_home)?;
    let mut locks = Vec::with_capacity(profiles.len());
    for profile in &profiles {
        locks.push(store.lifecycle_lock(profile.profile_id(), false, true)?);
    }
    if load_all(cargo_home)? != profiles {
        return Err(RailError::message(
            "cache profiles changed while global lifecycle authority was acquired",
        ));
    }
    Ok(ProfileRegistryWriteGuard {
        _registry: registry,
        _profiles: locks,
    })
}

/// Load an exact profile capability for a coordinator child process.
pub(crate) fn load_by_id(cargo_home: &Path, profile_id: &str, generation: &str) -> RailResult<InstalledCacheProfile> {
    validate_identity(profile_id)?;
    validate_identity(generation)?;
    let store = ProfileStore::new(cargo_home)?;
    let relative = ProfileStore::profile_relative(profile_id);
    let bytes = store
        .read(&relative, MAX_PROFILE_BYTES)?
        .ok_or_else(|| RailError::message("selected cache profile is unavailable"))?;
    let profile = decode_profile(&bytes)?;
    if profile.state != ProfileState::Active {
        return Err(RailError::message("selected cache profile is detached"));
    }
    if profile.generation != generation {
        return Err(RailError::message("selected cache profile generation changed"));
    }
    Ok(profile)
}

pub(crate) fn load_coordinator_capability(cargo_home: &Path, capability: &str) -> RailResult<InstalledCacheProfile> {
    let (profile_id, generation) = capability
        .split_once(':')
        .ok_or_else(|| RailError::message("cache profile coordinator capability is invalid"))?;
    load_by_id(cargo_home, profile_id, generation)
}

pub(crate) fn load_locked_coordinator_capability(
    cargo_home: &Path,
    capability: &str,
) -> RailResult<(InstalledCacheProfile, ProfileLifecycleLock)> {
    let profile = load_coordinator_capability(cargo_home, capability)?;
    let store = ProfileStore::new(cargo_home)?;
    let lock = ProfileLifecycleLock {
        _file: Arc::new(store.lifecycle_lock(profile.profile_id(), false, false)?),
    };
    let repeated = load_coordinator_capability(cargo_home, capability)?;
    if repeated != profile {
        return Err(RailError::message(
            "cache profile changed while coordinator authority was acquired",
        ));
    }
    Ok((repeated, lock))
}

pub(crate) fn state_directory(cargo_home: &Path, profile_id: &str) -> RailResult<PathBuf> {
    validate_identity(profile_id)?;
    Ok(ProfileStore::new(cargo_home)?.profile_state_directory(profile_id))
}

pub(crate) fn pre_profile_state_status(cargo_home: &Path) -> RailResult<Option<PreProfileStateStatus>> {
    let store = ProfileStore::new(cargo_home)?;
    let Some(bytes) = store.read_pre_profile_state()? else {
        return Ok(None);
    };
    let state = decode_pre_profile_state(&bytes)?;
    let remote = state
        .remote
        .as_ref()
        .map(InstalledRemoteCache::selection)
        .transpose()
        .map_err(|error| RailError::message(format!("unbound pre-profile remote policy is invalid: {error}")))?;
    Ok(Some(PreProfileStateStatus {
        state: "unbound",
        cache_base: state.cache.base().to_string_lossy().into_owned(),
        max_bytes: state.cache.max_bytes(),
        trust_domain: state.cache.trust_domain().map(str::to_string),
        remote_authority: remote
            .as_ref()
            .map(|selection| selection.authority().as_str().to_string()),
        remote_mode: remote.as_ref().map(|selection| selection.mode().as_str()),
        root_portability: state.root_portability.as_str(),
    }))
}

pub(crate) fn plan_pre_profile_state_removal(cargo_home: &Path) -> RailResult<PreProfileStateRemovalPlan> {
    let store = ProfileStore::new(cargo_home)?;
    if store.load_transaction()?.is_some() {
        return Err(RailError::with_help(
            "cache profile registry has an interrupted transaction",
            "rerun `cargo rail cache setup` for the affected workspace before removing pre-profile state",
        ));
    }
    let Some(state_before) = store.read_pre_profile_state()? else {
        return Ok(PreProfileStateRemovalPlan {
            store,
            state_before: None,
            cache_root: None,
            bytes: 0,
        });
    };
    let state = decode_pre_profile_state(&state_before)?;
    let cache_root = state.cache.configured_root()?;
    ensure_pre_profile_cache_is_unbound(&store, cache_root.as_deref())?;
    let bytes = cache_root
        .as_deref()
        .map(|root| crate::cache::cas::status_at_with_max(root, state.cache.max_bytes()))
        .transpose()?
        .flatten()
        .map_or(0, |status| status.bytes);
    Ok(PreProfileStateRemovalPlan {
        store,
        state_before: Some(state_before),
        cache_root,
        bytes,
    })
}

pub(crate) fn apply_pre_profile_state_removal(plan: &PreProfileStateRemovalPlan) -> RailResult<()> {
    let Some(state_before) = plan.state_before.as_deref() else {
        return Ok(());
    };
    let _lock = plan.store.lock()?;
    if plan.store.load_transaction()?.is_some() {
        return Err(RailError::message(
            "cache profile registry transaction changed after pre-profile cleanup planning",
        ));
    }
    if plan.store.read_pre_profile_state()?.as_deref() != Some(state_before) {
        return Err(RailError::message(
            "unbound pre-profile cache state changed after cleanup planning",
        ));
    }
    ensure_pre_profile_cache_is_unbound(&plan.store, plan.cache_root.as_deref())?;
    if let Some(root) = &plan.cache_root {
        crate::cache::cas::remove_owned_root_at(root)?;
    }
    plan.store.remove_pre_profile_state(state_before)?;
    Ok(())
}

pub(crate) fn list(cargo_home: &Path) -> RailResult<Vec<ProfileStatus>> {
    load_all(cargo_home)?
        .into_iter()
        .map(|profile| {
            let remote = profile.remote_selection()?;
            Ok(ProfileStatus {
                profile_id: profile.profile_id.clone(),
                generation: profile.generation.clone(),
                state: match profile.state {
                    ProfileState::Active => "active",
                    ProfileState::Detached => "detached",
                },
                roots: profile
                    .roots
                    .iter()
                    .map(|root| root.canonical_root.to_string_lossy().into_owned())
                    .collect(),
                cache_base: profile.cache.base().to_string_lossy().into_owned(),
                trust_domain: profile.cache.trust_domain().unwrap_or_default().to_string(),
                max_bytes: profile.cache.max_bytes(),
                remote_authority: remote
                    .as_ref()
                    .map(|selection| selection.authority().as_str().to_string()),
                remote_mode: remote.as_ref().map(|selection| selection.mode().as_str()),
                root_portability: profile.root_portability.as_str(),
            })
        })
        .collect()
}

pub(crate) fn load_all(cargo_home: &Path) -> RailResult<Vec<InstalledCacheProfile>> {
    let store = ProfileStore::new(cargo_home)?;
    validate_existing_layout(&store.root)?;
    let directory = store.root.join(PROFILES_DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        if profiles.len() >= MAX_PROFILES {
            return Err(RailError::message("cache profile registry exceeds its entry bound"));
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("cache profile record name is not valid UTF-8"))?;
        let profile_id = name
            .strip_suffix(".json")
            .ok_or_else(|| RailError::message("cache profile registry contains an unknown entry"))?;
        validate_identity(profile_id)?;
        let relative = ProfileStore::profile_relative(profile_id);
        let bytes = store
            .read(&relative, MAX_PROFILE_BYTES)?
            .ok_or_else(|| RailError::message("cache profile record changed while it was listed"))?;
        let profile = decode_profile(&bytes)?;
        if profile.profile_id != profile_id {
            return Err(RailError::message(
                "cache profile record name does not match its identity",
            ));
        }
        profiles.push(profile);
    }
    profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    Ok(profiles)
}

fn ensure_no_profile_bindings(store: &ProfileStore, profile_id: &str) -> RailResult<()> {
    let directory = store.root.join(BINDINGS_DIRECTORY);
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut count = 0usize;
    for entry in entries {
        count = count.saturating_add(1);
        if count > MAX_PROFILES.saturating_mul(MAX_PROFILE_ROOTS) {
            return Err(RailError::message(
                "cache profile binding registry exceeds its entry bound",
            ));
        }
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("cache profile binding name is not valid UTF-8"))?;
        let identity = name
            .strip_suffix(".json")
            .ok_or_else(|| RailError::message("cache profile binding registry contains an unknown entry"))?;
        validate_identity(identity)?;
        let relative = ProfileStore::binding_relative(identity);
        let bytes = store
            .read(&relative, MAX_BINDING_BYTES)?
            .ok_or_else(|| RailError::message("cache profile binding changed while it was inspected"))?;
        if decode_binding(&bytes)?.profile_id == profile_id {
            return Err(RailError::message(
                "cache profile still has an enrolled workspace binding",
            ));
        }
    }
    Ok(())
}

fn ensure_pre_profile_cache_is_unbound(store: &ProfileStore, cache_root: Option<&Path>) -> RailResult<()> {
    let Some(cache_root) = cache_root else {
        return Ok(());
    };
    let directory = store.root.join(PROFILES_DIRECTORY);
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut count = 0usize;
    for entry in entries {
        count = count.saturating_add(1);
        if count > MAX_PROFILES {
            return Err(RailError::message("cache profile registry exceeds its entry bound"));
        }
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("cache profile record name is not valid UTF-8"))?;
        let profile_id = name
            .strip_suffix(".json")
            .ok_or_else(|| RailError::message("cache profile registry contains an unknown entry"))?;
        validate_identity(profile_id)?;
        let relative = ProfileStore::profile_relative(profile_id);
        let bytes = store
            .read(&relative, MAX_PROFILE_BYTES)?
            .ok_or_else(|| RailError::message("cache profile record changed while it was inspected"))?;
        let profile = decode_profile(&bytes)?;
        if profile.cache.configured_root()?.as_deref() == Some(cache_root) {
            return Err(RailError::message(
                "unbound pre-profile CAS is selected by an installed workspace profile",
            ));
        }
    }
    Ok(())
}

fn lock_mutation_profiles(
    store: &ProfileStore,
    recovery: Option<&ProfileTransaction>,
    selected_profile: Option<&str>,
    create_selected: bool,
) -> RailResult<Vec<fs::File>> {
    let mut profile_ids = BTreeSet::new();
    if let Some(transaction) = recovery {
        profile_ids.insert(transaction.profile_id()?.to_string());
    }
    if let Some(profile_id) = selected_profile {
        profile_ids.insert(profile_id.to_string());
    }
    profile_ids
        .into_iter()
        .map(|profile_id| {
            store.lifecycle_lock(
                &profile_id,
                create_selected && selected_profile == Some(profile_id.as_str()),
                true,
            )
        })
        .collect()
}

fn select(store: &ProfileStore, workspace_root: &Path) -> RailResult<Option<InstalledCacheProfile>> {
    let identity = capture_workspace_identity(workspace_root)?;
    let binding_relative = ProfileStore::binding_relative(&identity.physical_identity);
    let Some(binding_bytes) = store.read(&binding_relative, MAX_BINDING_BYTES)? else {
        return Ok(None);
    };
    let binding = decode_binding(&binding_bytes)?;
    if binding.physical_identity != identity.physical_identity {
        return Err(RailError::message(
            "workspace cache profile binding names another physical root",
        ));
    }
    if binding.canonical_root != identity.canonical_root {
        return Err(RailError::with_help(
            "the enrolled workspace moved or its canonical spelling changed",
            format!(
                "explicitly re-enroll it with `cargo rail cache setup --profile {}`",
                binding.profile_id
            ),
        ));
    }
    let profile_relative = ProfileStore::profile_relative(&binding.profile_id);
    let profile_bytes = store
        .read(&profile_relative, MAX_PROFILE_BYTES)?
        .ok_or_else(|| RailError::message("workspace cache profile record is missing"))?;
    decode_profile(&profile_bytes)?.select_root(&identity).map(Some)
}

fn decode_profile(bytes: &[u8]) -> RailResult<InstalledCacheProfile> {
    let profile: InstalledCacheProfile =
        serde_json::from_slice(bytes).map_err(|_| RailError::message("installed cache profile is malformed"))?;
    profile.validate()?;
    if encode_canonical(&profile, MAX_PROFILE_BYTES)? != bytes {
        return Err(RailError::message("installed cache profile is not canonically encoded"));
    }
    Ok(profile)
}

fn decode_binding(bytes: &[u8]) -> RailResult<ProfileBindingRecord> {
    let binding: ProfileBindingRecord = serde_json::from_slice(bytes)
        .map_err(|_| RailError::message("workspace cache profile binding is malformed"))?;
    binding.validate()?;
    if encode_canonical(&binding, MAX_BINDING_BYTES)? != bytes {
        return Err(RailError::message(
            "workspace cache profile binding is not canonically encoded",
        ));
    }
    Ok(binding)
}

fn decode_pre_profile_state(bytes: &[u8]) -> RailResult<UnboundPreProfileState> {
    let state: UnboundPreProfileState = serde_json::from_slice(bytes)
        .map_err(|_| RailError::message("unbound pre-profile cache state is malformed"))?;
    state.validate()?;
    if encode_canonical(&state, MAX_PROFILE_BYTES)? != bytes {
        return Err(RailError::message(
            "unbound pre-profile cache state is not canonically encoded",
        ));
    }
    Ok(state)
}

fn encode_canonical<T: Serialize>(value: &T, maximum: u64) -> RailResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > maximum {
        return Err(RailError::message("cache profile private state exceeds its byte bound"));
    }
    Ok(bytes)
}

fn capture_workspace_identity(workspace_root: &Path) -> RailResult<WorkspaceIdentity> {
    let canonical_root = crate::utils::canonicalize_existing(workspace_root).map_err(|error| {
        RailError::message(format!(
            "failed to resolve cache profile workspace '{}': {error}",
            workspace_root.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(&canonical_root)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message("cache profile workspace is not a real directory"));
    }
    let manifest = canonical_root.join("Cargo.toml");
    let manifest_metadata = fs::symlink_metadata(&manifest)?;
    if !manifest_metadata.is_file() || crate::utils::is_symlink_or_reparse(&manifest_metadata) {
        return Err(RailError::message("cache profile workspace has no real Cargo.toml"));
    }
    let physical_identity = physical_directory_identity(&canonical_root)?;
    if crate::utils::canonicalize_existing(workspace_root)? != canonical_root
        || physical_directory_identity(&canonical_root)? != physical_identity
    {
        return Err(RailError::message(
            "cache profile workspace changed while its identity was captured",
        ));
    }
    Ok(WorkspaceIdentity {
        canonical_root,
        physical_identity,
    })
}

fn physical_directory_identity(path: &Path) -> RailResult<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-cache-profile-directory-v1\0");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::metadata(path)?;
        if !metadata.is_dir() {
            return Err(RailError::message("cache profile workspace is not a directory"));
        }
        hasher.update(metadata.dev().to_be_bytes());
        hasher.update(metadata.ino().to_be_bytes());
    }
    #[cfg(windows)]
    {
        let directory = crate::windows_fs::open_for_observation(path)?;
        let observation = crate::windows_fs::observe_file(&directory)?;
        crate::windows_fs::prove_local_ntfs(&directory, observation.volume_serial_number)?;
        if observation.file_attributes & 0x10 == 0 {
            return Err(RailError::message("cache profile workspace is not a directory"));
        }
        hasher.update(observation.volume_serial_number.to_be_bytes());
        hasher.update(observation.file_id.to_be_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        hasher.update(crate::utils::canonicalize_existing(path)?.to_string_lossy().as_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(crate::source::ContentDigest::from_sha256_bytes(digest).to_string())
}

fn validate_existing_layout(root: &Path) -> RailResult<()> {
    let Some(owner) = root.parent() else {
        return Err(RailError::message("cache profile store has no owner directory"));
    };
    let nested = [BINDINGS_DIRECTORY, PROFILES_DIRECTORY, STATE_DIRECTORY].map(|name| root.join(name));
    for path in [owner, root].into_iter().chain(nested.iter().map(PathBuf::as_path)) {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::with_help(
                format!(
                    "cache profile private directory '{}' is not a real directory",
                    path.display()
                ),
                "remove the hostile path manually; cargo-rail will not follow profile-state links",
            ));
        }
    }
    Ok(())
}

fn remove_file_durable(path: &Path) -> RailResult<()> {
    fs::remove_file(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("cache profile file has no parent directory"))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> RailResult<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> RailResult<()> {
    Ok(())
}

fn relative_store_identity(path: &Path) -> RailResult<(&str, &str)> {
    let mut components = path.components();
    let Some(std::path::Component::Normal(directory)) = components.next() else {
        return Err(RailError::message("cache profile private-state path is invalid"));
    };
    let directory = directory
        .to_str()
        .filter(|directory| *directory == BINDINGS_DIRECTORY || *directory == PROFILES_DIRECTORY)
        .ok_or_else(|| RailError::message("cache profile private-state path is invalid"))?;
    let Some(std::path::Component::Normal(file)) = components.next() else {
        return Err(RailError::message("cache profile private-state path is invalid"));
    };
    if components.next().is_some() {
        return Err(RailError::message("cache profile private-state path is invalid"));
    }
    let identity = file
        .to_str()
        .and_then(|file| file.strip_suffix(".json"))
        .ok_or_else(|| RailError::message("cache profile private-state path is invalid"))?;
    validate_identity(identity)?;
    Ok((directory, identity))
}

fn valid_relative_store_path(path: &Path) -> bool {
    relative_store_identity(path).is_ok()
}

fn validate_identity(value: &str) -> RailResult<&str> {
    if valid_identity(value) {
        Ok(value)
    } else {
        Err(RailError::message("cache profile identity is invalid"))
    }
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
