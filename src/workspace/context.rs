//! Unified workspace context - build once, pass everywhere
//!
//! # Design
//!
//! WorkspaceContext eliminates redundant metadata/config/graph loads by building
//! all workspace-level data structures once in main.rs, then passing by reference
//! to all commands. Snapshot-aware callers use an explicit constructor so current
//! commands do not pay capture cost before their ordered migration.
//!
//! # Performance Impact
//!
//! Before: Each command loaded metadata/config independently (50-200ms × N commands)
//! After: Single load in main, shared across all operations (50-200ms total)
//!
//! # Architecture
//!
//! ```text
//! main.rs:
//!   WorkspaceContext::build() -> &WorkspaceContext
//!   |
//!   v
//! commands/split.rs, sync.rs, etc:
//!   fn execute(ctx: &WorkspaceContext)
//! ```

use crate::cargo::multi_target_metadata::MultiTargetMetadata;
use crate::cargo::resolution::{
  ResolutionInputs, ResolutionRequest, ResolutionView, ResolutionViews, capture_target_identities,
};
use crate::compiler::cfg_eval::TargetCfgSet;
use crate::config::RailConfig;
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::graph::WorkspaceGraph;
use crate::source::{
  ContentDigest, GitWorktreeCapture, RepositoryPath, SourceEntryKind, SourceExclusions, SourceSnapshot,
};
use crate::workspace::snapshot::{CapturedLockfile, CapturedRailConfig, DerivedViews, WorkspaceSnapshot};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Cargo State (merged from cargo_state.rs)

/// Cargo state for the workspace
///
/// Provides cargo metadata and workspace information.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct CargoState {
  /// Underlying cargo metadata
  metadata: Arc<Metadata>,

  /// Cached workspace root
  workspace_root: PathBuf,

  /// Index: package name → index in metadata.packages (workspace members only)
  /// Enables O(1) package lookup instead of O(n) linear scan
  package_index: std::collections::HashMap<String, usize>,

  /// Cached set of proc-macro crate names for O(1) detection
  /// Built once during construction, used by change analysis
  proc_macro_crates: std::collections::HashSet<String>,
}

/// Cache version - increment when MetadataCache format changes
/// This ensures old cache files are automatically invalidated
const CACHE_VERSION: u32 = 2;

/// Metadata cache structure
#[derive(Serialize, Deserialize)]
struct MetadataCache {
  /// Cache format version - used to invalidate on schema changes
  #[serde(default)]
  version: u32,
  /// FNV-1a hash of Cargo.toml + Cargo.lock + all workspace member Cargo.toml manifests
  hash: u64,
  /// Cached metadata
  metadata: Metadata,
}

impl CargoState {
  /// Load cargo metadata from workspace root
  ///
  /// Uses a content-based cache (FNV-1a hash of workspace Cargo.toml/Cargo.lock + all workspace member Cargo.toml)
  /// stored in `target/cargo-rail/metadata.json` to speed up subsequent loads.
  fn load(workspace_root: &Path) -> RailResult<Self> {
    let cache_dir = super::cargo_rail_state_root(workspace_root);
    let cache_file = cache_dir.join("metadata.json");

    // Try to load from cache
    // Cache is valid only if:
    // 1. Version matches (cache format unchanged)
    // 2. Hash matches (workspace + member manifests unchanged)
    // 3. Workspace root path matches (repo hasn't been moved/copied)
    if let Some(cache) = fs::read_to_string(&cache_file)
      .ok()
      .and_then(|s| serde_json::from_str::<MetadataCache>(&s).ok())
      && cache.version == CACHE_VERSION
    {
      // Validate workspace root path matches current location
      // This handles the case where a repo is copied/moved to a different path
      let cached_root = cache.metadata.workspace_root.as_std_path();
      let current_root_canonical = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
      let cached_root_canonical = cached_root.canonicalize().unwrap_or_else(|_| cached_root.to_path_buf());

      if current_root_canonical == cached_root_canonical {
        let current_hash = compute_workspace_hash_with_members(workspace_root, &cache.metadata);
        if cache.hash == current_hash {
          // Cache hit - use cached metadata
          crate::instrumentation::record_cargo_metadata_cache_hit();
          return Ok(Self::from_metadata(Arc::new(cache.metadata)));
        }
      }
      // Hash mismatch or path mismatch - cache is stale, will reload below
    }

    // Cache miss or mismatch - load fresh metadata
    crate::instrumentation::record_cargo_metadata_load(false);
    let metadata = MetadataCommand::new()
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()?;

    let metadata = Arc::new(metadata);

    // Save to cache
    if let Ok(()) = fs::create_dir_all(&cache_dir) {
      let current_hash = compute_workspace_hash_with_members(workspace_root, &metadata);
      #[derive(Serialize)]
      struct MetadataCacheRef<'a> {
        version: u32,
        hash: u64,
        metadata: &'a Metadata,
      }
      let cache = MetadataCacheRef {
        version: CACHE_VERSION,
        hash: current_hash,
        metadata: &metadata,
      };
      match serde_json::to_string(&cache) {
        Ok(json) => {
          if let Err(e) = fs::write(&cache_file, json) {
            crate::warn!("failed to write metadata cache {}: {}", cache_file.display(), e);
          }
        }
        Err(e) => {
          crate::warn!(
            "failed to serialize metadata cache {}: {} (proceeding without cache)",
            cache_file.display(),
            e
          );
        }
      }
    }

