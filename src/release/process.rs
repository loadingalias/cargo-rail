//! Shared subprocess helpers for release flows.
//!
//! Centralizes process execution for cargo/gh/rustup checks and actions so
//! release code paths behave consistently.

use crate::error::{RailError, RailResult};
use std::path::Path;
use std::process::{Command, Output};

fn format_command(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{} {}", program, args.join(" "))
    }
}

/// Run a command and map spawn failures into `RailError`.
pub(crate) fn run(program: &str, args: &[&str], current_dir: Option<&Path>) -> RailResult<Output> {
    let mut cmd = Command::new(program);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.args(args)
        .output()
        .map_err(|e| RailError::message(format!("Failed to run {}: {}", format_command(program, args), e)))
}

/// Run a command that executes repository or dependency code without release credentials.
///
/// Build scripts and procedural macros inherit the environment; publication and forge
/// tokens belong only to the steps that publish.
pub(crate) fn run_without_release_credentials(
    program: &str,
    args: &[&str],
    current_dir: Option<&Path>,
) -> RailResult<Output> {
    let mut cmd = Command::new(program);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    remove_release_credentials(&mut cmd);
    cmd.args(args)
        .output()
        .map_err(|e| RailError::message(format!("Failed to run {}: {}", format_command(program, args), e)))
}

/// Registry and forge credentials that release steps may hold.
const RELEASE_CREDENTIALS: &[&str] = &[
    "CARGO_RAIL_RELEASE_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
];

/// Remove registry and forge credentials from a child's environment.
pub(crate) fn remove_release_credentials(command: &mut Command) {
    for name in RELEASE_CREDENTIALS {
        command.env_remove(name);
    }
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN") {
            command.env_remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_running_children_lose_registry_and_forge_credentials() {
        let mut command = Command::new("cargo");
        remove_release_credentials(&mut command);
        let removed = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for name in [
            "CARGO_REGISTRY_TOKEN",
            "CARGO_RAIL_RELEASE_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
        ] {
            assert!(
                removed.iter().any(|removed| removed == name),
                "{name} reaches the child"
            );
        }
    }
}
