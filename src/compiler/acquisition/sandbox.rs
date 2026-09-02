//! Reusable private Cargo sandbox generations.

use crate::error::{RailError, RailResult};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const CARGO_CACHE_DIRECTORY_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n\
# This file is a cache directory tag created by cargo.\n\
# For information about cache directory tags see https://bford.info/cachedir/\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxCompatibility {
    compiler: Box<str>,
    target: Box<str>,
    profile: Box<str>,
    environment: Box<str>,
    wrapper: Box<str>,
}

impl SandboxCompatibility {
    pub(crate) fn new(
        compiler: impl Into<Box<str>>,
        target: impl Into<Box<str>>,
        profile: impl Into<Box<str>>,
        environment: impl Into<Box<str>>,
        wrapper: impl Into<Box<str>>,
    ) -> Self {
        Self {
            compiler: compiler.into(),
            target: target.into(),
            profile: profile.into(),
            environment: environment.into(),
            wrapper: wrapper.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxState {
    Available,
    Poisoned,
}

#[derive(Debug)]
struct SandboxGeneration {
    #[cfg(test)]
    number: u64,
    root: PathBuf,
    target: PathBuf,
    build: PathBuf,
    compatibility: SandboxCompatibility,
    state: SandboxState,
}

/// One command's bounded private Cargo state.
pub(crate) struct SandboxPool {
    root: PathBuf,
    namespace: Box<str>,
    next_generation: u64,
    slots: Vec<SandboxSlot>,
    closed: bool,
    _workspace_lock: crate::cache::WorkspaceCacheLock,
}

#[derive(Debug)]
enum SandboxSlot {
    Vacant,
    Leased,
    Resident(SandboxGeneration),
}

impl SandboxPool {
    pub(crate) fn prepare(workspace_root: &Path, capacity: usize) -> RailResult<Self> {
        if capacity == 0 {
            return Err(RailError::message("compiler acquisition sandbox pool capacity is zero"));
        }
        let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
        let namespace = generation_namespace()?;
        let workspace_lock = crate::cache::lock_workspace(&workspace_root)?;
        let state_root = crate::workspace::cargo_rail_state_root(&workspace_root);
        let canonical_state_root = crate::utils::canonicalize_existing(&state_root)?;
        if canonical_state_root != state_root || !canonical_state_root.starts_with(&workspace_root) {
            return Err(RailError::message(
                "compiler acquisition sandbox root escaped the workspace",
            ));
        }
        let root = state_root.join("compiler-artifacts-v1");
        remove_optional_owned_directory(&root)?;
        create_private_directory(&root)?;
        fs::write(root.join("CACHEDIR.TAG"), CARGO_CACHE_DIRECTORY_TAG)?;
        Ok(Self {
            root,
            namespace,
            next_generation: 0,
            slots: (0..capacity).map(|_| SandboxSlot::Vacant).collect(),
            closed: false,
            _workspace_lock: workspace_lock,
        })
    }

    pub(crate) fn lease(&mut self, compatibility: SandboxCompatibility) -> RailResult<SandboxLease> {
        let slot = self
            .slots
            .iter()
            .position(|slot| {
                matches!(slot, SandboxSlot::Resident(generation) if generation.state == SandboxState::Available && generation.compatibility == compatibility)
            })
            .or_else(|| self.slots.iter().position(|slot| matches!(slot, SandboxSlot::Vacant)))
            .or_else(|| self.slots.iter().position(|slot| matches!(slot, SandboxSlot::Resident(_))))
            .ok_or_else(|| RailError::message("compiler acquisition sandbox pool has no available generation"))?;
        let previous = std::mem::replace(&mut self.slots[slot], SandboxSlot::Leased);
        let generation = match previous {
            SandboxSlot::Resident(generation)
                if generation.state == SandboxState::Available && generation.compatibility == compatibility =>
            {
                crate::instrumentation::record_compiler_acquisition_sandbox_reuse();
                generation
            }
            SandboxSlot::Resident(generation) => {
                remove_owned_directory(&generation.root)?;
                crate::instrumentation::record_compiler_acquisition_sandbox_delete();
                self.create_generation(slot, compatibility)?
            }
            SandboxSlot::Vacant => self.create_generation(slot, compatibility)?,
            SandboxSlot::Leased => {
                return Err(RailError::message(
                    "compiler acquisition selected an already leased sandbox generation",
                ));
            }
        };
        Ok(SandboxLease {
            slot,
            generation: Some(generation),
        })
    }

    fn create_generation(&mut self, slot: usize, compatibility: SandboxCompatibility) -> RailResult<SandboxGeneration> {
        let number = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| RailError::message("compiler acquisition sandbox generation overflowed"))?;
        let root = self.root.join(format!("generation-{}-{slot}-{number}", self.namespace));
        create_private_directory(&root)?;
        let target = root.join("target");
        let build = root.join("build");
        create_private_directory(&target)?;
        create_private_directory(&build)?;
        fs::write(target.join("CACHEDIR.TAG"), CARGO_CACHE_DIRECTORY_TAG)?;
        let generation = SandboxGeneration {
            #[cfg(test)]
            number,
            root,
            target,
            build,
            compatibility,
            state: SandboxState::Available,
        };
        crate::instrumentation::record_compiler_acquisition_sandbox_create();
        Ok(generation)
    }

    pub(crate) fn reclaim(&mut self, mut returned: ReturnedSandbox) -> RailResult<()> {
        let slot = self
            .slots
            .get_mut(returned.slot)
            .ok_or_else(|| RailError::message("compiler acquisition returned an unknown sandbox slot"))?;
        if !matches!(slot, SandboxSlot::Leased) {
            return Err(RailError::message(
                "compiler acquisition returned a sandbox that was not leased",
            ));
        }
        let generation = returned
            .generation
            .take()
            .ok_or_else(|| RailError::message("compiler acquisition returned an empty sandbox generation"))?;
        *slot = SandboxSlot::Resident(generation);
        Ok(())
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn close(mut self) -> RailResult<()> {
        self.remove_all()?;
        self.closed = true;
        Ok(())
    }

    fn remove_all(&mut self) -> RailResult<()> {
        for slot in &mut self.slots {
            if let SandboxSlot::Resident(generation) = std::mem::replace(slot, SandboxSlot::Vacant) {
                remove_owned_directory(&generation.root)?;
                crate::instrumentation::record_compiler_acquisition_sandbox_delete();
            }
        }
        remove_optional_owned_directory(&self.root)
    }
}

fn generation_namespace() -> RailResult<Box<str>> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|error| {
        RailError::message(format!(
            "failed to generate a compiler acquisition sandbox namespace: {error}"
        ))
    })?;
    let mut namespace = String::with_capacity(entropy.len() * 2);
    for byte in entropy {
        write!(&mut namespace, "{byte:02x}")
            .map_err(|_| RailError::message("compiler acquisition sandbox namespace formatting failed"))?;
    }
    Ok(namespace.into_boxed_str())
}

impl Drop for SandboxPool {
    fn drop(&mut self) {
        if !self.closed {
            drop(self.remove_all());
        }
    }
}

/// Exclusive, worker-owned access to one compatible sandbox generation.
pub(crate) struct SandboxLease {
    slot: usize,
    generation: Option<SandboxGeneration>,
}

impl SandboxLease {
    pub(crate) fn target_dir(&self) -> &Path {
        &self.generation.as_ref().expect("live sandbox lease").target
    }

