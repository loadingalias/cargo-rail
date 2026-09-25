//! Stable migration diagnostics for commands that earlier releases removed.
//!
//! Clap alone reports an unrecognized subcommand. A user following an older guide needs the
//! release that removed the command and what to run instead, or a clear statement that no
//! replacement exists.

use std::ffi::OsString;

use crate::error::RailError;

/// Global options that take a separate value.
const GLOBAL_OPTIONS_WITH_VALUES: &[&str] = &["--config", "--workspace-root", "--diagnostics-file"];

struct RemovedCommand {
    command: &'static [&'static str],
    removed_in: &'static str,
    instead: &'static str,
}

const REMOVED: &[RemovedCommand] = &[
    RemovedCommand {
        command: &["run"],
        removed_in: "0.22.0",
        instead: "run the planner's Cargo arguments with Cargo, cargo-nextest, or Just directly; \
                  `cargo rail plan --explain` shows them and `cargo rail plan --json` saves them",
    },
    RemovedCommand {
        command: &["hash"],
        removed_in: "0.24.0",
        instead: "read `identity` from `cargo rail plan --json`; it compares decisions and is not a cache key",
    },
    RemovedCommand {
        command: &["diff-hash"],
        removed_in: "0.24.0",
        instead: "compare the `inputs` and `changes` of two saved plans, \
                  or run `cargo rail plan --explain-work WORK_ID` for one decision",
    },
    RemovedCommand {
        command: &["graph"],
        removed_in: "0.24.0",
        instead: "there is no direct replacement; `cargo rail plan --explain` shows each decision's inputs",
    },
    RemovedCommand {
        command: &["release", "finalize"],
        removed_in: "0.26.0",
        instead: "`cargo rail release run` completes a release in one transaction; \
                  `cargo rail release resume` continues an interrupted one",
    },
];

/// Return the migration diagnostic for a removed command in `arguments`, if any.
///
/// `arguments` excludes the program name and a leading `rail`.
pub fn removed_command(arguments: &[OsString]) -> Option<RailError> {
    let mut words = Vec::with_capacity(2);
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str()?;
        if argument == "--" {
            break;
        }
        if argument.starts_with('-') {
            if GLOBAL_OPTIONS_WITH_VALUES.contains(&argument) {
                arguments.next();
            }
            continue;
        }
        words.push(argument);
        if words.len() == 2 {
            break;
        }
    }
    REMOVED
        .iter()
        .find(|removed| words.starts_with(removed.command))
        .map(|removed| {
            RailError::with_help(
                format!(
                    "`cargo rail {}` was removed in Cargo-Rail {}",
                    removed.command.join(" "),
                    removed.removed_in
                ),
                removed.instead,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn removed_commands_are_found_after_global_options_only() {
        let message = |words: &[&str]| removed_command(&arguments(words)).map(|error| error.to_string());
        assert_eq!(
            message(&["--config", "run", "run"]).as_deref(),
            Some("`cargo rail run` was removed in Cargo-Rail 0.22.0"),
            "`run` after --config is the option's value"
        );
        assert_eq!(
            message(&["-q", "release", "finalize", "--json"]).as_deref(),
            Some("`cargo rail release finalize` was removed in Cargo-Rail 0.26.0")
        );
        assert_eq!(message(&["release", "run"]), None);
        assert_eq!(message(&["plan", "run"]), None, "only the command position matters");
        assert_eq!(message(&["graphs"]), None);
        let help = removed_command(&arguments(&["graph"]))
            .and_then(|error| error.help_message())
            .expect("graph help");
        assert!(help.starts_with("there is no direct replacement"), "{help}");
    }
}