    Ok(Self::from_metadata(metadata))
  }

  fn load_fresh(
    workspace_root: &Path,
    cargo_current_dir: &Path,
    cargo_program: &std::ffi::OsStr,
    credential_sensitive: bool,
  ) -> RailResult<Self> {
    crate::instrumentation::record_cargo_metadata_load(false);
    let metadata = MetadataCommand::new()
      .cargo_path(PathBuf::from(cargo_program))
      .current_dir(cargo_current_dir)
      .manifest_path(workspace_root.join("Cargo.toml"))
      .exec()
      .map_err(|error| {
        if credential_sensitive {
          RailError::with_help(
            "Cargo metadata failed while credential capabilities were active",
            "run cargo metadata directly for provider diagnostics; cargo-rail suppresses credential-provider output",
          )
        } else {
          error.into()
        }
      })?;
    Ok(Self::from_metadata(Arc::new(metadata)))
  }

  /// Build CargoState from metadata, constructing the package index and proc-macro cache
  fn from_metadata(metadata: Arc<Metadata>) -> Self {
    // Normalize path separators on Windows (cargo metadata uses forward slashes).
    // Using .components().collect() converts to platform-native separators without
    // adding the \\?\ prefix that canonicalize() would add.
    let workspace_root: PathBuf = metadata.workspace_root.as_std_path().components().collect();

    // Build O(1) lookup index: package name → index in metadata.packages
    let workspace_member_ids: std::collections::HashSet<_> = metadata.workspace_members.iter().collect();
    let workspace_packages: Vec<_> = metadata
      .packages
      .iter()
      .enumerate()
      .filter(|(_, pkg)| workspace_member_ids.contains(&pkg.id))
      .collect();

    let package_index = workspace_packages
      .iter()
      .map(|(idx, pkg)| (pkg.name.to_string(), *idx))
      .collect();

    // Build proc-macro crate set for O(1) detection
    let proc_macro_crates = workspace_packages
      .iter()
      .filter(|(_, pkg)| {
        pkg.targets.iter().any(|t| {
          t.kind
            .iter()
            .any(|k| matches!(k, cargo_metadata::TargetKind::ProcMacro))
        })
      })
      .map(|(_, pkg)| pkg.name.to_string())
      .collect();

    Self {
      metadata,
      workspace_root,
      package_index,
      proc_macro_crates,
    }
  }

  /// Get workspace root path
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Get all workspace members
  pub fn workspace_members(&self) -> Vec<&Package> {
    self.metadata.workspace_packages()
  }

  /// Get a specific workspace package by name (O(1) lookup)
  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self.package_index.get(name).map(|&idx| &self.metadata.packages[idx])
  }

  /// Get the underlying metadata (for compatibility)
  pub fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// Clone the shared metadata handle for another workspace-level index.
  pub(crate) fn shared_metadata(&self) -> Arc<Metadata> {
    Arc::clone(&self.metadata)
  }

  /// Check if a crate is a proc-macro crate (O(1) lookup)
  pub fn is_proc_macro(&self, crate_name: &str) -> bool {
    self.proc_macro_crates.contains(crate_name)
  }

  /// Get the set of all proc-macro crate names
  pub fn proc_macro_crates(&self) -> &std::collections::HashSet<String> {
    &self.proc_macro_crates
  }

  /// Check if a package is publishable based on Cargo.toml
  ///
  /// Returns `true` if the package can be published to a registry.
  /// Returns `false` if `publish = false` in Cargo.toml.
  ///
  /// Note: This only checks Cargo.toml. For combined Cargo.toml + rail.toml
  /// checking, use `ReleaseValidator::is_publishable()`.
  pub fn is_package_publishable(package: &Package) -> bool {
    // package.publish is Option<Vec<String>>:
    // - None = publishable (no restriction)
    // - Some([]) = publish = false (empty registry list)
    // - Some(["crates-io"]) = can publish to listed registries
    package.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true)
  }

  /// Check if a workspace member is binary-only (has `[[bin]]` targets but no library target).
  ///
  /// This is used by planner/executor flows to optionally skip crates
  /// that can't be selected by `cargo test -p <crate>` (no library target).
  pub fn is_binary_only(&self, crate_name: &str) -> bool {
    let Some(pkg) = self.get_package(crate_name) else {
      return false;
    };

    let mut has_bin = false;
    let mut has_lib_like = false;

    for target in &pkg.targets {
      for kind in &target.kind {
        match kind {
          cargo_metadata::TargetKind::Bin => has_bin = true,
          cargo_metadata::TargetKind::Lib | cargo_metadata::TargetKind::ProcMacro => has_lib_like = true,
          _ => {}
        }
      }
    }

    has_bin && !has_lib_like
  }
}

