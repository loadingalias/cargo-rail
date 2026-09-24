//! Run `cargo metadata` and report a failure through one safe primary cause.
//!
//! Every failure names a cause, one recovery, and the exact command that reproduces it.
//! Cargo forwards credential-provider output through its stderr, so Cargo's own text
//! appears only when no credential capability is active.

use crate::cargo::resolution::CargoConfigSnapshot;
use crate::error::{RailError, RailResult};
use cargo_metadata::{Metadata, MetadataCommand};
use std::path::{Path, PathBuf};

/// Cargo output retained in a failure report.
const MAX_CARGO_OUTPUT_BYTES: usize = 8 * 1024;

/// Whether Cargo's stderr may appear in a failure report.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CargoOutput<'a> {
    /// The captured Cargo configuration has no credential capability.
    Shown,
    /// The captured Cargo configuration has a credential capability.
    Withheld,
    /// Decide from the Cargo configuration visible from this directory, only after a failure.
    DiscoverFrom(&'a Path),
}

impl CargoOutput<'_> {
    pub(crate) fn for_credential_capability(active: bool) -> Self {
        if active { Self::Withheld } else { Self::Shown }
    }

    fn is_shown(self) -> bool {
        match self {
            Self::Shown => true,
            Self::Withheld => false,
            // Unreadable configuration may still name a provider.
            Self::DiscoverFrom(directory) => {
                CargoConfigSnapshot::capture(directory).is_ok_and(|config| !config.has_credential_capability())
            }
        }
    }
}

/// Run `command` for the Cargo workspace rooted at `workspace_root`.
pub(crate) fn exec(command: &MetadataCommand, workspace_root: &Path, output: CargoOutput<'_>) -> RailResult<Metadata> {
    command
        .exec()
        .map_err(|error| failure(command, workspace_root, output, error))
}

/// A failure class that Cargo-Rail can name without quoting Cargo's output.
#[derive(Debug, PartialEq, Eq)]
enum Cause {
    StaleLockfile,
    Manifest(String),
    Target(String),
    HostRustc,
    CargoUnavailable(String),
    Unclassified,
}

impl Cause {
    /// Cargo loads every workspace manifest before it contacts a registry, so no credential
    /// provider has run and Cargo's output describes only local files.
    fn precedes_registry_access(&self) -> bool {
        matches!(self, Self::Manifest(_))
    }

    fn message(&self) -> String {
        match self {
            Self::StaleLockfile => "Cargo.lock is out of date with the workspace manifests".to_string(),
            Self::Manifest(manifest) => format!("Cargo cannot load manifest `{manifest}`"),
            Self::Target(target) => format!("Cargo cannot query target `{target}` with the selected Rust toolchain"),
            Self::HostRustc => "Cargo cannot run rustc to learn about the host target".to_string(),
            Self::CargoUnavailable(program) => format!("Cargo executable `{program}` was not found"),
            Self::Unclassified => "Cargo metadata failed".to_string(),
        }
    }

    fn recovery(&self) -> String {
        match self {
            Self::StaleLockfile => {
                "update only workspace entries with `cargo update --workspace`, review the Cargo.lock change, and commit it"
                    .to_string()
            }
            Self::Manifest(_) => "fix that manifest; the reproduction command prints Cargo's complete cause".to_string(),
            Self::Target(target) => {
                format!("install the target with `rustup target add {target}`, or correct the target name")
            }
            Self::HostRustc => "check the selected toolchain and any RUSTC or build.rustc setting".to_string(),
            Self::CargoUnavailable(_) => "install the Rust toolchain, or point CARGO at an installed Cargo".to_string(),
            Self::Unclassified => "run the reproduction command to see Cargo's complete cause".to_string(),
        }
    }
}

fn failure(
    command: &MetadataCommand,
    workspace_root: &Path,
    output: CargoOutput<'_>,
    error: cargo_metadata::Error,
) -> RailError {
    let invocation = command.cargo_command();
    let cargo_directory = invocation.get_current_dir().map(Path::to_path_buf);
    let (cause, cargo_text) = match &error {
        cargo_metadata::Error::CargoMetadata { stderr } => (
            classify(stderr, &invocation, cargo_directory.as_deref(), workspace_root),
            Some(stderr.as_str()),
        ),
        cargo_metadata::Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => (
            Cause::CargoUnavailable(invocation.get_program().to_string_lossy().into_owned()),
            None,
        ),
        _ => (Cause::Unclassified, None),
    };

    let mut message = cause.message();
    let mut help = format!("{}\nreproduce: {}", cause.recovery(), reproduction(&invocation));
    match cargo_text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) if cause.precedes_registry_access() || output.is_shown() => {
            message.push_str("\nCargo reported:\n");
            message.push_str(bounded(text));
        }
        Some(_) => help.push_str("\nCargo's output is withheld because a Cargo credential capability is active"),
        // Parse and I/O failures carry no Cargo output, so their own text is safe.
        None if cause == Cause::Unclassified => {
            message.push_str(": ");
            message.push_str(&error.to_string());
        }
        None => {}
    }
    RailError::with_help(message, help)
}

