//! Native libraries that rustc finds itself in the `-L native=` and `-L all=` directories.
//!
//! A native search directory has three roles, and each binds what the directory selects, never its content:
//!
//! - rustc looks up Rust crates in `-L all=` and bare `-L` directories, and `rust_inputs` binds those searches;
//! - the linker resolves the libraries it is asked to link, and the linker witness binds the files it read
//!   and the lookups that failed;
//! - rustc resolves some native libraries itself, to bundle a static library into an rlib or staticlib or to
//!   name a library to the linker by path. The compiler driver reports each such search with every
//!   candidate that rustc can test, in rustc's order.
//!
//! This module binds the third role. The selector is the search itself, the same in every execution of one
//! action. The captured state is rustc's selection: each candidate before the first that exists stays
//! absent, and the match keeps its spelling, canonical target, and content. Candidates after the match are
//! not inputs. A directory that selects no such library binds only its spelling, which the compiler
//! arguments already carry, so symbolic links, unrelated files, and subdirectories there neither block nor
//! invalidate reuse.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compiler::native_input_protocol::{
    MAX_NATIVE_LIBRARY_PROBES, MAX_NATIVE_LIBRARY_SEARCHES, NativeLibraryForm, NativeLibrarySearch,
    native_library_candidate_valid, native_library_name_valid,
};
use crate::error::{RailError, RailResult};

use super::{LINK_CAPTURE_LIMITS, LinkFileWitness, NativeCaptureBudget, capture_link_file, validate_link_file};

/// One candidate that a search tests: a native search directory, by its position among the action's
/// `-L native=`, `-L all=`, and bare `-L` arguments, and a file name in it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeLibraryCandidate {
    pub(crate) directory: u32,
    pub(crate) file: String,
}

/// One native-library search that rustc performs in every execution of the same base action, with every
/// candidate it can test, in its order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeLibrarySelector {
    pub(crate) name: String,
    pub(crate) form: NativeLibraryForm,
    pub(crate) verbatim: bool,
    pub(crate) candidates: Vec<NativeLibraryCandidate>,
}