/// Compute a content-based hash of workspace manifests
///
/// Uses FNV-1a hash of workspace Cargo.toml/Cargo.lock plus all workspace member Cargo.toml
/// manifests to detect changes that invalidate cached cargo metadata.
fn compute_workspace_hash_with_members(workspace_root: &Path, metadata: &Metadata) -> u64 {
  const FNV_PRIME: u64 = 0x100000001b3;
  const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

  let mut hash = FNV_OFFSET_BASIS;
  crate::instrumentation::record_hash_operation();

  fn hash_file(hash: &mut u64, path: &Path, fnv_prime: u64) {
    let Ok(mut file) = fs::File::open(path) else {
      // If the file doesn't exist or can't be opened, still mix in a marker so the hash
      // is very unlikely to match a previous state where it was readable.
      // This avoids "accidental cache hit" if a file becomes unreadable.
      *hash ^= 0xff;
      *hash = hash.wrapping_mul(fnv_prime);
      crate::instrumentation::record_hash_input_bytes(1);
      return;
    };

    let mut buffer = [0; 8192];
    while let Ok(n) = file.read(&mut buffer) {
      if n == 0 {
        break;
      }
      crate::instrumentation::record_hash_input_bytes(n);
      crate::instrumentation::record_hashed_file_bytes_read(n);
      for byte in &buffer[..n] {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(fnv_prime);
      }
    }
  }

  // Hash workspace root Cargo.toml and Cargo.lock (if present)
  hash_file(&mut hash, &workspace_root.join("Cargo.toml"), FNV_PRIME);
  hash_file(&mut hash, &workspace_root.join("Cargo.lock"), FNV_PRIME);

  // Hash all workspace member manifests (sorted for determinism)
  let mut member_manifests: Vec<PathBuf> = metadata
    .workspace_packages()
    .iter()
    .map(|p| p.manifest_path.as_std_path().to_path_buf())
    .collect();

  member_manifests.sort_unstable_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

  for manifest_path in member_manifests {
    hash_file(&mut hash, &manifest_path, FNV_PRIME);
  }

  hash
}

// Git State (merged from git_state.rs)

/// Git state for the workspace
///
/// Provides git operations scoped to the workspace repository.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct GitState {
  /// Underlying git backend
  git: SystemGit,

  /// Cached repository root (git working tree)
  repo_root: PathBuf,
}

impl GitState {
  /// Open git repository at the given path
  fn open(path: &Path) -> RailResult<Self> {
    let git = SystemGit::open(path)?;
    let repo_root = git.worktree_root.clone();

    Ok(Self { git, repo_root })
  }

  /// Get repository root path
  pub fn repo_root(&self) -> &Path {
    &self.repo_root
  }

  /// Access underlying SystemGit for advanced operations
  ///
  /// Use this when you need direct access to SystemGit methods.
  pub fn git(&self) -> &SystemGit {
    &self.git
  }

  /// Get current branch name
  ///
  /// Returns "HEAD" if in detached HEAD state.
  pub fn current_branch(&self) -> RailResult<String> {
    self.git.current_branch()
  }

  /// Check if HEAD is detached (not on any branch)
  pub fn is_detached_head(&self) -> RailResult<bool> {
    self.git.is_detached_head()
  }

  /// Get the default branch name (main/master) via remote HEAD
  ///
  /// Returns `None` if no default branch can be determined.
  pub fn default_branch(&self) -> RailResult<Option<String>> {
    self.git.default_branch()
  }
}

// Workspace Context

/// Unified workspace context containing all shared workspace-level data.
///
/// Built once at startup, passed by reference to all commands and operations.
/// This eliminates redundant loads and provides a single source of truth for
/// workspace state.
///
/// Uses Arc for efficient sharing across threads and parallel operations.
/// Cloning is extremely cheap (just increments Arc refcounts).
pub struct WorkspaceContext {
  /// Cargo workspace root (`Cargo.toml` location).
  ///
  /// When Git is present, this root is validated as equal to or below the Git
  /// worktree. Access the repository root through [`WorkspaceContext::git`].
  pub workspace_root: PathBuf,

  /// Git state and operations, when the workspace is inside a Git repository.
  ///
  /// Commands that require Git must call [`WorkspaceContext::git`] so Cargo-only
  /// commands can still run in sandboxed workspaces.
  git: Option<Arc<GitState>>,

  /// Cargo metadata and workspace info (Arc for cheap cloning across threads)
  cargo: Arc<CargoState>,

  /// Validated repository-relative path to the Cargo workspace, when nested.
  workspace_prefix: Option<PathBuf>,

  /// Immutable source captured before Cargo metadata can create generated state.
  source_capture: Option<Arc<GitWorktreeCapture>>,

  /// Dependency graph (built from cargo metadata)
  /// Wrapped in Arc for efficient sharing across threads/commands
  graph: Arc<WorkspaceGraph>,

  /// One exact lazy derived-view owner shared with the snapshot when present.
  derived_views: Arc<DerivedViews>,

  /// Complete authoritative snapshot when requested during context construction.
  snapshot: Option<WorkspaceSnapshot>,

  /// Rail configuration (rail.toml)
  /// Optional because not all commands require configuration
  /// Wrapped in Arc for efficient sharing
  config: Option<Arc<RailConfig>>,
}

impl WorkspaceContext {
  /// Build workspace context from a root directory.
  ///
  /// Loads optional git state, cargo metadata, builds graph, and attempts to load rail.toml config.
  /// Config is optional - commands that require it should check and error.
  ///
  /// # Errors
  ///
  /// Returns [`RailError::Message`] if `cargo metadata` fails (e.g., invalid `Cargo.toml`).
  ///
  /// Returns [`RailError::Message`] if Cargo discovers a workspace outside the
  /// Git worktree opened from `workspace_root`.
  ///
  /// Returns [`RailError::Config`] if `rail.toml` exists but fails to parse.
  ///
  /// # Performance
  ///
  /// - Git state: <5ms (single git rev-parse)
  /// - Cargo metadata: 50-200ms for large workspaces
  /// - Graph build: 10-50ms
  /// - Config load: <5ms (or None if not found)
  /// - **Total: ~100-300ms** (vs 100-300ms × N commands without context)
  pub fn build(workspace_root: &Path) -> RailResult<Self> {
    Self::build_inner(workspace_root, ContextCapture::None)
  }