fn classify(
    stderr: &str,
    invocation: &std::process::Command,
    cargo_directory: Option<&Path>,
    workspace_root: &Path,
) -> Cause {
    let is_error = |line: &str| line.starts_with("error: ");
    if stderr.lines().any(|line| {
        is_error(line) && line.contains("lock file") && (line.contains("--locked") || line.contains("--frozen"))
    }) {
        return Cause::StaleLockfile;
    }
    if let Some(manifest) = failed_manifest(stderr, cargo_directory) {
        return Cause::Manifest(display_path(&manifest, workspace_root));
    }
    if stderr.contains("to learn about target-specific information")
        || stderr.contains("error[E0463]")
        || stderr.contains("can't find crate")
        || stderr.contains("target may not be installed")
    {
        return target_filter(invocation).map_or(Cause::HostRustc, Cause::Target);
    }
    Cause::Unclassified
}

/// The most specific manifest Cargo reports as unloadable, with its location when given.
fn failed_manifest(stderr: &str, cargo_directory: Option<&Path>) -> Option<PathBuf> {
    let quoted_after = |marker: &str| {
        stderr.lines().find_map(|line| {
            let (_, rest) = line.split_once(marker)?;
            rest.split_once('`').map(|(path, _)| PathBuf::from(path))
        })
    };
    if let Some(manifest) = quoted_after("failed to parse manifest at `") {
        return Some(manifest);
    }
    // TOML syntax errors name only a location relative to Cargo's working directory.
    let located = stderr.lines().find_map(|line| {
        let location = line.trim_start().strip_prefix("--> ")?;
        let manifest_end = location.find("Cargo.toml")? + "Cargo.toml".len();
        let (manifest, position) = location.split_at_checked(manifest_end)?;
        let manifest = Path::new(manifest);
        let manifest = match cargo_directory {
            Some(directory) if manifest.is_relative() => directory.join(manifest),
            _ => manifest.to_path_buf(),
        };
        let mut shown = manifest.into_os_string();
        shown.push(position);
        Some(PathBuf::from(shown))
    });
    located.or_else(|| {
        quoted_after("failed to load manifest for workspace member `").map(|member| member.join("Cargo.toml"))
    })
}

/// A workspace-relative path when the path lies inside the workspace.
fn display_path(path: &Path, workspace_root: &Path) -> String {
    let normalized = normalize_lexically(path);
    let canonical_root = crate::utils::canonicalize_existing(workspace_root).ok();
    let canonical_path = canonical_existing_prefix(&normalized);
    [workspace_root, canonical_root.as_deref().unwrap_or(workspace_root)]
        .into_iter()
        .find_map(|root| {
            normalized
                .strip_prefix(root)
                .ok()
                .or_else(|| canonical_path.as_deref()?.strip_prefix(root).ok())
        })
        .map_or_else(|| normalized.display().to_string(), crate::utils::path_to_git_format)
}

/// Canonicalize the longest existing ancestor so a trailing `:line:column` suffix survives.
fn canonical_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut suffix = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = crate::utils::canonicalize_existing(current) {
            return Some(suffix.into_iter().rev().fold(canonical, |path, part| path.join(part)));
        }
        suffix.push(current.file_name()?.to_os_string());
        current = current.parent()?;
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if normalized.file_name().is_some() => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

fn target_filter(invocation: &std::process::Command) -> Option<String> {
    let mut arguments = invocation.get_args();
    while let Some(argument) = arguments.next() {
        if argument == "--filter-platform" {
            return arguments.next().map(|target| target.to_string_lossy().into_owned());
        }
    }
    None
}

