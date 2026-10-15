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
//! - an upward search above the workspace root, such as the `.env` lookup of `sqlx`, binds the absence of
//!   the searched name in every directory above the root. Directories outside the repository decide that
//!   absence, so such a result stays in the local cache;
//! - any other read or process, a read through a symbolic link, and every unobservable effect bypass by name.
//!   The cache report names every effect, process, and read that stops the binding, not only the first.

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

use super::cargo_inputs::{CargoInputs, CargoQuery};
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
    /// The path is one name, absent from every directory above the workspace root.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) above_root: bool,
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
            || self.above_root && (self.listing || self.cargo_query.is_some() || !is_single_name(&self.path))
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

/// One normal path component, as an upward search joins it to each directory.
fn is_single_name(name: &str) -> bool {
    !name.contains(['/', '\\'])
        && matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    above_root: bool,
    state: MacroPathState,
}

impl MacroPathInput {
    pub(super) fn selector(&self) -> MacroPathSelector {
        MacroPathSelector {
            path: self.path.clone(),
            listing: self.listing,
            cargo_query: self.cargo_query,
            above_root: self.above_root,
        }
    }
}

const OUTSIDE: &str = "procedural_macro_read_outside_repository";
const SYMLINK: &str = "procedural_macro_read_through_symlink";
const PROCESS: &str = "procedural_macro_process_unmodeled";
/// Each detail section names at most this many processes or paths, then their count.
const MAX_DETAIL_SUBJECTS: usize = 4;

/// Why a unit that loaded procedural macros bypasses reuse.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MacroBypass {
    /// The first blocking condition in report order: effects, secret variables, processes, then reads.
    pub(super) reason: &'static str,
    /// Every blocking effect, process, and read, for the cache report.
    pub(super) detail: String,
}

/// Bind every observed read, or name every condition that stops the binding.
///
/// The bypass keeps one reason, so the counts of the cache report still add up to the bypassed units. Its
/// detail names each blocking condition, so one cold build shows every class that one unit must clear.
pub(super) fn select(
    observation: &NativeMacroObservation,
    workspace_root: &Path,
    source_root_spelling: &Path,
    captured: &[&Path],
) -> Result<Vec<MacroPathSelector>, MacroBypass> {
    let selected = path_selectors(observation, workspace_root, source_root_spelling, captured);
    let Some(reason) = bypass_reason(observation) else {
        return selected;
    };
    let mut detail = bypass_detail(observation);
    if let Err(paths) = selected {
        detail = format!("{detail}; {}", paths.detail);
    }
    Err(MacroBypass { reason, detail })
}

/// The bypass reason for effects that no key can bind, if the macro produced any.
fn bypass_reason(observation: &NativeMacroObservation) -> Option<&'static str> {
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