  /// Build a context and optionally capture WORKTREE source before metadata loading.
  #[doc(hidden)]
  pub fn build_with_source_capture(workspace_root: &Path, capture_source: bool) -> RailResult<Self> {
    let capture = if capture_source {
      ContextCapture::Worktree
    } else {
      ContextCapture::None
    };
    Self::build_inner(workspace_root, capture)
  }

  /// Build a context with one complete immutable workspace snapshot.
  ///
  /// Existing commands do not use this constructor until their ordered snapshot
  /// migration. It performs exact source, Cargo configuration, Cargo/rustc/rustdoc
  /// and compiler-wrapper identity, target configuration, manifest, lockfile,
  /// and base-resolution capture without loading target metadata views.
  ///
  /// # Errors
  ///
  /// Returns an error when any authoritative input is unreadable, malformed,
  /// outside the captured source boundary, contains a credential-bearing Cargo
  /// configuration URL, or changes during capture. Cargo, rustc, rustdoc, and
  /// configured compiler-wrapper identity commands must also succeed.
  pub fn build_with_snapshot(workspace_root: &Path) -> RailResult<Self> {
    Self::build_inner(workspace_root, ContextCapture::Snapshot)
  }

  fn build_inner(workspace_root: &Path, capture: ContextCapture) -> RailResult<Self> {
    let cargo_current_dir = std::env::current_dir()
      .map_err(|error| RailError::message(format!("failed to determine Cargo metadata current directory: {error}")))?;

    // Load git state when available. Cargo-only commands such as `unify --check`
    // must work in source sandboxes that intentionally omit `.git`.
    let git = match GitState::open(workspace_root) {
      Ok(git) => Some(Arc::new(git)),
      Err(RailError::Git(GitError::RepoNotFound { .. })) => None,
      Err(err) => return Err(err),
    };

    let initial_generated_roots = [super::cargo_rail_state_root(workspace_root)];
    let mut source_capture = if capture != ContextCapture::None {
      git
        .as_ref()
        .map(|state| GitWorktreeCapture::capture_excluding(state.git(), &initial_generated_roots))
        .transpose()?
    } else {
      None
    };

    let resolution_inputs = if capture == ContextCapture::Snapshot {
      Some(ResolutionViews::capture_inputs(&cargo_current_dir)?)
    } else {
      None
    };

    // Load cargo state
    let cargo = Arc::new(if let Some(inputs) = &resolution_inputs {
      CargoState::load_fresh(
        workspace_root,
        &cargo_current_dir,
        inputs.toolchain.cargo_program(),
        inputs.cargo_config.has_credential_capability(),
      )?
    } else {
      CargoState::load(workspace_root)?
    });
    let workspace_root = cargo.workspace_root().to_path_buf();

    // Repository paths are the authority for Git-backed capture and mutation.
    // A Cargo workspace may be nested inside that boundary, but never contain it
    // or escape it. Capture this relation once so every path consumer agrees.
    let workspace_prefix = git
      .as_ref()
      .map(|git| {
        crate::utils::path_relative_to(git.repo_root(), &workspace_root).map_err(|error| {
          RailError::with_help(
            format!(
              "Cargo workspace root '{}' is outside Git worktree '{}': {}",
              workspace_root.display(),
              git.repo_root().display(),
              error
            ),
            "select a Cargo workspace contained by this repository, or run outside the nested Git worktree",
          )
        })
      })
      .transpose()?
      .filter(|prefix| !prefix.as_os_str().is_empty());

    let generated_lockfile = if let (Some(capture), Some(git)) = (source_capture.as_mut(), git.as_ref()) {
      let generated_roots = validated_generated_source_roots(git.repo_root(), &workspace_root, &cargo)?;
      capture.exclude_generated_roots(git.git(), &generated_roots)?;
      stabilize_cargo_generated_lockfile(capture, git, &workspace_root, &generated_roots)?
    } else {
      None
    };

    // Build dependency graph from already-loaded metadata (avoids 50-200ms reload)
    let graph = Arc::new(WorkspaceGraph::from_metadata(cargo.metadata())?);

    let resolution_views = Arc::new(if let Some(inputs) = resolution_inputs.clone() {
      ResolutionViews::new_with_inputs(
        workspace_root.clone(),
        cargo_current_dir.clone(),
        cargo.shared_metadata(),
        Arc::clone(&graph),
        inputs,
      )
    } else {
      ResolutionViews::new(
        workspace_root.clone(),
        cargo_current_dir,
        cargo.shared_metadata(),
        Arc::clone(&graph),
      )
    });

    // Discover once and retain the exact parsed bytes for snapshot coherence.
    let config_path = RailConfig::find_config_path(&workspace_root);
    let (config, rail_config) = match config_path {
      Some(path) => {
        let (parsed, bytes) = RailConfig::load_path_with_bytes(&path)?;
        let parsed = Arc::new(parsed);
        (
          Some(Arc::clone(&parsed)),
          Some(CapturedRailConfig::new(path, bytes, parsed)),
        )
      }
      None => (None, None),
    };

    // Validate configured targets against rustc's canonical target list
    if capture != ContextCapture::Snapshot
      && let Some(ref cfg) = config
      && !cfg.targets.is_empty()
    {
      crate::targets::validate_targets(&cfg.targets)?;
    }

    // Validate config settings that require workspace context
    if let Some(ref cfg) = config {
      // Validate change-detection glob patterns
      cfg.change_detection.validate().map_err(RailError::Config)?;

      // Validate unify config (e.g., the transitive pinning host path).
      cfg.unify.validate(&workspace_root).map_err(RailError::Config)?;

      // Validate run profile schema.
      cfg.run.validate().map_err(RailError::Config)?;
    }

    // Store targets for lazy multi-target metadata loading
    // The metadata is only loaded when unify actually needs it (saves 150-600ms for other commands)
    let targets = config.as_ref().map(|c| c.targets.clone()).unwrap_or_default();
    let derived_views = Arc::new(DerivedViews::new(
      workspace_root.clone(),
      targets.clone(),
      Arc::clone(&resolution_views),
    ));

    let snapshot = if let Some(inputs) = resolution_inputs {
      let generated_lock_marker = generated_lockfile
        .as_ref()
        .map(|lockfile| (lockfile.path().to_path_buf(), ContentDigest::sha256(lockfile.bytes())));
      let source = match (&source_capture, &git) {
        (Some(capture), Some(_)) => capture.shared_snapshot(),
        (None, None) => Arc::new(SourceSnapshot::capture_filesystem(
          &workspace_root,
          &generated_source_roots(&workspace_root, &cargo),
        )?),
        _ => {
          return Err(RailError::message(
            "workspace snapshot source backend is inconsistent with repository state",
          ));
        }
      };
      let source_root = git.as_ref().map_or(workspace_root.as_path(), |git| git.repo_root());
      let target_identities = capture_target_identities(derived_views.cargo_current_dir(), &targets, &inputs)?;
      let snapshot = WorkspaceSnapshot::capture(
        source_root,
        source,
        Arc::clone(&cargo),
        rail_config,
        generated_lockfile,
        inputs.clone(),
        target_identities,
        Arc::clone(&derived_views),
      )?;
      if let (Some(capture), Some(git)) = (&source_capture, &git) {
        capture.validate_unchanged(git.git())?;
      }
      snapshot.validate_authoritative_files_unchanged()?;
      let current_inputs = ResolutionViews::capture_inputs(resolution_views.cargo_current_dir())?;
      validate_resolution_inputs_unchanged(&inputs, &current_inputs)?;
      snapshot.validate_external_targets_unchanged()?;
      if let Some((path, digest)) = generated_lock_marker {
        write_generated_lock_marker(&workspace_root, &path, digest)?;
      }
      Some(snapshot)
    } else {
      None
    };

    Ok(Self {
      workspace_root,
      git,
      cargo,
      workspace_prefix,
      source_capture: source_capture.map(Arc::new),
      graph,
      derived_views,
      snapshot,
      config,
    })
  }

