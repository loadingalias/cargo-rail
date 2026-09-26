//! Files, directories, and variables that loaded procedural macros read, as the compiler driver observed them.
//!
//! Rustc records what expanded code includes and the variables it reads through `env!` and tracked
//! environment. A macro can also read files, list directories, and read variables that rustc never records.
//! The driver observes those reads on every cold compilation and reports effects that no key can bind.
//! This module decides which observed reads an action binds and which make the unit bypass reuse:
//!
//! - a variable read joins the compiler's own environment reads, so it binds and shares exactly as rustc's
//!   `env!` reads do;
//! - a path read inside the repository binds the path's current state: its contents, its absence, or its
//!   directory listing;
//! - a read inside a namespace the action already captures whole, such as the crate's own sources or its
//!   build-script output, is already bound;
//! - `cargo locate-project`, which `proc-macro-crate` runs to find the workspace, binds what Cargo reads to
//!   answer it;
//! - any other read or process, a read through a symbolic link, and every unobservable effect bypass by name.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compiler::native_input_protocol::{
    NativeMacroObservation, NativeMacroPathAccess, NativeMacroSpawn, NativeMacroUnobservable as Unobservable,
};
use crate::compiler::observation::{EnvironmentObservation, is_secret_name};
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

use super::cargo_inputs::CargoInputs;
use super::{NATIVE_CAPTURE_LIMITS, NativeCaptureBudget, capture_guarded_file, native_relative_path, semantic_mode};

/// At most this many paths bind one action; a macro that reads more bypasses.
pub(super) const MAX_MACRO_PATH_INPUTS: usize = 4096;
/// Devices whose reads depend on no state a key could bind. `Command` opens `/dev/null` for a child's
/// standard input, and randomness sources are not inputs.
const STATELESS_DEVICES: [&str; 4] = ["/dev/null", "/dev/random", "/dev/urandom", "/dev/zero"];
/// Entries retained for one directory listing.
const MAX_LISTING_ENTRIES: usize = 16_384;

/// One repository path a macro read, and whether it listed the directory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MacroPathSelector {
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) listing: bool,
    /// The path is the package directory of a `cargo locate-project` query, relative to this root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cargo_query: Option<CargoQueryRoot>,
}

/// Where the package directory of a Cargo query is rooted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CargoQueryRoot {
    Repository,
    /// An unpacked registry or Git package below Cargo's home.
    CargoHome,
}