/// What a bypassed macro did, for the cache report: the imports that no table classifies, the effects, and
/// the secret-named variables that it read.
fn bypass_detail(observation: &NativeMacroObservation) -> String {
    let mut sections = Vec::new();
    if !observation.unobservable_imports.is_empty() {
        sections.push(format!(
            "procedural-macro imports without a classification: {}",
            observation.unobservable_imports.join(", ")
        ));
    }
    if !observation.unobservable.is_empty() {
        let effects = observation.unobservable.iter().map(|effect| {
            serde_json::to_value(effect)
                .ok()
                .and_then(|name| name.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{effect:?}"))
        });
        sections.push(format!(
            "procedural-macro effects: {}",
            effects.collect::<Vec<_>>().join(", ")
        ));
    }
    let secret = observation
        .environment
        .iter()
        .filter(|name| is_secret_name(name))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !secret.is_empty() {
        sections.push(format!(
            "procedural macro read secret-named variables: {}",
            secret.join(", ")
        ));
    }
    sections.join("; ")
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

/// Map observed reads and processes to selectors, or name every one that cannot bind.
///
/// `source_root_spelling` is the spelling Cargo used for the workspace root, which resolves to
/// `workspace_root`. `captured` names canonical directories the action already binds whole.
pub(super) fn path_selectors(
    observation: &NativeMacroObservation,
    workspace_root: &Path,
    source_root_spelling: &Path,
    captured: &[&Path],
) -> Result<Vec<MacroPathSelector>, MacroBypass> {
    let canonical_root = crate::utils::canonicalize_existing(workspace_root).map_err(|_| MacroBypass {
        reason: OUTSIDE,
        detail: format!("the workspace root '{}' cannot be resolved", workspace_root.display()),
    })?;
    // Only a read outside the repository needs the directories above the root.
    let mut above = None;
    let mut selectors = BTreeSet::new();
    let mut blocked = Blocked::default();
    for spawn in &observation.spawns {
        match cargo_query_selector(spawn, &canonical_root) {
            Some(selector) => {
                selectors.insert(selector);
            }
            None => blocked.add(PROCESS, process_name(spawn)),
        }
    }
    for read in &observation.paths {
        let spelled = Path::new(&read.path);
        if STATELESS_DEVICES.contains(&read.path.as_str()) {
            continue;
        }
        let resolved = resolved_location(spelled);
        if resolved
            .as_deref()
            .is_some_and(|resolved| captured.iter().any(|root| resolved.starts_with(root)))
        {
            continue;
        }
        let Some(remainder) = [canonical_root.as_path(), workspace_root, source_root_spelling]
            .into_iter()
            .find_map(|root| spelled.strip_prefix(root).ok())
        else {
            let above =
                above.get_or_insert_with(|| directories_above(&canonical_root, [workspace_root, source_root_spelling]));
            match resolved
                .as_deref()
                .map(|resolved| above_root_read(resolved, read.access, above))
            {
                Some(AboveRoot::Directory) => {}
                Some(AboveRoot::Absent(selector)) => {
                    selectors.insert(selector);
                }
                Some(AboveRoot::Unbound) | None => blocked.add(OUTSIDE, read.path.clone()),
            }
            continue;
        };
        let relative = match normalize_within(&canonical_root, remainder) {
            Ok(relative) => relative,
            Err(PathError::Outside) => {
                blocked.add(OUTSIDE, read.path.clone());
                continue;
            }
            Err(PathError::Symlink) => {
                blocked.add(SYMLINK, read.path.clone());
                continue;
            }
        };
        let absolute = canonical_root.join(&relative);
        if captured.iter().any(|root| absolute.starts_with(root)) {
            continue;
        }
        selectors.insert(MacroPathSelector {
            path: relative,
            listing: read.access == NativeMacroPathAccess::Listing,
            cargo_query: None,
            above_root: false,
        });
    }
    if let Some(bypass) = blocked.bypass() {
        return Err(bypass);
    }
    if selectors.len() > MAX_MACRO_PATH_INPUTS {
        return Err(MacroBypass {
            reason: "procedural_macro_observation_limit",
            detail: format!("procedural macro read more than {MAX_MACRO_PATH_INPUTS} paths"),
        });
    }
    Ok(selectors.into_iter().collect())
}

/// Processes and reads that cannot bind, grouped by reason in the order they were first found.
#[derive(Default)]
struct Blocked {
    sections: Vec<(&'static str, Vec<String>)>,
}

impl Blocked {
    fn add(&mut self, reason: &'static str, subject: String) {
        match self.sections.iter_mut().find(|(known, _)| *known == reason) {
            Some((_, subjects)) => subjects.push(subject),
            None => self.sections.push((reason, vec![subject])),
        }
    }

    fn bypass(self) -> Option<MacroBypass> {
        let reason = self.sections.first()?.0;
        let detail = self
            .sections
            .iter()
            .map(|(reason, subjects)| {
                let label = match *reason {
                    PROCESS => "procedural-macro processes without a model",
                    SYMLINK => "procedural-macro reads through a symbolic link",
                    _ => "procedural-macro reads outside the repository",
                };
                let shown = subjects.iter().take(MAX_DETAIL_SUBJECTS).map(String::as_str);
                let more = subjects.len().saturating_sub(MAX_DETAIL_SUBJECTS);
                let more = if more == 0 {
                    String::new()
                } else {
                    format!(", and {more} more")
                };
                format!(
                    "{label} ({}): {}{more}",
                    subjects.len(),
                    shown.collect::<Vec<_>>().join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(MacroBypass { reason, detail })
    }
}

/// The program name and subcommand of a process, such as `cargo metadata`. Other arguments can carry
/// values that a report must not show.
fn process_name(spawn: &NativeMacroSpawn) -> String {
    let program = Path::new(&spawn.program)
        .file_name()
        .map_or_else(|| spawn.program.clone(), |name| name.to_string_lossy().into_owned());
    match spawn.arguments.get(1) {
        Some(subcommand) if !subcommand.starts_with('-') => format!("{program} {subcommand}"),
        _ => program,
    }
}

/// Every existing directory above the workspace root, in canonical form, as each root spelling reaches it.
///
/// A macro searches upward from the spelling that it got, such as `CARGO_MANIFEST_DIR`, or from the canonical
/// working directory. A spelling through a symbolic link reaches other directories than the canonical root.
fn directories_above<'a>(canonical_root: &Path, spellings: impl IntoIterator<Item = &'a Path>) -> BTreeSet<PathBuf> {
    let mut directories = canonical_root
        .ancestors()
        .skip(1)
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    for spelling in spellings {
        for directory in spelling.ancestors().skip(1) {
            if let Ok(directory) = crate::utils::canonicalize_existing(directory)
                && !directory.starts_with(canonical_root)
            {
                directories.insert(directory);
            }
        }
    }
    directories
}

/// How a read outside the repository relates to the directories above the workspace root.
enum AboveRoot {
    /// The metadata of a directory above the root, which exists while the root does.
    Directory,
    /// A missing name in a directory above the root, which binds as absent from all of them.
    Absent(MacroPathSelector),
    Unbound,
}

fn above_root_read(resolved: &Path, access: NativeMacroPathAccess, above: &BTreeSet<PathBuf>) -> AboveRoot {
    if above.contains(resolved) {
        return if access == NativeMacroPathAccess::Entry {
            AboveRoot::Directory
        } else {
            AboveRoot::Unbound
        };
    }
    let (Some(parent), Some(name)) = (resolved.parent(), resolved.file_name().and_then(OsStr::to_str)) else {
        return AboveRoot::Unbound;
    };
    if access == NativeMacroPathAccess::Listing || !above.contains(parent) || !is_single_name(name) {
        return AboveRoot::Unbound;
    }
    // A name that exists above the root is a host file, possibly with credentials, so it does not bind.
    match fs::symlink_metadata(resolved) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AboveRoot::Absent(MacroPathSelector {
            path: name.to_string(),
            listing: false,
            cargo_query: None,
            above_root: true,
        }),
        Ok(_) | Err(_) => AboveRoot::Unbound,
    }
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
        above_root: false,
    })
}

fn capture_cargo_query(
    workspace_root: &Path,
    root: CargoQueryRoot,
    relative: &Path,
    budget: &mut NativeCaptureBudget,
) -> RailResult<MacroPathState> {
    cargo_query_state(&cargo_query_inputs(workspace_root, root, relative, budget)?)
}

fn cargo_query_state(inputs: &CargoInputs) -> RailResult<MacroPathState> {
    Ok(MacroPathState::CargoQuery {
        inputs_digest: format!("sha256:{}", ContentDigest::sha256(&serde_json::to_vec(inputs)?)),
    })
}

fn cargo_query_inputs(
    workspace_root: &Path,
    root: CargoQueryRoot,
    relative: &Path,
    budget: &mut NativeCaptureBudget,
) -> RailResult<CargoInputs> {
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
    CargoInputs::capture(
        &base.join(relative),
        &current_directory,
        &cargo,
        CargoQuery::LocateProject,
        None,
        budget,
    )
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
///
/// `source_root_spelling` is the spelling Cargo used for the workspace root. A name above the root binds in
/// every directory that the canonical root and that spelling reach.
pub(super) fn capture(
    workspace_root: &Path,
    source_root_spelling: &Path,
    selectors: &[MacroPathSelector],
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<Vec<MacroPathInput>> {
    validate_selectors(selectors)?;
    let canonical_root = crate::utils::canonicalize_existing(workspace_root)?;
    let above = selectors
        .iter()
        .any(|selector| selector.above_root)
        .then(|| directories_above(&canonical_root, [workspace_root, source_root_spelling]));
    selectors
        .iter()
        .map(|selector| {
            budget.account_entry(&selector.path)?;
            let state = match &above {
                Some(above) if selector.above_root => capture_above_root(above, &selector.path, started, budget)?,
                _ => capture_state(&canonical_root, selector, started, budget)?,
            };
            Ok(MacroPathInput {
                path: selector.path.clone(),
                listing: selector.listing,
                cargo_query: selector.cargo_query,
                above_root: selector.above_root,
                state,
            })
        })
        .collect()
}

/// `name` must be absent from every directory above the root. A name that exists there is a host file that
/// the action cannot bind, so the capture fails and the unit compiles.
fn capture_above_root(
    above: &BTreeSet<PathBuf>,
    name: &str,
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<MacroPathState> {
    for directory in above {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Ok(_) => {
                return Err(RailError::message(format!(
                    "procedural-macro input '{}' exists above the workspace root",
                    path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        }
        budget.check(0, started.elapsed())?;
    }
    Ok(MacroPathState::Absent)
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

/// Every bound path. A Cargo query binds what Cargo reads, so it is recaptured, and it must still match.
///
/// A name above the workspace root is left out. Directories such as the home directory change for unrelated
/// reasons at any time, as do the directories where Cargo probes for configuration. The capture and every
/// revalidation still check the absence.
pub(super) fn post_execution_paths(
    workspace_root: &Path,
    inputs: &[MacroPathInput],
) -> RailResult<Vec<super::PostExecutionPath>> {
    let canonical_root = crate::utils::canonicalize_existing(workspace_root)?;
    let mut paths = Vec::new();
    for input in inputs.iter().filter(|input| !input.above_root) {
        let relative = Path::new(&input.path);
        match (&input.state, input.cargo_query) {
            (MacroPathState::CargoQuery { .. }, Some(root)) => {
                let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
                let query = cargo_query_inputs(&canonical_root, root, relative, &mut budget)?;
                if cargo_query_state(&query)? != input.state {
                    return Err(RailError::message(
                        "a procedural macro's Cargo query inputs changed after the capture",
                    ));
                }
                paths.extend(query.post_execution_paths());
            }
            (MacroPathState::Absent, _) => paths.push(super::PostExecutionPath::Absent(canonical_root.join(relative))),
            _ => paths.push(super::PostExecutionPath::Present(canonical_root.join(relative))),
        }
    }
    Ok(paths)
}

/// Recapture every input and fail if any state changed since the action was captured.
pub(super) fn revalidate(
    workspace_root: &Path,
    source_root_spelling: &Path,
    inputs: &[MacroPathInput],
) -> RailResult<()> {
    let selectors = inputs.iter().map(MacroPathInput::selector).collect::<Vec<_>>();
    let current = capture(
        workspace_root,
        source_root_spelling,
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
            MacroPathState::Absent => true,
            _ if input.above_root => false,
            MacroPathState::Directory => true,
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

    /// A name that no directory above a test workspace holds.
    const PROBE: &str = ".cargo-rail-above-root-probe";

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
            above_root: false,
        }
    }

    fn above_root(name: &str) -> MacroPathSelector {
        MacroPathSelector {
            above_root: true,
            ..selector(name, false)
        }
    }

    fn read(path: &Path, access: NativeMacroPathAccess) -> NativeMacroPathRead {
        NativeMacroPathRead {
            path: path.to_str().expect("UTF-8 test path").into(),
            access,
        }
    }

    fn reasons(selected: Result<Vec<MacroPathSelector>, MacroBypass>) -> Result<Vec<MacroPathSelector>, &'static str> {
        selected.map_err(|bypass| bypass.reason)
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
            reasons(path_selectors(&observed, &root, &root, &[captured.as_path()])),
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
            reasons(path_selectors(&through_spelling, &root, &spelling, &[])),
            Ok(vec![selector("real", false)])
        );
        let outside = observation(&root, &[("../elsewhere", NativeMacroPathAccess::Contents)]);
        assert_eq!(
            reasons(path_selectors(&outside, &root, &root, &[])),
            Err("procedural_macro_read_outside_repository")
        );
        let unrelated = observation(Path::new("/etc"), &[("hosts", NativeMacroPathAccess::Contents)]);
        assert_eq!(
            reasons(path_selectors(&unrelated, &root, &root, &[])),
            Err("procedural_macro_read_outside_repository")
        );
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        for read in ["link/file", "link/../real"] {
            let through_link = observation(&root, &[(read, NativeMacroPathAccess::Contents)]);
            assert_eq!(
                reasons(path_selectors(&through_link, &root, &root, &[])),
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
        revalidate(&root, &root, &initial).unwrap();

        for change in [
            |root: &Path| fs::write(root.join("setting.txt"), "two").unwrap(),
            |root: &Path| fs::write(root.join("templates/second"), "").unwrap(),
            |root: &Path| fs::create_dir_all(root.join("missing/below")).unwrap(),
        ] {
            let before = capture_now();
            change(&root);
            assert!(revalidate(&root, &root, &before).is_err());
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

    #[test]
    fn a_bypass_keeps_the_first_reason_and_names_every_blocking_condition() {
        let workspace = tempfile::tempdir().unwrap();
        let root = crate::utils::canonicalize_existing(workspace.path()).unwrap();
        fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let metadata = NativeMacroSpawn {
            program: "/elsewhere/bin/cargo".into(),
            arguments: [
                "cargo",
                "metadata",
                "--format-version=1",
                "--no-deps",
                "--manifest-path=/secret",
            ]
            .map(String::from)
            .to_vec(),
        };
        let mut observed = observation(&root, &[("link/file", NativeMacroPathAccess::Contents)]);
        observed.spawns = vec![metadata];
        observed.paths.extend(
            [
                "/etc/one",
                "/etc/two",
                "/etc/three",
                "/etc/four",
                "/etc/five",
                "/etc/six",
            ]
            .map(|path| read(Path::new(path), NativeMacroPathAccess::Contents)),
        );
        observed.paths.sort_unstable();

        // Without an effect, the process is the first reason, and every read still appears.
        let bypass = select(&observed, &root, &root, &[]).unwrap_err();
        assert_eq!(bypass.reason, "procedural_macro_process_unmodeled");
        assert_eq!(
            bypass.detail,
            format!(
                "procedural-macro processes without a model (1): cargo metadata; \
                 procedural-macro reads outside the repository (6): /etc/five, /etc/four, /etc/one, /etc/six, and 2 more; \
                 procedural-macro reads through a symbolic link (1): {}",
                root.join("link/file").display()
            )
        );
        assert!(!bypass.detail.contains("/secret"), "{}", bypass.detail);

        // An effect stays the reason, and the detail adds the processes and reads behind it.
        observed.unobservable = vec![Unobservable::ProcessControl, Unobservable::EnvironmentEnumeration];
        observed.environment = vec!["SERVICE_API_TOKEN".into()];
        let bypass = select(&observed, &root, &root, &[]).unwrap_err();
        assert_eq!(bypass.reason, "procedural_macro_process_control");
        assert!(
            bypass.detail.starts_with(
                "procedural-macro effects: process_control, environment_enumeration; \
                 procedural macro read secret-named variables: SERVICE_API_TOKEN; \
                 procedural-macro processes without a model (1): cargo metadata; \
                 procedural-macro reads outside the repository (6)"
            ),
            "{}",
            bypass.detail
        );

        // A unit with nothing to report binds its reads.
        let clean = observation(&root, &[("real", NativeMacroPathAccess::Entry)]);
        assert_eq!(
            reasons(select(&clean, &root, &root, &[])),
            Ok(vec![selector("real", false)])
        );
    }

    #[test]
    fn a_missing_name_above_the_root_binds_while_it_stays_absent_everywhere_above() {
        let temporary = tempfile::tempdir().unwrap();
        let above = crate::utils::canonicalize_existing(temporary.path()).unwrap();
        let root = above.join("workspace");
        fs::create_dir_all(root.join("member")).unwrap();
        // An upward search from the member: each directory, and the searched name in each of them.
        let mut observed = observation(
            &root,
            &[
                ("member", NativeMacroPathAccess::Entry),
                (&format!("member/{PROBE}"), NativeMacroPathAccess::Contents),
                ("", NativeMacroPathAccess::Entry),
                (PROBE, NativeMacroPathAccess::Contents),
            ],
        );
        for directory in above.ancestors() {
            observed.paths.push(read(directory, NativeMacroPathAccess::Entry));
            observed
                .paths
                .push(read(&directory.join(PROBE), NativeMacroPathAccess::Contents));
        }
        observed.paths.sort_unstable();
        let expected = vec![
            selector("", false),
            selector(PROBE, false),
            above_root(PROBE),
            selector("member", false),
            selector(&format!("member/{PROBE}"), false),
        ];
        assert_eq!(
            reasons(path_selectors(&observed, &root, &root, &[])),
            Ok(expected.clone())
        );

        let capture_now = |spelling: &Path| {
            capture(
                &root,
                spelling,
                &expected,
                Instant::now(),
                &mut NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS),
            )
        };
        let initial = capture_now(&root).unwrap();
        validate_inputs(&initial).unwrap();
        let encoded = serde_json::to_vec(&initial).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<MacroPathInput>>(&encoded).unwrap(),
            initial
        );
        revalidate(&root, &root, &initial).unwrap();
        // The after-execution check leaves out directories above the root, which change for unrelated reasons.
        let checked = post_execution_paths(&root, &initial).unwrap();
        assert!(
            checked.iter().all(|path| match path {
                super::super::PostExecutionPath::Absent(path) | super::super::PostExecutionPath::Present(path) =>
                    path.starts_with(&root),
            }),
            "{checked:?}"
        );

        // The name appears above the root: no stored result revalidates, and a new observation bypasses.
        fs::write(above.join(PROBE), "SETTING=1\n").unwrap();
        assert!(revalidate(&root, &root, &initial).is_err());
        capture_now(&root).expect_err("an existing name above the root stops the capture");
        let bypass = path_selectors(&observed, &root, &root, &[]).unwrap_err();
        assert_eq!(bypass.reason, "procedural_macro_read_outside_repository");
        assert!(
            bypass.detail.contains(&above.join(PROBE).display().to_string()),
            "{}",
            bypass.detail
        );
        fs::remove_file(above.join(PROBE)).unwrap();
        revalidate(&root, &root, &initial).unwrap();

        // Only the metadata of a directory above the root and a missing name in one bind. An existing sibling
        // of the workspace is a host entry.
        fs::create_dir(above.join("sibling")).unwrap();
        for (path, access) in [
            (above.clone(), NativeMacroPathAccess::Listing),
            (above.clone(), NativeMacroPathAccess::Contents),
            (above.join(PROBE), NativeMacroPathAccess::Listing),
            (above.join("other").join(PROBE), NativeMacroPathAccess::Contents),
            (above.join("sibling"), NativeMacroPathAccess::Entry),
        ] {
            let mut unbound = observation(&root, &[]);
            unbound.paths.push(read(&path, access));
            assert_eq!(
                reasons(path_selectors(&unbound, &root, &root, &[])),
                Err("procedural_macro_read_outside_repository"),
                "{} {access:?}",
                path.display()
            );
        }
    }

    #[test]
    fn a_missing_name_above_the_root_binds_every_directory_that_the_root_spelling_reaches() {
        let (real, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let root = crate::utils::canonicalize_existing(real.path())
            .unwrap()
            .join("workspace");
        fs::create_dir_all(&root).unwrap();
        let other = crate::utils::canonicalize_existing(other.path()).unwrap();
        let spelling = other.join("link");
        std::os::unix::fs::symlink(&root, &spelling).unwrap();
        // The macro searches upward from the spelling that Cargo gave it.
        let mut observed = observation(&root, &[]);
        observed
            .paths
            .push(read(&other.join(PROBE), NativeMacroPathAccess::Contents));
        assert_eq!(
            reasons(path_selectors(&observed, &root, &spelling, &[])),
            Ok(vec![above_root(PROBE)])
        );
        assert_eq!(
            reasons(path_selectors(&observed, &root, &root, &[])),
            Err("procedural_macro_read_outside_repository"),
            "a directory that no root spelling reaches does not bind"
        );
        let capture_with = |spelling: &Path| {
            capture(
                &root,
                spelling,
                &[above_root(PROBE)],
                Instant::now(),
                &mut NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS),
            )
        };
        let initial = capture_with(&spelling).unwrap();
        fs::write(other.join(PROBE), "").unwrap();
        capture_with(&spelling).expect_err("the spelling reaches the existing name");
        assert!(revalidate(&root, &spelling, &initial).is_err());
        assert_eq!(
            capture_with(&root).unwrap(),
            initial,
            "the canonical root does not reach the spelling's parent"
        );
    }

    #[test]
    fn a_name_above_the_root_is_one_normal_component_with_an_absent_state() {
        for invalid in [
            MacroPathSelector {
                listing: true,
                ..above_root(PROBE)
            },
            MacroPathSelector {
                cargo_query: Some(CargoQueryRoot::Repository),
                ..above_root(PROBE)
            },
            above_root("nested/name"),
            above_root(".."),
            above_root("."),
            above_root(""),
        ] {
            assert!(
                validate_selectors(std::slice::from_ref(&invalid)).is_err(),
                "{invalid:?}"
            );
        }
        validate_selectors(&[selector(PROBE, false), above_root(PROBE)]).unwrap();
        // Stored selectors from before this field bind repository paths.
        let stored: MacroPathSelector = serde_json::from_str(r#"{"path":".env"}"#).unwrap();
        assert!(!stored.above_root);
        let present = MacroPathInput {
            path: PROBE.into(),
            listing: false,
            cargo_query: None,
            above_root: true,
            state: MacroPathState::Directory,
        };
        assert!(validate_inputs(&[present]).is_err());
    }
}