  /// Get config or error if not found.
  ///
  /// Use this in commands that require rail.toml configuration.
  pub fn require_config(&self) -> RailResult<&Arc<RailConfig>> {
    self.config().ok_or_else(|| {
      crate::error::RailError::message(format!(
        "No rail.toml found in: {}\nSearched: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml\nRun 'cargo rail init' to create one.",
        self.workspace_root.display()
      ))
    })
  }

  /// Return the parsed rail configuration from the authoritative snapshot when present.
  pub fn config(&self) -> Option<&Arc<RailConfig>> {
    self
      .snapshot
      .as_ref()
      .and_then(WorkspaceSnapshot::shared_config)
      .or(self.config.as_ref())
  }

  /// Return Cargo state paired with the authoritative snapshot metadata.
  pub fn cargo(&self) -> &CargoState {
    self
      .snapshot
      .as_ref()
      .map(WorkspaceSnapshot::cargo)
      .unwrap_or(&self.cargo)
  }

  /// Return the exact dependency graph owned by the authoritative base resolution.
  pub fn graph(&self) -> &WorkspaceGraph {
    self
      .snapshot
      .as_ref()
      .map(|snapshot| snapshot.base_resolution().graph())
      .unwrap_or(&self.graph)
  }

  /// Return Git state for commands that require repository history.
  pub fn git(&self) -> RailResult<&GitState> {
    self.git.as_deref().ok_or_else(|| {
      RailError::Git(GitError::RepoNotFound {
        path: self.workspace_root.clone(),
      })
    })
  }

  /// Return true when this workspace is inside a Git repository.
  pub fn has_git(&self) -> bool {
    self.git.is_some()
  }

  /// Return source captured before metadata loading, when requested by dispatch.
  ///
  /// Ignored paths, the resolved Cargo target/build directories, and
  /// workspace-owned cargo-rail state are absent from this capture.
  pub fn source_capture(&self) -> Option<&GitWorktreeCapture> {
    self.source_capture.as_deref()
  }

  pub(crate) fn capture_worktree_source(&self) -> RailResult<GitWorktreeCapture> {
    let git = self.git()?;
    let generated_roots = validated_generated_source_roots(git.repo_root(), &self.workspace_root, &self.cargo)?;
    GitWorktreeCapture::capture_excluding(git.git(), &generated_roots)
  }