impl NativeLibrarySelector {
    fn validate(&self) -> RailResult<()> {
        let mut candidates = std::collections::BTreeSet::new();
        let valid = native_library_name_valid(&self.name)
            && self.candidates.len() <= MAX_NATIVE_LIBRARY_PROBES
            && self
                .candidates
                .windows(2)
                .all(|pair| pair[0].directory <= pair[1].directory)
            && self.candidates.iter().all(|candidate| {
                native_library_candidate_valid(&self.name, self.verbatim, &candidate.file)
                    && candidates.insert(candidate)
            });
        if !valid {
            return Err(RailError::message(
                "native library selector is not a bounded rustc search",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_selectors(selectors: &[NativeLibrarySelector]) -> RailResult<()> {
    if selectors.len() > MAX_NATIVE_LIBRARY_SEARCHES
        || selectors.windows(2).any(|pair| {
            (&pair[0].name, pair[0].form, pair[0].verbatim) >= (&pair[1].name, pair[1].form, pair[1].verbatim)
        })
    {
        return Err(RailError::message(
            "native library selectors are not bounded, sorted, and unique",
        ));
    }
    selectors.iter().try_for_each(NativeLibrarySelector::validate)
}

/// What one candidate held when the action was captured, up to and including the first present candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "file", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum NativeLibraryCandidateState {
    Absent,
    /// The library rustc selected: its spelling, canonical target, and content.
    Present(LinkFileWitness),
}

/// One bound search and the captured state of each candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeLibraryInput {
    selector: NativeLibrarySelector,
    states: Vec<NativeLibraryCandidateState>,
}

impl NativeLibraryInput {
    pub(super) fn selector(&self) -> &NativeLibrarySelector {
        &self.selector
    }
}

pub(crate) fn validate_inputs(inputs: &[NativeLibraryInput]) -> RailResult<()> {
    let selectors = inputs.iter().map(|input| input.selector.clone()).collect::<Vec<_>>();
    validate_selectors(&selectors)?;
    // The states end at the first present candidate, or cover every candidate when none is present.
    for input in inputs {
        let last = input.states.len().checked_sub(1);
        if input.states.len() > input.selector.candidates.len()
            || input.states.len() < input.selector.candidates.len()
                && !matches!(input.states.last(), Some(NativeLibraryCandidateState::Present(_)))
            || input.states.iter().enumerate().any(|(index, state)| match state {
                NativeLibraryCandidateState::Absent => false,
                NativeLibraryCandidateState::Present(file) => {
                    Some(index) != last
                        || validate_link_file(file).is_err()
                        || !Path::new(&file.path).ends_with(&input.selector.candidates[index].file)
                }
            })
        {
            return Err(RailError::message("native library input state is invalid"));
        }
    }
    Ok(())
}

/// Selectors for the searches that the compiler driver observed, each candidate's directory resolved to
/// its position among `directories`, the action's native search directories in argument order.
///
/// Rustc spells a directory as its argument names it. The recorded arguments may name the same directory
/// through another root spelling, so a candidate matches its own spelling first and its canonical
/// directory otherwise, never at an earlier position than the previous candidate.
pub(super) fn selectors(
    searches: &[NativeLibrarySearch],
    directories: &[PathBuf],
) -> RailResult<Vec<NativeLibrarySelector>> {
    let canonical = directories
        .iter()
        .map(|directory| crate::utils::canonicalize_allow_missing(directory).map_err(RailError::from))
        .collect::<RailResult<Vec<_>>>()?;
    let selectors = searches
        .iter()
        .map(|search| {
            search.validate().map_err(RailError::message)?;
            let mut position = 0usize;
            let mut candidates = Vec::with_capacity(search.probes.len());
            for probe in &search.probes {
                let spelling = Path::new(&probe.directory);
                let resolved = crate::utils::canonicalize_allow_missing(spelling)?;
                position = (position..directories.len())
                    .find(|index| directories[*index] == spelling)
                    .or_else(|| (position..directories.len()).find(|index| canonical[*index] == resolved))
                    .ok_or_else(|| {
                        RailError::message(format!(
                            "compiler searched a native library in '{}', which is not a native search directory of the action",
                            spelling.display()
                        ))
                    })?;
                candidates.push(NativeLibraryCandidate {
                    directory: u32::try_from(position)
                        .map_err(|_| RailError::message("native search directory position overflowed"))?,
                    file: probe.file.clone(),
                });
            }
            Ok(NativeLibrarySelector {
                name: search.name.clone(),
                form: search.form,
                verbatim: search.verbatim,
                candidates,
            })
        })
        .collect::<RailResult<Vec<_>>>()?;
    validate_selectors(&selectors)?;
    Ok(selectors)
}

/// Capture the current state of every selected candidate. Returns the inputs and the bytes hashed.
pub(super) fn capture(
    selectors: &[NativeLibrarySelector],
    directories: &[PathBuf],
) -> RailResult<(Vec<NativeLibraryInput>, u64)> {
    validate_selectors(selectors)?;
    let started = Instant::now();
    let mut budget = NativeCaptureBudget::new(LINK_CAPTURE_LIMITS);
    let inputs = selectors
        .iter()
        .map(|selector| {
            // Rustc selects the first candidate that exists; later candidates are not inputs.
            let mut states = Vec::new();
            for candidate in &selector.candidates {
                let directory = usize::try_from(candidate.directory)
                    .ok()
                    .and_then(|index| directories.get(index))
                    .ok_or_else(|| {
                        RailError::message("native library selector names a search directory the action lacks")
                    })?;
                let state = capture_candidate(&directory.join(&candidate.file), started, &mut budget)?;
                let selected = matches!(state, NativeLibraryCandidateState::Present(_));
                states.push(state);
                if selected {
                    break;
                }
            }
            Ok(NativeLibraryInput {
                selector: selector.clone(),
                states,
            })
        })
        .collect::<RailResult<Vec<_>>>()?;
    validate_inputs(&inputs)?;
    Ok((inputs, budget.bytes_hashed))
}

/// One candidate as rustc's `Path::exists` sees it: present when the path, with symbolic links followed,
/// names a file. An error other than a missing entry cannot prove that the candidate stays absent.
fn capture_candidate(
    path: &Path,
    started: Instant,
    budget: &mut NativeCaptureBudget,
) -> RailResult<NativeLibraryCandidateState> {
    match fs::metadata(path) {
        Ok(_) => {
            let (file, _) = capture_link_file(path, started, budget)?;
            Ok(NativeLibraryCandidateState::Present(file))
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            budget.account_entry(&path.to_string_lossy())?;
            Ok(NativeLibraryCandidateState::Absent)
        }
        Err(error) => Err(RailError::message(format!(
            "native library candidate '{}' cannot be observed: {error}",
            path.display()
        ))),
    }
}

/// The canonical file of each library that a bound search selected.
pub(super) fn selected_files(inputs: &[NativeLibraryInput]) -> impl Iterator<Item = &str> {
    inputs.iter().filter_map(|input| match input.states.last() {
        Some(NativeLibraryCandidateState::Present(file)) => Some(file.canonical_path.as_str()),
        _ => None,
    })
}

/// Every captured candidate: the selected library as rustc spelled it, and each absent candidate before it.
pub(super) fn post_execution_paths(
    inputs: &[NativeLibraryInput],
    directories: &[PathBuf],
) -> RailResult<Vec<super::PostExecutionPath>> {
    let mut paths = Vec::new();
    for input in inputs {
        for (candidate, state) in input.selector.candidates.iter().zip(&input.states) {
            let directory = usize::try_from(candidate.directory)
                .ok()
                .and_then(|index| directories.get(index))
                .ok_or_else(|| {
                    RailError::message("native library selector names a search directory the action lacks")
                })?;
            let path = directory.join(&candidate.file);
            paths.push(match state {
                NativeLibraryCandidateState::Absent => super::PostExecutionPath::Absent(path),
                NativeLibraryCandidateState::Present(_) => super::PostExecutionPath::Present(path),
            });
        }
    }
    Ok(paths)
}

/// Fail unless rustc's selection is still the one the compiler observed while it searched.
pub(super) fn verify_observed(inputs: &[NativeLibraryInput], searches: &[NativeLibrarySearch]) -> RailResult<()> {
    let observed = inputs.len() == searches.len()
        && inputs.iter().zip(searches).all(|(input, search)| {
            let selected = search.probes.iter().position(|probe| probe.present);
            input.states.len() == selected.map_or(search.probes.len(), |index| index + 1)
                && input
                    .states
                    .iter()
                    .zip(&search.probes)
                    .all(|(state, probe)| matches!(state, NativeLibraryCandidateState::Present(_)) == probe.present)
        });
    if !observed {
        return Err(RailError::message(
            "native library candidates changed while the compiler searched them",
        ));
    }
    Ok(())
}

/// Recapture every input and fail if any candidate changed since the action was captured.
pub(super) fn revalidate(inputs: &[NativeLibraryInput], directories: &[PathBuf]) -> RailResult<u64> {
    let selectors = inputs.iter().map(|input| input.selector.clone()).collect::<Vec<_>>();
    let (current, bytes_hashed) = capture(&selectors, directories)?;
    if current != inputs {
        return Err(RailError::message(
            "a native library candidate changed before the restore commit",
        ));
    }
    Ok(bytes_hashed)
}
