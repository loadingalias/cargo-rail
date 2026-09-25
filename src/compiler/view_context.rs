//! Private invocation context for one compiler-evidence view.
//!
//! Cargo passes its whole environment to every build script it runs. A view therefore gives
//! Cargo only standard variables and runs staged wrappers that sit beside one private context
//! file, written for that view and removed after it. A wrapper recovers its role and session from
//! its own staged path; a native cache worker recovers them from the staged workspace wrapper
//! that Cargo passes as its first argument.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::compiler::invocation::{RUSTDOC_WRAPPER_MARKER, WRAPPER_MARKER};
use crate::error::{RailError, RailResult};

const CONTEXT_FILE: &str = "cargo-rail-view-context.json";
const CONTEXT_VERSION: u32 = 1;
const MAX_CONTEXT_BYTES: u64 = 64 * 1024;
const RUSTC_WRAPPER_STEM: &str = "cargo-rail-rustc-observer";
const RUSTDOC_WRAPPER_STEM: &str = "cargo-rail-rustdoc-observer";

static ACTIVE: OnceLock<BTreeMap<String, OsString>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextRecord {
    version: u32,
    /// Private variable names mapped to platform-encoded values in lowercase hex.
    values: BTreeMap<String, String>,
}

/// Staged wrapper paths beside one view-context location.
pub(crate) struct StagedWrappers {
    directory: PathBuf,
    pub(crate) rustc: PathBuf,
    pub(crate) rustdoc: PathBuf,
}

/// Stage the observation wrappers in `directory`, where view contexts will be written.
///
/// Cargo hashes the workspace-wrapper path into each workspace unit's `-C metadata`, so a
/// directory that is stable across commands keeps compiled units and native results reusable.
pub(crate) fn stage(wrapper: &Path, directory: &Path) -> RailResult<StagedWrappers> {
    Ok(StagedWrappers {
        directory: directory.to_path_buf(),
        rustc: stage_wrapper(wrapper, directory, RUSTC_WRAPPER_STEM)?,
        rustdoc: stage_wrapper(wrapper, directory, RUSTDOC_WRAPPER_STEM)?,
    })
}

/// One view's private context, removed when the view's Cargo process has finished.
pub(crate) struct ViewContext {
    path: PathBuf,
}