  /// Capture this declared Cargo workspace directly from the filesystem.
  ///
  /// The exact Cargo target/build directories and workspace-owned cargo-rail
  /// state are excluded. Git ignore files are not interpreted, so this remains
  /// deterministic in source archives and sandboxes without repository state.
  ///
  /// # Errors
  ///
  /// Returns an error when this context has a Git backend; callers must use the
  /// Git-backed capture so repository metadata and ignore policy remain authoritative.
  ///
  /// Returns an error when the workspace cannot be walked consistently or a
  /// generated-state root would exclude the complete workspace boundary.
  pub fn capture_filesystem_source(&self) -> RailResult<SourceSnapshot> {
    if self.has_git() {
      return Err(RailError::with_help(
        "filesystem-backed source capture is only available without Git",
        "use the Git-backed worktree capture for repositories",
      ));
    }
    SourceSnapshot::capture_filesystem(
      &self.workspace_root,
      &generated_source_roots(&self.workspace_root, &self.cargo),
    )
  }

  /// Return current non-generated source paths that differ from `HEAD`.
  pub(crate) fn changed_source_paths(&self) -> RailResult<Vec<PathBuf>> {
    let git = self.git()?.git();
    Ok(
      self
        .capture_worktree_source()?
        .changes_from(git, "HEAD")?
        .entries()
        .iter()
        .map(|change| change.path.as_path().to_path_buf())
        .collect(),
    )
  }

  pub(crate) fn exclude_generated_source_paths(&self, paths: Vec<PathBuf>) -> RailResult<Vec<PathBuf>> {
    let git = self.git()?;
    let generated_roots = validated_generated_source_roots(git.repo_root(), &self.workspace_root, &self.cargo)?;
    let exclusions = SourceExclusions::from_absolute_roots(git.git(), &generated_roots)?;
    paths
      .into_iter()
      .filter_map(|path| match exclusions.contains_path(&path) {
        Ok(true) => None,
        Ok(false) => Some(Ok(path)),
        Err(error) => Some(Err(error)),
      })
      .collect()
  }

  /// Get multi-target metadata, loading lazily on first access.
  ///
  /// This is only used by `cargo rail unify`. Other commands don't need it,
  /// so lazy loading saves 150-600ms for commands like `plan`, `run`, `split`, etc.
  ///
  /// The metadata is cached after first load - subsequent calls return the cached value.
  pub fn multi_target_metadata(&self) -> RailResult<Arc<MultiTargetMetadata>> {
    if let Some(snapshot) = &self.snapshot {
      snapshot.multi_target_metadata()
    } else {
      self.derived_views.multi_target_metadata()
    }
  }

  /// Return the exact lazy Cargo resolution for one package/feature/target request.
  pub fn resolution_view(&self, request: ResolutionRequest) -> RailResult<Arc<ResolutionView>> {
    if let Some(snapshot) = &self.snapshot {
      snapshot.resolution_view(request)
    } else {
      self.derived_views.resolution_view(request)
    }
  }

  /// Return the authoritative snapshot captured by [`Self::build_with_snapshot`].
  ///
  /// Other constructors intentionally avoid this work until command consumers
  /// migrate in the ordered improvement program.
  ///
  /// # Errors
  ///
  /// Returns an error when this context was built by another constructor.
  pub fn snapshot(&self) -> RailResult<&WorkspaceSnapshot> {
    self.snapshot.as_ref().ok_or_else(|| {
      RailError::with_help(
        "workspace context was built without authoritative snapshot capture",
        "construct it with WorkspaceContext::build_with_snapshot",
      )
    })
  }

  /// Return the authoritative snapshot identity when this context owns one.
  #[doc(hidden)]
  pub fn snapshot_id(&self) -> Option<crate::workspace::SnapshotId> {
    self.snapshot.as_ref().map(WorkspaceSnapshot::id)
  }

  pub(crate) fn validate_snapshot_unchanged(&self) -> RailResult<()> {
    let snapshot = self.snapshot()?;
    if let (Some(capture), Some(git)) = (&self.source_capture, &self.git) {
      capture.validate_unchanged(git.git())?;
    }
    snapshot.validate_live_authoritative_inputs()
  }

  /// Load each target's exact rustc cfg set once for the command context.
  pub fn target_cfg_sets(&self) -> RailResult<Arc<std::collections::HashMap<String, TargetCfgSet>>> {
    if let Some(snapshot) = &self.snapshot {
      snapshot.target_cfg_sets()
    } else {
      self.derived_views.target_cfg_sets()
    }
  }

  /// Get workspace root as Path reference (convenience)
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Get the validated relative path from the Git root to the Cargo workspace.
  ///
  /// Returns `Some(prefix)` if workspace is nested inside git repo (e.g., "codex-rs"),
  /// or `None` if they're the same directory.
  ///
  /// This is used to strip the prefix from git-relative paths so they can be
  /// matched against workspace-relative paths (e.g., for crate membership).
  pub fn workspace_prefix(&self) -> Option<PathBuf> {
    self.workspace_prefix.clone()
  }