impl MacroPathSelector {
    fn validate(&self) -> RailResult<()> {
        // The empty path names the workspace root itself.
        if self.cargo_query.is_some() && self.listing
            || self.path.len() > super::MAX_DYNAMIC_REPOSITORY_PATH_BYTES
            || !self.path.is_empty() && native_relative_path(Path::new(&self.path)).ok().as_deref() != Some(&self.path)
        {
            return Err(RailError::message(
                "procedural-macro input path is not a normalized repository path",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_selectors(selectors: &[MacroPathSelector]) -> RailResult<()> {
    if selectors.len() > MAX_MACRO_PATH_INPUTS || selectors.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(RailError::message(
            "procedural-macro inputs are not bounded, sorted, and unique",
        ));
    }
    selectors.iter().try_for_each(MacroPathSelector::validate)
}

/// The kind of one directory entry, as a listing binds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ListedEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// The state of one path when the action was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum MacroPathState {
    Absent,
    File {
        content_digest: String,
        bytes: u64,
        mode: u32,
    },
    Directory,
    /// Entry names and kinds, digested in name order.
    Listing {
        listing_digest: String,
        entries: u64,
    },
    /// Everything Cargo reads to answer the query, digested.
    CargoQuery {
        inputs_digest: String,
    },
}

/// One bound path and its captured state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MacroPathInput {
    path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    listing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cargo_query: Option<CargoQueryRoot>,
    state: MacroPathState,
}

impl MacroPathInput {
    pub(super) fn selector(&self) -> MacroPathSelector {
        MacroPathSelector {
            path: self.path.clone(),
            listing: self.listing,
            cargo_query: self.cargo_query,
        }
    }
}

/// The bypass reason for effects that no key can bind, if the macro produced any.
pub(super) fn bypass_reason(observation: &NativeMacroObservation) -> Option<&'static str> {
    if let Some(reason) = observation.unobservable.first() {
        return Some(match reason {
            Unobservable::ObservationUnavailable => "procedural_macro_observation_unavailable",
            Unobservable::ImportUnclassified => "procedural_macro_import_unclassified",
            Unobservable::DynamicDependency => "procedural_macro_dynamic_dependency",
            Unobservable::Initializer => "procedural_macro_initializer",
            Unobservable::RawSystemCall => "procedural_macro_raw_system_call",
            Unobservable::ProcessControl => "procedural_macro_process_control",
            Unobservable::Network => "procedural_macro_network",
            Unobservable::FileWrite => "procedural_macro_file_write",
            Unobservable::EnvironmentWrite => "procedural_macro_environment_write",
            Unobservable::EnvironmentEnumeration => "procedural_macro_environment_enumeration",
            Unobservable::DynamicLoad => "procedural_macro_dynamic_load",
            Unobservable::ExecutableMemory => "procedural_macro_executable_memory",
            Unobservable::WorkingDirectory => "procedural_macro_working_directory",
            Unobservable::HostState => "procedural_macro_host_state",
            Unobservable::PathUnavailable => "procedural_macro_path_unavailable",
            Unobservable::ObservationLimit => "procedural_macro_observation_limit",
        });
    }
    if observation.environment.iter().any(|name| is_secret_name(name)) {
        return Some("procedural_macro_secret_environment");
    }
    None
}

/// The variables a macro read, as compiler environment reads with their current values.
///
/// The compiler process inherits this process's environment, less private capabilities that the selector
/// rejects, so the current value is the value the macro read.
pub(super) fn environment_reads(observation: &NativeMacroObservation) -> impl Iterator<Item = EnvironmentObservation> {
    observation.environment.iter().map(|name| EnvironmentObservation {
        name: name.clone(),
        value_digest: std::env::var_os(name)
            .map(|value| format!("sha256:{}", ContentDigest::sha256(value.as_encoded_bytes()))),
        secret_capability: is_secret_name(name),
    })
}

/// Map observed path reads to repository selectors, or name why one cannot bind.
///
/// `source_root_spelling` is the spelling Cargo used for the workspace root, which resolves to
/// `workspace_root`. `captured` names canonical directories the action already binds whole.
pub(super) fn path_selectors(
    observation: &NativeMacroObservation,
    workspace_root: &Path,
    source_root_spelling: &Path,
    captured: &[&Path],
) -> Result<Vec<MacroPathSelector>, &'static str> {
    const OUTSIDE: &str = "procedural_macro_read_outside_repository";
    const SYMLINK: &str = "procedural_macro_read_through_symlink";
    let canonical_root = crate::utils::canonicalize_existing(workspace_root).map_err(|_| OUTSIDE)?;
    let mut selectors = BTreeSet::new();
    for spawn in &observation.spawns {
        selectors.insert(cargo_query_selector(spawn, &canonical_root).ok_or("procedural_macro_process_unmodeled")?);
    }
    for read in &observation.paths {
        let spelled = Path::new(&read.path);
        if STATELESS_DEVICES.contains(&read.path.as_str()) {
            continue;
        }
        if resolved_location(spelled).is_some_and(|resolved| captured.iter().any(|root| resolved.starts_with(root))) {
            continue;
        }
        let remainder = [canonical_root.as_path(), workspace_root, source_root_spelling]
            .into_iter()
            .find_map(|root| spelled.strip_prefix(root).ok())
            .ok_or(OUTSIDE)?;
        let relative = normalize_within(&canonical_root, remainder).map_err(|error| match error {
            PathError::Outside => OUTSIDE,
            PathError::Symlink => SYMLINK,
        })?;
        let absolute = canonical_root.join(&relative);
        if captured.iter().any(|root| absolute.starts_with(root)) {
            continue;
        }
        selectors.insert(MacroPathSelector {
            path: relative,
            listing: read.access == NativeMacroPathAccess::Listing,
            cargo_query: None,
        });
    }
    if selectors.len() > MAX_MACRO_PATH_INPUTS {
        return Err("procedural_macro_observation_limit");
    }
    Ok(selectors.into_iter().collect())
}