    pub(crate) fn build_dir(&self) -> &Path {
        &self.generation.as_ref().expect("live sandbox lease").build
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation.as_ref().expect("live sandbox lease").number
    }

    pub(crate) fn finish(mut self) -> ReturnedSandbox {
        self.generation.as_mut().expect("live sandbox lease").state = SandboxState::Available;
        self.returned()
    }

    pub(crate) fn poison(mut self) -> ReturnedSandbox {
        self.generation.as_mut().expect("live sandbox lease").state = SandboxState::Poisoned;
        crate::instrumentation::record_compiler_acquisition_sandbox_poison();
        self.returned()
    }

    fn returned(&mut self) -> ReturnedSandbox {
        ReturnedSandbox {
            slot: self.slot,
            generation: self.generation.take(),
        }
    }
}

impl Drop for SandboxLease {
    fn drop(&mut self) {
        if let Some(generation) = self.generation.take() {
            crate::instrumentation::record_compiler_acquisition_sandbox_poison();
            if remove_owned_directory(&generation.root).is_ok() {
                crate::instrumentation::record_compiler_acquisition_sandbox_delete();
            }
        }
    }
}

/// A resolved generation returned to the coordinator for deterministic reuse.
pub(crate) struct ReturnedSandbox {
    slot: usize,
    generation: Option<SandboxGeneration>,
}

impl Drop for ReturnedSandbox {
    fn drop(&mut self) {
        if let Some(generation) = self.generation.take()
            && remove_owned_directory(&generation.root).is_ok()
        {
            crate::instrumentation::record_compiler_acquisition_sandbox_delete();
        }
    }
}

fn create_private_directory(path: &Path) -> RailResult<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "compiler acquisition sandbox '{}' is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn remove_optional_owned_directory(path: &Path) -> RailResult<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => remove_owned_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_owned_directory(path: &Path) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "refusing to recursively remove non-directory compiler acquisition sandbox '{}'",
            path.display()
        )));
    }
    fs::remove_dir_all(path).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{SandboxCompatibility, SandboxPool};
    use std::fs;

    fn compatibility(target: &str) -> SandboxCompatibility {
        SandboxCompatibility::new("compiler", target, "check", "environment", "diagnostic")
    }

    #[test]
    fn unresolved_lease_poison_recreates_the_whole_generation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut pool = SandboxPool::prepare(workspace.path(), 1).expect("sandbox pool");
        let first_target = {
            let lease = pool.lease(compatibility("default")).expect("first lease");
            fs::write(lease.target_dir().join("partial"), b"partial").expect("partial artifact");
            let target = lease.target_dir().to_path_buf();
            let returned = lease.poison();
            pool.reclaim(returned).expect("return poisoned sandbox");
            target
        };
        let second = pool.lease(compatibility("default")).expect("recreated lease");
        assert_eq!(second.generation(), 1);
        assert!(!first_target.exists(), "poisoned generation survived reuse");
        assert!(!second.target_dir().join("partial").exists());
        let returned = second.finish();
        pool.reclaim(returned).expect("return recreated sandbox");
    }

    #[test]
    fn compatible_lease_reuses_and_incompatible_lease_recreates() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut pool = SandboxPool::prepare(workspace.path(), 1).expect("sandbox pool");
        let first_target = {
            let first = pool.lease(compatibility("default")).expect("first lease");
            let target = first.target_dir().to_path_buf();
            let returned = first.finish();
            pool.reclaim(returned).expect("return first sandbox");
            target
        };
        let second = pool.lease(compatibility("default")).expect("compatible lease");
        assert_eq!(second.generation(), 0);
        assert_eq!(second.target_dir(), first_target);
        let returned = second.finish();
        pool.reclaim(returned).expect("return compatible sandbox");

        let third = pool.lease(compatibility("wasm32-wasip1")).expect("incompatible lease");
        assert_eq!(third.generation(), 1);
        assert_ne!(third.target_dir(), first_target);
        assert!(!first_target.exists());
        let returned = third.finish();
        pool.reclaim(returned).expect("return incompatible sandbox");
    }

    #[test]
    fn bounded_pool_never_leases_one_generation_twice() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut pool = SandboxPool::prepare(workspace.path(), 2).expect("sandbox pool");
        let first = pool.lease(compatibility("default")).expect("first lease");
        let second = pool.lease(compatibility("default")).expect("second lease");
        assert_ne!(first.target_dir(), second.target_dir());
        assert!(pool.lease(compatibility("default")).is_err());
        let first = first.finish();
        let second = second.finish();
        pool.reclaim(first).expect("return first lease");
        pool.reclaim(second).expect("return second lease");
    }

    #[test]
    fn pool_reclaims_interrupted_state_and_serializes_workspace_owners() {
        let workspace = tempfile::tempdir().expect("workspace");
        let stale = workspace
            .path()
            .join("target/cargo-rail/compiler-artifacts-v1/interrupted/debug/deps");
        fs::create_dir_all(&stale).expect("stale artifact tree");
        fs::write(stale.join("artifact"), b"reconstructible").expect("stale artifact");

        let pool = SandboxPool::prepare(workspace.path(), 1).expect("sandbox pool");
        assert!(!stale.exists(), "interrupted artifact graph survived restart cleanup");
        let workspace_root = workspace.path().to_path_buf();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let next = SandboxPool::prepare(&workspace_root, 1).expect("next pool");
                drop(next);
                finished_tx.send(()).expect("completion");
            });
            assert!(
                finished_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
                "a second compiler acquisition crossed the workspace sandbox authority"
            );
            let root = pool.root().to_path_buf();
            drop(pool);
            assert!(!root.exists(), "sandbox pool left its command root");
            finished_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("next pool should proceed after cleanup");
        });
    }
}