  /// Convert a git-relative path to a workspace-relative path.
  ///
  /// If the workspace is nested inside the git repo (e.g., git at `/repo`, workspace at `/repo/rust`),
  /// git returns paths like `rust/src/lib.rs` but the workspace expects `src/lib.rs`.
  ///
  /// Returns `None` if the path doesn't belong to this workspace.
  pub fn to_workspace_path(&self, git_path: &Path) -> Option<PathBuf> {
    if let Some(prefix) = &self.workspace_prefix {
      // Git always uses forward slashes, but PathBuf::from converts to platform separators.
      // On Windows, this causes strip_prefix to fail when git_path has / but prefix has \.
      // Normalize git_path by rebuilding through components to use platform separators.
      let normalized = git_path.components().collect::<PathBuf>();
      normalized.strip_prefix(prefix).ok().map(|p| p.to_path_buf())
    } else {
      Some(git_path.to_path_buf())
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextCapture {
  None,
  Worktree,
  Snapshot,
}

fn validate_resolution_inputs_unchanged(initial: &ResolutionInputs, current: &ResolutionInputs) -> RailResult<()> {
  if current.cargo_config != initial.cargo_config {
    return Err(RailError::with_help(
      "Cargo configuration changed while constructing the workspace snapshot",
      "retry after Cargo configuration and environment changes have stopped",
    ));
  }
  if current.toolchain != initial.toolchain {
    return Err(RailError::with_help(
      "Cargo, rustc, rustdoc, or compiler-wrapper identity changed while constructing the workspace snapshot",
      "retry after toolchain updates and overrides have stopped",
    ));
  }
  Ok(())
}

fn validated_generated_source_roots(
  git_root: &Path,
  workspace_root: &Path,
  cargo: &CargoState,
) -> RailResult<Vec<PathBuf>> {
  validate_cargo_output_root(
    "target",
    git_root,
    workspace_root,
    cargo.metadata().target_directory.as_std_path(),
  )?;
  if let Some(build_directory) = &cargo.metadata().build_directory {
    validate_cargo_output_root("build", git_root, workspace_root, build_directory.as_std_path())?;
  }
  Ok(generated_source_roots(workspace_root, cargo))
}

fn generated_source_roots(workspace_root: &Path, cargo: &CargoState) -> Vec<PathBuf> {
  let mut roots = vec![
    super::cargo_rail_state_root(workspace_root),
    cargo.metadata().target_directory.as_std_path().to_path_buf(),
  ];
  if let Some(build_directory) = &cargo.metadata().build_directory {
    roots.push(build_directory.as_std_path().to_path_buf());
  }
  roots
}

fn stabilize_cargo_generated_lockfile(
  initial: &mut GitWorktreeCapture,
  git: &GitState,
  workspace_root: &Path,
  generated_roots: &[PathBuf],
) -> RailResult<Option<CapturedLockfile>> {
  let lockfile = workspace_root.join("Cargo.lock");
  let lockfile_metadata = match fs::symlink_metadata(&lockfile) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  if !lockfile_metadata.file_type().is_file() {
    return Ok(None);
  }
  let relative = crate::utils::path_relative_to(git.repo_root(), &lockfile)?;
  let relative = RepositoryPath::new(&relative)?;
  let initially_captured = initial
    .snapshot()
    .tree()
    .entries()
    .binary_search_by(|entry| entry.path.cmp(&relative))
    .is_ok();
  if initially_captured {
    if initial.is_untracked(&relative) {
      let bytes = fs::read(&lockfile)?;
      let digest = ContentDigest::sha256(&bytes);
      if generated_lock_marker_matches(workspace_root, digest)? {
        initial.exclude_generated_roots(git.git(), std::slice::from_ref(&lockfile))?;
        initial.validate_unchanged(git.git())?;
        return Ok(Some(CapturedLockfile::new(lockfile, bytes)));
      }
    }
    return Ok(None);
  }

  let repeated = GitWorktreeCapture::capture_excluding(git.git(), generated_roots)?;
  let repeated_digest = repeated
    .snapshot()
    .tree()
    .entries()
    .binary_search_by(|entry| entry.path.cmp(&relative))
    .ok()
    .and_then(|index| match repeated.snapshot().tree().entries()[index].kind {
      SourceEntryKind::RegularFile { digest, .. } => Some(digest),
      _ => None,
    });
  let Some(repeated_digest) = repeated_digest else {
    return Ok(None);
  };
  let bytes = fs::read(&lockfile)?;
  if repeated_digest != ContentDigest::sha256(&bytes) {
    return Err(RailError::with_help(
      "Cargo-generated lockfile changed after source capture",
      "retry after workspace input changes have stopped",
    ));
  }
  initial.exclude_generated_roots(git.git(), std::slice::from_ref(&lockfile))?;
  initial.validate_unchanged(git.git()).map_err(|_| {
    RailError::with_help(
      "workspace source changed while Cargo generated a missing lockfile",
      "retry after source changes have stopped; Cargo.lock may be retained as the resolved workspace lock",
    )
  })?;
  Ok(Some(CapturedLockfile::new(lockfile, bytes)))
}

fn generated_lock_marker_path(workspace_root: &Path) -> PathBuf {
  super::cargo_rail_state_root(workspace_root).join("generated-lockfile-v1")
}

fn generated_lock_marker_value(workspace_root: &Path, digest: ContentDigest) -> RailResult<Vec<u8>> {
  let workspace = workspace_root
    .to_str()
    .ok_or_else(|| RailError::message("workspace root is not valid UTF-8 for generated lockfile provenance"))?;
  let mut marker = Vec::from(&b"cargo-rail-generated-lockfile-v1\0"[..]);
  marker.extend_from_slice(&(workspace.len() as u64).to_le_bytes());
  marker.extend_from_slice(workspace.as_bytes());
  marker.extend_from_slice(digest.as_bytes());
  Ok(marker)
}

fn generated_lock_marker_matches(workspace_root: &Path, digest: ContentDigest) -> RailResult<bool> {
  let marker = generated_lock_marker_path(workspace_root);
  let metadata = match fs::symlink_metadata(&marker) {
    Ok(metadata) if metadata.file_type().is_file() => metadata,
    Ok(_) => return Ok(false),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error.into()),
  };
  let expected = generated_lock_marker_value(workspace_root, digest)?;
  let expected_len = u64::try_from(expected.len())
    .map_err(|_| RailError::message("generated lockfile marker length exceeds the supported range"))?;
  if metadata.len() != expected_len {
    return Ok(false);
  }
  let mut actual = Vec::with_capacity(expected.len());
  fs::File::open(marker)?
    .take(expected_len.saturating_add(1))
    .read_to_end(&mut actual)?;
  Ok(actual == expected)
}

fn write_generated_lock_marker(workspace_root: &Path, lockfile: &Path, digest: ContentDigest) -> RailResult<()> {
  if lockfile != workspace_root.join("Cargo.lock") {
    return Err(RailError::message(format!(
      "refusing to mark unexpected generated lockfile '{}'",
      lockfile.display()
    )));
  }
  let marker = generated_lock_marker_path(workspace_root);
  if fs::symlink_metadata(&marker).is_ok_and(|metadata| !metadata.file_type().is_file()) {
    return Err(RailError::message(format!(
      "generated lockfile marker '{}' is not a regular file",
      marker.display()
    )));
  }
  let parent = marker
    .parent()
    .ok_or_else(|| RailError::message("generated lockfile marker has no parent directory"))?;
  fs::create_dir_all(parent)?;
  fs::write(&marker, generated_lock_marker_value(workspace_root, digest)?).map_err(|error| {
    RailError::message(format!(
      "failed to write generated lockfile marker '{}': {error}",
      marker.display()
    ))
  })
}

fn validate_cargo_output_root(
  kind: &str,
  git_root: &Path,
  workspace_root: &Path,
  output_root: &Path,
) -> RailResult<()> {
  let git_root = crate::utils::canonicalize_existing(git_root)?;
  let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
  let output_root = crate::utils::canonicalize_allow_missing(output_root)?;
  if output_root.starts_with(&git_root) && workspace_root.starts_with(&output_root) {
    return Err(RailError::with_help(
      format!(
        "Cargo {} directory '{}' contains Cargo workspace root '{}'",
        kind,
        output_root.display(),
        workspace_root.display()
      ),
      "configure Cargo build output in a dedicated directory below or outside the repository",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_workspace_context_build() {
    // Use current directory as test workspace
    let current_dir = std::env::current_dir().unwrap();

    // Should successfully build workspace context
    let ctx = WorkspaceContext::build(&current_dir);
    assert!(ctx.is_ok(), "Should successfully build workspace context");

    let ctx = ctx.unwrap();

    // Verify all components are initialized
    assert!(ctx.git().unwrap().repo_root().exists(), "Repo root should exist");
    assert!(ctx.workspace_root.exists(), "Workspace root should exist");

    // Git state should be initialized (git root typically == workspace root)
    // We allow them to differ but in most cases they're the same
    let _ = ctx.git().unwrap().repo_root();

    // Cargo state should be initialized
    assert_eq!(
      ctx.cargo().workspace_root(),
      &ctx.workspace_root,
      "Cargo workspace root should match"
    );

    // Should find cargo-rail in workspace
    let packages = ctx.cargo().workspace_members();
    assert!(!packages.is_empty(), "Should have workspace packages");
    assert!(
      packages.iter().any(|p| p.name == "cargo-rail"),
      "Should find cargo-rail package"
    );

    // Graph should be initialized
    let members = ctx.graph().workspace_members();
    assert!(!members.is_empty(), "Graph should have workspace members");
    assert!(
      members.contains(&"cargo-rail".to_string()),
      "Graph should contain cargo-rail"
    );

    // Config may or may not be loaded depending on workspace
    // Just verify it's an Option
    let _ = ctx.config();
  }

  #[test]
  fn test_git_state_wrapper() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Should be able to access git operations via git() accessor
    let head = ctx.git().unwrap().git().head_commit();
    assert!(head.is_ok(), "Should get HEAD commit");

    let head_sha = head.unwrap();
    assert_eq!(head_sha.len(), 40, "HEAD SHA should be 40 characters");

    // Should be able to get current branch
    let branch = ctx.git().unwrap().git().current_branch();
    assert!(branch.is_ok(), "Should get current branch");
  }

  #[test]
  fn test_cargo_state_wrapper() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Should be able to access cargo metadata
    let metadata = ctx.cargo().metadata();
    let packages = metadata.workspace_packages();
    assert!(!packages.is_empty(), "Should have packages");

    // Should be able to get specific package
    let cargo_rail = ctx.cargo().get_package("cargo-rail");
    assert!(cargo_rail.is_some(), "Should find cargo-rail package");

    let pkg = cargo_rail.unwrap();
    assert_eq!(pkg.name.as_str(), "cargo-rail");
  }

  #[test]
  fn test_graph_integration() {
    let current_dir = std::env::current_dir().unwrap();
    let ctx = WorkspaceContext::build(&current_dir).unwrap();

    // Graph should be consistent with cargo metadata
    let graph_members = ctx.graph().workspace_members();
    let cargo_packages: Vec<_> = ctx
      .cargo()
      .workspace_members()
      .iter()
      .map(|p| p.name.as_str())
      .collect();

    for member in graph_members {
      assert!(
        cargo_packages.contains(&member.as_str()),
        "Graph member {} should be in cargo packages",
        member
      );
    }
  }
}
