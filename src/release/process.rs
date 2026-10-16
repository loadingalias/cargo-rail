//! Shared subprocess helpers for release flows.
//!
//! Centralizes process execution for cargo/gh/rustup checks and actions so
//! release code paths behave consistently.

use crate::error::{RailError, RailResult};
use std::ffi::OsString;
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
///
/// Release runners pass the push credential as process-scoped Git configuration
/// (`GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_n` and `GIT_CONFIG_VALUE_n`, or `GIT_CONFIG_PARAMETERS`).
const RELEASE_CREDENTIALS: &[&str] = &[
    "CARGO_RAIL_RELEASE_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

/// Remove registry and forge credentials from a child's environment.
///
/// The child can run build scripts and procedural macros, which can read every variable it inherits.
pub(crate) fn remove_release_credentials(command: &mut Command) {
    for name in RELEASE_CREDENTIALS {
        command.env_remove(name);
    }
    for name in inherited_credentials(std::env::vars_os().map(|(name, _)| name)) {
        command.env_remove(name);
    }
}

/// The variable names in `environment` that hold a registry token or process-scoped Git configuration.
fn inherited_credentials(environment: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    environment
        .into_iter()
        .filter(|name| {
            let name = name.to_string_lossy();
            (name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN"))
                || name.starts_with("GIT_CONFIG_KEY_")
                || name.starts_with("GIT_CONFIG_VALUE_")
        })
        .collect()
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
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ] {
            assert!(
                removed.iter().any(|removed| removed == name),
                "{name} reaches the child"
            );
        }
    }

    #[test]
    fn inherited_registry_tokens_and_git_configuration_are_credentials() {
        let environment = [
            "CARGO_REGISTRIES_FIXTURE_TOKEN",
            "CARGO_REGISTRIES_FIXTURE_INDEX",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_VALUE_12",
            "GIT_DIR",
            "PATH",
        ]
        .map(OsString::from);
        assert_eq!(
            inherited_credentials(environment),
            [
                "CARGO_REGISTRIES_FIXTURE_TOKEN",
                "GIT_CONFIG_KEY_0",
                "GIT_CONFIG_VALUE_0",
                "GIT_CONFIG_VALUE_12"
            ]
            .map(OsString::from)
        );
    }
}
