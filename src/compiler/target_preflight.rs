//! Prove cheaply, before compiler acquisition, that selected targets can run on this host.
//!
//! Checks cover only facts that hold for every build of the selected work: an installed target
//! library for any compilation, and a linked probe with the configured linker for work that
//! links. Everything else, such as build-script prerequisites, stays with Cargo.

use crate::cargo::{TargetIdentity, TargetSpecificationIdentity};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceSnapshot;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PROBE_STDERR_BYTES: usize = 4096;
const MAX_LIBRARY_ENTRIES: usize = 4096;

/// What the selected work needs from one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TargetUse {
    /// Compile without linking, such as `cargo check`.
    Check,
    /// Compile and link a standard-library binary, such as a doctest.
    Link,
}

/// One target proven ready for its selected use.
#[derive(Debug)]
pub(crate) struct TargetReadiness {
    pub(crate) target: String,
    /// The target library directory in the selected sysroot.
    pub(crate) standard_library: PathBuf,
    pub(crate) linker: Option<OsString>,
    pub(crate) linked_probe: bool,
}

/// One target that cannot run its selected use on this host.
#[derive(Debug)]
pub(crate) struct TargetFailure {
    pub(crate) target: String,
    pub(crate) cause: String,
}

/// Host facts captured from one workspace snapshot.
#[derive(Debug, Clone)]
pub(crate) struct TargetPreflight {
    rustc: PathBuf,
    sysroot: PathBuf,
    host: String,
    cargo_current_dir: PathBuf,
    targets: Vec<TargetIdentity>,
}

impl TargetPreflight {
    pub(crate) fn capture(snapshot: &WorkspaceSnapshot) -> Self {
        let sysroot = snapshot.toolchain().direct_rustc_sysroot().to_path_buf();
        Self {
            rustc: sysroot
                .join("bin")
                .join(if cfg!(windows) { "rustc.exe" } else { "rustc" }),
            sysroot,
            host: snapshot.toolchain().host_target().to_string(),
            cargo_current_dir: snapshot.cargo_current_dir().to_path_buf(),
            targets: snapshot.targets().to_vec(),
        }
    }

    /// Check each selected target, where `default` names the host.
    pub(crate) fn check(&self, selected: &[(&str, TargetUse)]) -> Result<Vec<TargetReadiness>, Vec<TargetFailure>> {
        let mut ready = Vec::with_capacity(selected.len());
        let mut failures = Vec::new();
        for (selected, use_) in selected {
            let name = if *selected == "default" {
                self.host.as_str()
            } else {
                selected
            };
            match self.check_one(name, *use_) {
                Ok(readiness) => ready.push(readiness),
                Err(cause) => failures.push(TargetFailure {
                    target: name.to_string(),
                    cause,
                }),
            }
        }
        if failures.is_empty() { Ok(ready) } else { Err(failures) }
    }

    fn check_one(&self, name: &str, use_: TargetUse) -> Result<TargetReadiness, String> {
        let target = self
            .targets
            .iter()
            .find(|target| target_name(target) == name)
            .ok_or_else(|| "target is absent from the captured Cargo configuration".to_string())?;
        let standard_library = self.sysroot.join("lib").join("rustlib").join(name).join("lib");
        let linked_probe = use_ == TargetUse::Link;
        // A check-only custom specification may build its libraries with `build-std`.
        let built_in = matches!(target.specification(), TargetSpecificationIdentity::BuiltIn(_));
        if built_in || linked_probe {
            validate_target_library(&standard_library)?;
        }
        if linked_probe {
            self.linked_probe(target)?;
        }
        Ok(TargetReadiness {
            target: name.to_string(),
            standard_library,
            linker: target.linker().map(OsStr::to_os_string),
            linked_probe,
        })
    }

    /// Link one standard-library binary; only this check needs a scratch directory.
    fn linked_probe(&self, target: &TargetIdentity) -> Result<(), String> {
        let directory = tempfile::Builder::new()
            .prefix("cargo-rail-target-preflight-")
            .tempdir()
            .map_err(|error| format!("cannot create a link probe directory: {error}"))?;
        let source = directory.path().join("main.rs");
        fs::write(&source, b"fn main() {}\n").map_err(|error| format!("cannot write the link probe: {error}"))?;
        let mut command = Command::new(&self.rustc);
        command
            .current_dir(&self.cargo_current_dir)
            .arg(&source)
            .args([
                "--crate-name",
                "cargo_rail_target_preflight",
                "--edition",
                "2024",
                "--target",
            ])
            .arg(target_argument(target))
            .arg("-o")
            .arg(directory.path().join("probe"))
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("RUSTUP_NO_UPDATE_CHECK", "1");
        if let Some(linker) = target.linker() {
            let mut value = OsString::from("linker=");
            value.push(linker);
            command.arg("-C").arg(value);
        }
        match crate::compiler::acquisition::process::run_bounded_process(
            &mut command,
            PROBE_TIMEOUT,
            0,
            MAX_PROBE_STDERR_BYTES,
        ) {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(format!(
                "linked compiler probe failed with status {}: {}",
                output.status,
                bounded_stderr(&output.stderr)
            )),
            Err(error) => Err(format!("failed to start the selected rustc: {error}")),
        }
    }
}

/// Render failures as one bounded list for a command-specific error.
pub(crate) fn render_failures(failures: &[TargetFailure]) -> String {
    const MAX_FAILURES: usize = 16;
    failures
        .iter()
        .take(MAX_FAILURES)
        .map(|failure| format!("- {}: {}", failure.target, failure.cause))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_target_library(directory: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        format!(
            "Rust target library '{}' is not installed: {error}",
            directory.display()
        )
    })?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(format!(
            "Rust target library path '{}' is not a real directory",
            directory.display()
        ));
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read Rust target library '{}': {error}", directory.display()))?;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_LIBRARY_ENTRIES {
            return Err("Rust target library inventory exceeds its bound".to_string());
        }
        let name = entry
            .map_err(|error| format!("cannot read Rust target library entry: {error}"))?
            .file_name();
        let name = name.to_string_lossy();
        if name.starts_with("libcore-") && name.ends_with(".rlib") {
            return Ok(());
        }
    }
    Err(format!(
        "Rust target library '{}' has no libcore rlib",
        directory.display()
    ))
}

pub(crate) fn target_name(target: &TargetIdentity) -> &str {
    match target.specification() {
        TargetSpecificationIdentity::BuiltIn(name) => name,
        TargetSpecificationIdentity::Custom(specification) => specification.name(),
    }
}

pub(crate) fn target_argument(target: &TargetIdentity) -> &OsStr {
    match target.specification() {
        TargetSpecificationIdentity::BuiltIn(name) => OsStr::new(name),
        TargetSpecificationIdentity::Custom(specification) => specification.path().as_os_str(),
    }
}

fn bounded_stderr(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(MAX_PROBE_STDERR_BYTES);
    String::from_utf8_lossy(stderr.get(start..).unwrap_or_default())
        .trim()
        .to_string()
}

/// Reject when any failure exists, naming each target and the caller's recovery.
pub(crate) fn require_ready(
    result: Result<Vec<TargetReadiness>, Vec<TargetFailure>>,
    subject: &str,
    recovery: &str,
) -> RailResult<Vec<TargetReadiness>> {
    result.map_err(|failures| {
        RailError::with_help(
            format!(
                "{subject} target preflight failed before compiler acquisition:\n{}",
                render_failures(&failures)
            ),
            recovery,
        )
    })
}