enum PathError {
    Outside,
    Symlink,
}

/// Where the operating system resolves `path`: its canonical form, or that of its deepest existing ancestor
/// followed by the missing components.
fn resolved_location(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = crate::utils::canonicalize_existing(current) {
            return Some(
                missing
                    .iter()
                    .rev()
                    .fold(canonical, |resolved, name| resolved.join(name)),
            );
        }
        let name = current.file_name()?;
        if name == OsStr::new("..") || name == OsStr::new(".") {
            return None;
        }
        missing.push(name.to_owned());
        current = current.parent()?;
    }
}

/// The package directory of a `cargo locate-project` query the macro ran with this process's `cargo`, if
/// the query is one Cargo answers from the inputs `CargoInputs` binds.
fn cargo_query_selector(spawn: &NativeMacroSpawn, workspace_root: &Path) -> Option<MacroPathSelector> {
    let cargo = std::env::var_os("CARGO")?;
    if Path::new(&spawn.program) != Path::new(&cargo) || !Path::new(&cargo).is_absolute() {
        return None;
    }
    // The first argument is the program name.
    let mut arguments = spawn.arguments.iter().skip(1).map(String::as_str);
    if arguments.next()? != "locate-project" {
        return None;
    }
    let current_directory = std::env::current_dir().ok()?;
    let mut manifest = None;
    while let Some(argument) = arguments.next() {
        match argument {
            "--workspace" | "--offline" | "--frozen" | "--locked" | "--quiet" | "-q" => {}
            "--message-format=plain" | "--message-format=json" => {}
            "--message-format" => {
                if !matches!(arguments.next()?, "plain" | "json") {
                    return None;
                }
            }
            "--manifest-path" => manifest = Some(arguments.next()?),
            _ => manifest = Some(argument.strip_prefix("--manifest-path=")?),
        }
    }
    let directory = match manifest {
        Some(manifest) => {
            let manifest = current_directory.join(manifest);
            if manifest.file_name() != Some(OsStr::new("Cargo.toml")) {
                return None;
            }
            manifest.parent()?.to_path_buf()
        }
        None => current_directory.clone(),
    };
    let directory = crate::utils::canonicalize_existing(&directory).ok()?;
    let cargo_home = crate::cargo::CargoConfigSnapshot::cargo_home(&current_directory)
        .ok()
        .and_then(|home| crate::utils::canonicalize_existing(&home).ok());
    let (root, relative) = if let Ok(relative) = directory.strip_prefix(workspace_root) {
        (CargoQueryRoot::Repository, relative)
    } else {
        (
            CargoQueryRoot::CargoHome,
            directory.strip_prefix(cargo_home.as_deref()?).ok()?,
        )
    };
    Some(MacroPathSelector {
        path: native_relative_path(relative).ok()?,
        listing: false,
        cargo_query: Some(root),
    })
}

fn capture_cargo_query(
    workspace_root: &Path,
    root: CargoQueryRoot,
    relative: &Path,
    budget: &mut NativeCaptureBudget,
) -> RailResult<MacroPathState> {
    let current_directory = std::env::current_dir()?;
    let base = match root {
        CargoQueryRoot::Repository => workspace_root.to_path_buf(),
        CargoQueryRoot::CargoHome => {
            crate::utils::canonicalize_existing(&crate::cargo::CargoConfigSnapshot::cargo_home(&current_directory)?)?
        }
    };
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| RailError::message("the compiler's cargo executable is not an absolute CARGO path"))?;
    let inputs = CargoInputs::capture(&base.join(relative), &current_directory, &cargo, None, budget)?;
    Ok(MacroPathState::CargoQuery {
        inputs_digest: format!("sha256:{}", ContentDigest::sha256(&serde_json::to_vec(&inputs)?)),
    })
}