/// The exact Cargo command line, run from Cargo-Rail's working directory for Cargo.
fn reproduction(invocation: &std::process::Command) -> String {
    let command = std::iter::once(invocation.get_program())
        .chain(invocation.get_args())
        .map(|part| crate::utils::shell_quote(&part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    match invocation.get_current_dir() {
        Some(directory) => format!(
            "cd {} && {command}",
            crate::utils::shell_quote(&directory.to_string_lossy())
        ),
        None => command,
    }
}

fn bounded(text: &str) -> &str {
    let end = (0..=MAX_CARGO_OUTPUT_BYTES.min(text.len()))
        .rev()
        .find(|&end| text.is_char_boundary(end))
        .unwrap_or_default();
    text.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE: &str = "error: cannot update the lock file /work/Cargo.lock because --locked was passed to prevent this\n\
        help: to generate the lock file without accessing the network, remove the --locked flag and use --offline instead.\n";
    const INHERITED: &str = "error: failed to load manifest for workspace member `/work/crates/member`\n\
        referenced by workspace at `/work/Cargo.toml`\n\nCaused by:\n  failed to parse manifest at `/work/crates/member/Cargo.toml`\n\n\
        Caused by:\n  error inheriting `absent` from workspace root manifest's `workspace.dependencies.absent`\n";
    const SYNTAX: &str = "error: unclosed table, expected `]`\n --> crates/member/Cargo.toml:1:9\n  |\n1 | [package\n  |         ^\n\
        error: failed to load manifest for workspace member `/work/crates/member`\n";
    const MISSING_MEMBER: &str = "error: failed to load manifest for workspace member `/work/gone`\n\
        referenced by workspace at `/work/Cargo.toml`\n\nCaused by:\n  failed to read `/work/gone/Cargo.toml`\n";
    const TARGET: &str = "error: failed to run `rustc` to learn about target-specific information\n\nCaused by:\n  process didn't exit successfully\n";

    fn classified(stderr: &str, arguments: &[&str]) -> Cause {
        let mut invocation = std::process::Command::new("cargo");
        invocation.args(arguments);
        classify(stderr, &invocation, Some(Path::new("/work")), Path::new("/work"))
    }

    #[test]
    fn cargo_failures_reduce_to_one_workspace_relative_cause() {
        assert_eq!(classified(STALE, &[]), Cause::StaleLockfile);
        assert_eq!(
            classified(INHERITED, &[]),
            Cause::Manifest("crates/member/Cargo.toml".to_string())
        );
        assert_eq!(
            classified(SYNTAX, &[]),
            Cause::Manifest("crates/member/Cargo.toml:1:9".to_string())
        );
        assert_eq!(
            classified(MISSING_MEMBER, &[]),
            Cause::Manifest("gone/Cargo.toml".to_string())
        );
        assert_eq!(
            classified(TARGET, &["--filter-platform", "wasm32-unknown-unknown"]),
            Cause::Target("wasm32-unknown-unknown".to_string())
        );
        assert_eq!(classified(TARGET, &[]), Cause::HostRustc);
        assert_eq!(
            classified("error: registry index was unreachable\n", &[]),
            Cause::Unclassified
        );
    }

    #[test]
    fn withheld_output_keeps_the_cause_and_exact_reproduction_only() {
        let mut command = MetadataCommand::new();
        command
            .cargo_path("cargo")
            .current_dir("/work dir")
            .manifest_path("/work dir/Cargo.toml")
            .other_options(vec!["--locked".to_string()]);
        let stderr = format!("{STALE}provider said: secret-token-value\n");
        let error = failure(
            &command,
            Path::new("/work dir"),
            CargoOutput::Withheld,
            cargo_metadata::Error::CargoMetadata { stderr },
        );
        let help = error.help_message().expect("failure carries recovery");
        assert_eq!(
            error.to_string(),
            "Cargo.lock is out of date with the workspace manifests"
        );
        assert!(help.contains("cargo update --workspace"), "{help}");
        assert!(
            help.contains("reproduce: cd '/work dir' && cargo metadata --format-version 1 --manifest-path '/work dir/Cargo.toml' --locked"),
            "{help}"
        );
        assert!(!format!("{error}\n{help}").contains("secret-token-value"));

        // Manifest loading ends before any registry access, so its local cause stays visible.
        let manifest = failure(
            &command,
            Path::new("/work dir"),
            CargoOutput::Withheld,
            cargo_metadata::Error::CargoMetadata {
                stderr: INHERITED.to_string(),
            },
        );
        assert!(
            manifest
                .to_string()
                .contains("Cargo reported:\nerror: failed to load manifest"),
            "{manifest}"
        );
        let unclassified = failure(
            &command,
            Path::new("/work dir"),
            CargoOutput::Withheld,
            cargo_metadata::Error::CargoMetadata {
                stderr: "error: failed to query registry\nprovider said: secret-token-value\n".to_string(),
            },
        );
        assert_eq!(unclassified.to_string(), "Cargo metadata failed");
        let shown = failure(
            &command,
            Path::new("/work dir"),
            CargoOutput::Shown,
            cargo_metadata::Error::CargoMetadata {
                stderr: "error: failed to query registry\n".to_string(),
            },
        );
        assert!(
            shown
                .to_string()
                .ends_with("Cargo reported:\nerror: failed to query registry"),
            "{shown}"
        );
    }

    #[test]
    fn retained_cargo_output_is_bounded_on_a_character_boundary() {
        let text = "é".repeat(MAX_CARGO_OUTPUT_BYTES);
        let retained = bounded(&text);
        assert!(retained.len() <= MAX_CARGO_OUTPUT_BYTES);
        assert!(text.starts_with(retained));
    }
}