impl StagedWrappers {
    /// Write the context for the one view that runs these wrappers.
    ///
    /// Creation is exclusive: a second concurrent view in the same directory fails closed.
    pub(crate) fn begin_view(&self, values: &BTreeMap<&str, &OsStr>) -> RailResult<ViewContext> {
        let record = ContextRecord {
            version: CONTEXT_VERSION,
            values: values
                .iter()
                .map(|(name, value)| ((*name).to_string(), encode(value)))
                .collect(),
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let path = self.directory.join(CONTEXT_FILE);
        let mut file = options.open(&path).map_err(|error| {
            RailError::message(format!("creating compiler view context '{}': {error}", path.display()))
        })?;
        let context = ViewContext { path };
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        Ok(context)
    }
}

impl Drop for ViewContext {
    fn drop(&mut self) {
        drop(fs::remove_file(&self.path));
    }
}

fn stage_wrapper(wrapper: &Path, directory: &Path, stem: &str) -> RailResult<PathBuf> {
    let staged = directory.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
    fs::hard_link(wrapper, &staged)
        .or_else(|_| fs::copy(wrapper, &staged).map(|_| ()))
        .map_err(|error| {
            RailError::message(format!(
                "staging compiler-observation wrapper '{}' as '{}': {error}",
                wrapper.display(),
                staged.display()
            ))
        })?;
    Ok(staged)
}

/// Recover this process's view context from its staged path or its first argument.
///
/// Call once before reading any private variable. A process outside a view, or one whose
/// Cargo-Rail parent passed an explicit session, keeps its environment.
pub(crate) fn activate() -> RailResult<()> {
    // A Cargo-Rail parent passes an explicit session to its own children; Cargo never does.
    if std::env::var_os(crate::compiler::session::FACT_SESSION_ENV).is_some() {
        return Ok(());
    }
    let arguments = std::env::args_os().take(2).map(PathBuf::from).collect::<Vec<_>>();
    for candidate in &arguments {
        if let Some(context) = load(candidate)? {
            drop(ACTIVE.set(context));
            break;
        }
    }
    Ok(())
}

/// Read one private invocation variable from the active view context, else the environment.
pub(crate) fn var_os(name: &str) -> Option<OsString> {
    ACTIVE
        .get()
        .map_or_else(|| std::env::var_os(name), |context| context.get(name).cloned())
}

fn load(candidate: &Path) -> RailResult<Option<BTreeMap<String, OsString>>> {
    let Some(stem) = candidate.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    let stem = stem.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(stem);
    let marker = match stem {
        RUSTC_WRAPPER_STEM => WRAPPER_MARKER,
        RUSTDOC_WRAPPER_STEM => RUSTDOC_WRAPPER_MARKER,
        _ => return Ok(None),
    };
    let Some(directory) = candidate.parent().filter(|directory| !directory.as_os_str().is_empty()) else {
        return Ok(None);
    };
    let path = directory.join(CONTEXT_FILE);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(None);
    };
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() > MAX_CONTEXT_BYTES
        || !crate::compiler::session::private_mode(&metadata)
    {
        return Err(RailError::message(
            "compiler view context is not a bounded private regular file",
        ));
    }
    let mut file = fs::File::open(&path)?;
    if !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
        return Err(RailError::message("compiler view context changed before it was opened"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(MAX_CONTEXT_BYTES + 1).read_to_end(&mut bytes)?;
    let record: ContextRecord = serde_json::from_slice(&bytes)?;
    if record.version != CONTEXT_VERSION {
        return Err(RailError::message("compiler view context has an unsupported version"));
    }
    let mut values = record
        .values
        .into_iter()
        .map(|(name, value)| decode(&value).map(|value| (name, value)))
        .collect::<RailResult<BTreeMap<_, _>>>()?;
    values.insert(marker.to_string(), OsString::from("1"));
    Ok(Some(values))
}

#[cfg(unix)]
fn encode(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(windows)]
fn encode(value: &OsStr) -> String {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().map(|unit| format!("{unit:04x}")).collect()
}

#[cfg(unix)]
fn decode(value: &str) -> RailResult<OsString> {
    use std::os::unix::ffi::OsStringExt as _;
    hex_units(value, 2)?
        .into_iter()
        .map(|unit| u8::try_from(unit).map_err(|_| RailError::message("compiler view context byte is out of range")))
        .collect::<RailResult<Vec<_>>>()
        .map(OsString::from_vec)
}

#[cfg(windows)]
fn decode(value: &str) -> RailResult<OsString> {
    use std::os::windows::ffi::OsStringExt as _;
    hex_units(value, 4).map(|units| OsString::from_wide(&units))
}

fn hex_units(value: &str, width: usize) -> RailResult<Vec<u16>> {
    if !value.len().is_multiple_of(width) || !value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(RailError::message("compiler view context value is not canonical hex"));
    }
    value
        .as_bytes()
        .chunks(width)
        .map(|chunk| {
            std::str::from_utf8(chunk)
                .ok()
                .and_then(|chunk| u16::from_str_radix(chunk, 16).ok())
                .ok_or_else(|| RailError::message("compiler view context value is not canonical hex"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_wrappers_recover_their_role_and_exact_values() {
        let directory = tempfile::tempdir().expect("view directory");
        let wrapper = directory.path().join("source-wrapper");
        fs::write(&wrapper, b"wrapper").expect("wrapper bytes");
        let source_root = OsString::from("/repository with spaces/ünïcode");
        let staged = stage(&wrapper, directory.path()).expect("staged wrappers");
        let view = staged
            .begin_view(&BTreeMap::from([(
                "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT",
                source_root.as_os_str(),
            )]))
            .expect("view context");
        assert!(
            staged.begin_view(&BTreeMap::new()).is_err(),
            "a concurrent view must not share a context"
        );

        let rustc = load(&staged.rustc)
            .expect("rustc context")
            .expect("staged rustc wrapper");
        assert_eq!(rustc.get(WRAPPER_MARKER), Some(&OsString::from("1")));
        assert!(!rustc.contains_key(RUSTDOC_WRAPPER_MARKER));
        assert_eq!(
            rustc.get("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT"),
            Some(&source_root)
        );
        let rustdoc = load(&staged.rustdoc)
            .expect("rustdoc context")
            .expect("staged rustdoc wrapper");
        assert_eq!(rustdoc.get(RUSTDOC_WRAPPER_MARKER), Some(&OsString::from("1")));

        assert!(load(&wrapper).expect("unstaged wrapper").is_none());
        assert!(
            load(Path::new("cargo-rail-rustc-observer"))
                .expect("bare name")
                .is_none()
        );
        drop(view);
        assert!(
            load(&staged.rustc).expect("finished view").is_none(),
            "a finished view leaves no context behind"
        );
    }

    #[test]
    fn a_tampered_context_fails_closed() {
        let directory = tempfile::tempdir().expect("view directory");
        let wrapper = directory.path().join("source-wrapper");
        fs::write(&wrapper, b"wrapper").expect("wrapper bytes");
        let staged = stage(&wrapper, directory.path()).expect("staged wrappers");
        let context = directory.path().join(CONTEXT_FILE);
        let write_context = |bytes: &[u8], mode: u32| {
            fs::write(&context, bytes).expect("context bytes");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&context, fs::Permissions::from_mode(mode)).expect("context mode");
            }
            #[cfg(not(unix))]
            let _ = mode;
        };

        write_context(br#"{"version":1,"values":{"NAME":"not hex"}}"#, 0o600);
        load(&staged.rustc).expect_err("a non-hex value must not authorize a view");
        write_context(br#"{"version":2,"values":{}}"#, 0o600);
        load(&staged.rustc).expect_err("an unknown version must not authorize a view");
        #[cfg(unix)]
        {
            write_context(br#"{"version":1,"values":{}}"#, 0o644);
            load(&staged.rustc).expect_err("a shared context must not authorize a view");
        }
    }
}