/// Resolve `.` and `..` lexically below `root`, which is valid only while every traversed component is a
/// real directory, then require that no existing component of the result is a symbolic link.
fn normalize_within(root: &Path, remainder: &Path) -> Result<String, PathError> {
    let mut components = Vec::<&OsStr>::new();
    for component in remainder.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => components.push(name),
            Component::ParentDir => {
                let current = components.iter().fold(root.to_path_buf(), |path, name| path.join(name));
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {}
                    Ok(_) => return Err(PathError::Symlink),
                    // The operating system fails to resolve `missing/..`, so the read found nothing that a
                    // later state could change without also creating the missing directory.
                    Err(_) => return Err(PathError::Symlink),
                }
                components.pop().ok_or(PathError::Outside)?;
            }
            Component::RootDir | Component::Prefix(_) => return Err(PathError::Outside),
        }
    }
    let relative = components.iter().collect::<PathBuf>();
    if relative.as_os_str().is_empty() {
        return Ok(String::new());
    }
    reject_symlink_components(root, &relative).map_err(|_| PathError::Symlink)?;
    native_relative_path(&relative).map_err(|_| PathError::Outside)
}

/// Fail when an existing component below `root` is a symbolic link or not a directory where one is needed.
fn reject_symlink_components(root: &Path, relative: &Path) -> RailResult<()> {
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if crate::utils::is_symlink_or_reparse(&metadata) => {
                return Err(RailError::message("procedural-macro input crosses a symbolic link"));
            }
            Ok(metadata) if index + 1 < count && !metadata.is_dir() => {
                // A later component cannot exist below a file; the read found nothing.
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Capture the current state of every selected path.
pub(super) fn capture(
    workspace_root: &Path,
    selectors: &[MacroPathSelector],
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<Vec<MacroPathInput>> {
    validate_selectors(selectors)?;
    let canonical_root = crate::utils::canonicalize_existing(workspace_root)?;
    selectors
        .iter()
        .map(|selector| {
            budget.account_entry(&selector.path)?;
            Ok(MacroPathInput {
                path: selector.path.clone(),
                listing: selector.listing,
                cargo_query: selector.cargo_query,
                state: capture_state(&canonical_root, selector, started, budget)?,
            })
        })
        .collect()
}

fn capture_state(
    root: &Path,
    selector: &MacroPathSelector,
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<MacroPathState> {
    let relative = Path::new(&selector.path);
    if let Some(query_root) = selector.cargo_query {
        return capture_cargo_query(root, query_root, relative, budget);
    }
    reject_symlink_components(root, relative)?;
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(MacroPathState::Absent);
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.is_file() {
        let (content_digest, _, bytes) = capture_guarded_file(&path, started, budget)?;
        return Ok(MacroPathState::File {
            content_digest,
            bytes,
            mode: semantic_mode(&metadata),
        });
    }
    if !metadata.is_dir() {
        return Err(RailError::message(
            "procedural-macro input is neither a regular file nor a directory",
        ));
    }
    if !selector.listing {
        return Ok(MacroPathState::Directory);
    }
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("procedural-macro listing contains a non-UTF-8 name"))?;
        let file_type = entry.file_type()?;
        let kind = if file_type.is_symlink() {
            ListedEntryKind::Symlink
        } else if file_type.is_dir() {
            ListedEntryKind::Directory
        } else if file_type.is_file() {
            ListedEntryKind::File
        } else {
            ListedEntryKind::Other
        };
        entries.insert(name, kind);
        if entries.len() > MAX_LISTING_ENTRIES {
            return Err(RailError::message("procedural-macro listing exceeds its entry bound"));
        }
        budget.check(0, started.elapsed())?;
    }
    let encoded = serde_json::to_vec(&entries)?;
    Ok(MacroPathState::Listing {
        listing_digest: format!("sha256:{}", ContentDigest::sha256(&encoded)),
        entries: entries.len() as u64,
    })
}

/// Recapture every input and fail if any state changed since the action was captured.
pub(super) fn revalidate(workspace_root: &Path, inputs: &[MacroPathInput]) -> RailResult<()> {
    let selectors = inputs.iter().map(MacroPathInput::selector).collect::<Vec<_>>();
    let current = capture(
        workspace_root,
        &selectors,
        Instant::now(),
        &mut NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS),
    )?;
    if current != inputs {
        return Err(RailError::message(
            "a procedural-macro input changed before the restore commit",
        ));
    }
    Ok(())
}

pub(crate) fn validate_inputs(inputs: &[MacroPathInput]) -> RailResult<()> {
    let selectors = inputs.iter().map(MacroPathInput::selector).collect::<Vec<_>>();
    validate_selectors(&selectors)?;
    for input in inputs {
        let valid = match &input.state {
            MacroPathState::Absent | MacroPathState::Directory => true,
            MacroPathState::File { content_digest, .. } => super::validate_sha256(content_digest).is_ok(),
            MacroPathState::Listing { listing_digest, .. } => {
                input.listing && super::validate_sha256(listing_digest).is_ok()
            }
            MacroPathState::CargoQuery { inputs_digest } => {
                input.cargo_query.is_some() && super::validate_sha256(inputs_digest).is_ok()
            }
        };
        if !valid {
            return Err(RailError::message("procedural-macro input state is invalid"));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::compiler::native_input_protocol::NativeMacroPathRead;

    fn observation(root: &Path, reads: &[(&str, NativeMacroPathAccess)]) -> NativeMacroObservation {
        let mut paths = reads
            .iter()
            .map(|(path, access)| NativeMacroPathRead {
                path: format!("{}/{path}", root.display()),
                access: *access,
            })
            .collect::<Vec<_>>();
        paths.sort_unstable();
        NativeMacroObservation {
            paths,
            ..NativeMacroObservation::default()
        }
    }

    fn selector(path: &str, listing: bool) -> MacroPathSelector {
        MacroPathSelector {
            path: path.into(),
            listing,
            cargo_query: None,
        }
    }

    #[test]
    fn bypass_names_unobservable_effects_before_processes_and_secret_variables() {
        let mut observed = NativeMacroObservation::default();
        assert_eq!(bypass_reason(&observed), None);
        observed.environment = vec!["SERVICE_API_TOKEN".into()];
        assert_eq!(bypass_reason(&observed), Some("procedural_macro_secret_environment"));
        observed.unobservable = vec![Unobservable::Network, Unobservable::FileWrite];
        assert_eq!(bypass_reason(&observed), Some("procedural_macro_network"));
    }

    #[test]
    fn path_selectors_resolve_parent_components_and_skip_captured_namespaces() {
        let workspace = tempfile::tempdir().unwrap();
        let root = crate::utils::canonicalize_existing(workspace.path()).unwrap();
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();
        let observed = observation(
            &root,
            &[
                ("crates/a/../../Cargo.toml", NativeMacroPathAccess::Contents),
                ("shared", NativeMacroPathAccess::Listing),
                ("shared/./absent.txt", NativeMacroPathAccess::Entry),
                ("crates/a/src/lib.rs", NativeMacroPathAccess::Contents),
                ("", NativeMacroPathAccess::Entry),
            ],
        );
        let mut observed = observed;
        observed
            .paths
            .push(crate::compiler::native_input_protocol::NativeMacroPathRead {
                path: "/dev/null".into(),
                access: NativeMacroPathAccess::Contents,
            });
        let captured = root.join("crates/a/src");
        assert_eq!(
            path_selectors(&observed, &root, &root, &[captured.as_path()]),
            Ok(vec![
                selector("", false),
                selector("Cargo.toml", false),
                selector("shared", true),
                selector("shared/absent.txt", false),
            ])
        );
    }

    #[test]
    fn path_selectors_accept_the_cargo_root_spelling_and_reject_escapes_and_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let root = crate::utils::canonicalize_existing(temporary.path())
            .unwrap()
            .join("workspace");
        fs::create_dir_all(root.join("real")).unwrap();
        let spelling = root.with_file_name("spelling");
        std::os::unix::fs::symlink(&root, &spelling).unwrap();
        let through_spelling = observation(&spelling, &[("real", NativeMacroPathAccess::Entry)]);
        assert_eq!(
            path_selectors(&through_spelling, &root, &spelling, &[]),
            Ok(vec![selector("real", false)])
        );
        let outside = observation(&root, &[("../elsewhere", NativeMacroPathAccess::Contents)]);
        assert_eq!(
            path_selectors(&outside, &root, &root, &[]),
            Err("procedural_macro_read_outside_repository")
        );
        let unrelated = observation(Path::new("/etc"), &[("hosts", NativeMacroPathAccess::Contents)]);
        assert_eq!(
            path_selectors(&unrelated, &root, &root, &[]),
            Err("procedural_macro_read_outside_repository")
        );
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        for read in ["link/file", "link/../real"] {
            let through_link = observation(&root, &[(read, NativeMacroPathAccess::Contents)]);
            assert_eq!(
                path_selectors(&through_link, &root, &root, &[]),
                Err("procedural_macro_read_through_symlink"),
                "{read}"
            );
        }
    }

    #[test]
    fn captured_states_bind_contents_absence_directories_and_listings() {
        let workspace = tempfile::tempdir().unwrap();
        let root = crate::utils::canonicalize_existing(workspace.path()).unwrap();
        fs::create_dir_all(root.join("templates")).unwrap();
        fs::write(root.join("setting.txt"), "one").unwrap();
        fs::write(root.join("templates/first"), "").unwrap();
        let selectors = vec![
            selector("missing/below", false),
            selector("setting.txt", false),
            selector("templates", false),
            selector("templates", true),
        ];
        let capture_now = || {
            capture(
                &root,
                &selectors,
                Instant::now(),
                &mut NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS),
            )
            .unwrap()
        };
        let initial = capture_now();
        let states = initial.iter().map(|input| input.state.clone()).collect::<Vec<_>>();
        assert!(matches!(states[0], MacroPathState::Absent));
        assert!(matches!(&states[1], MacroPathState::File { bytes: 3, .. }));
        assert!(matches!(states[2], MacroPathState::Directory));
        assert!(matches!(states[3], MacroPathState::Listing { entries: 1, .. }));
        validate_inputs(&initial).unwrap();
        // Stored validations and publication proofs carry these records through JSON.
        let encoded = serde_json::to_vec(&initial).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<MacroPathInput>>(&encoded).unwrap(),
            initial
        );
        revalidate(&root, &initial).unwrap();

        for change in [
            |root: &Path| fs::write(root.join("setting.txt"), "two").unwrap(),
            |root: &Path| fs::write(root.join("templates/second"), "").unwrap(),
            |root: &Path| fs::create_dir_all(root.join("missing/below")).unwrap(),
        ] {
            let before = capture_now();
            change(&root);
            assert!(revalidate(&root, &before).is_err());
            assert_ne!(capture_now(), before);
        }
    }

    #[test]
    fn selectors_must_be_sorted_unique_and_normalized() {
        assert!(validate_selectors(&[selector("b", false), selector("a", false)]).is_err());
        assert!(validate_selectors(&[selector("a", false), selector("a", false)]).is_err());
        assert!(validate_selectors(&[selector("a/../b", false)]).is_err());
        assert!(validate_selectors(&[selector("/a", false)]).is_err());
        validate_selectors(&[selector("", true), selector("a", false), selector("a", true)]).unwrap();
    }
}
