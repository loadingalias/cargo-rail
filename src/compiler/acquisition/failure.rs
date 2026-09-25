//! Reduce a failed compiler-evidence Cargo view to one safe cause and one recovery.
//!
//! The message, recovery, and machine output never contain Cargo or build-script output,
//! except one missing tool name or source path parsed from a fixed pattern, bounded to
//! path-safe characters and redacted like the detail. Text mode adds bounded detail after the cause: rendered compiler errors, the last lines
//! of a failing build script's own stderr, and, with `--verbose`, Cargo's stderr unless a
//! Cargo credential capability is configured. Every exact value of an inherited environment
//! variable in that detail is replaced with `<env:NAME>`.

use crate::error::{FailureClass, RailError};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

/// Cargo stderr retained in verbose text output.
const MAX_DETAIL_BYTES: usize = 8 * 1024;
/// Rendered compiler diagnostics retained in text output.
const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;
/// Environment names reported for one build script.
const MAX_ENVIRONMENT_NAMES: usize = 8;
/// Build-script stderr lines retained in text output.
const MAX_BUILD_SCRIPT_LINES: usize = 20;
const MAX_BUILD_SCRIPT_BYTES: usize = 2 * 1024;
/// Longest tool name or path accepted into a message.
const MAX_NAMED_INPUT_BYTES: usize = 128;
/// Shorter environment values are too common to identify an inherited value.
const MIN_REDACTED_VALUE_BYTES: usize = 8;
/// Location variables whose values appear in ordinary paths and are not secrets.
const UNREDACTED_LOCATIONS: &[&str] = &[
    "HOME",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "TMP",
    "TEMP",
    "USERPROFILE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "PATH",
    "SHELL",
];

/// One failed compiler-evidence Cargo view.
pub(crate) struct FailedView<'a> {
    /// Stable view label: package, target, and feature selection.
    pub(crate) label: &'a str,
    /// Target triple, or `default` for the host.
    pub(crate) platform: &'a str,
    /// Exit status as Cargo reported it.
    pub(crate) status: &'a str,
    /// Cargo program and arguments of the view.
    pub(crate) cargo_program: &'a OsStr,
    pub(crate) cargo_arguments: &'a [std::ffi::OsString],
    /// Directory in which Cargo ran.
    pub(crate) workspace_root: &'a Path,
    /// Packages and Cargo targets that emitted compiler errors.
    pub(crate) error_targets: &'a [String],
    /// Retained Cargo JSON messages.
    pub(crate) stdout: &'a [u8],
    /// Retained tail of Cargo's stderr.
    pub(crate) stderr: &'a [u8],
    /// Whether Cargo's own stderr may appear in text output.
    pub(crate) show_cargo_output: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Cause {
    CargoRail(String),
    MissingTargetLibrary,
    MissingLinker(String),
    CompilerProbe,
    BuildScript {
        package: String,
        environment: Vec<String>,
        missing_tool: Option<String>,
    },
    MissingSourceFile(String),
    Source,
    Unclassified,
}

pub(crate) fn classify(view: &FailedView<'_>) -> RailError {
    let stderr = String::from_utf8_lossy(view.stderr);
    let diagnostics = compiler_errors(view.stdout);
    let cause = cause(&stderr, &diagnostics, view.error_targets);
    let reproduce = format!("reproduce with Cargo: {}", reproduction(view));
    let target = match view.platform {
        "default" => "the host target".to_string(),
        platform => format!("target `{platform}`"),
    };
    let without_evidence = "or set `unify.compiler_targets = \"none\"` to run Unify without compiler evidence; \
                            it then retains every dependency whose use it cannot prove";
    let verbose_hint = if !view.show_cargo_output {
        Some("Cargo's output is withheld because a Cargo credential capability is active; the reproduction command shows it")
    } else if !crate::output::is_verbose() {
        Some("rerun with --verbose to see Cargo's output")
    } else {
        None
    }
    .into_iter();
    let cargo_detail = || {
        (crate::output::is_verbose() && view.show_cargo_output)
            .then(|| format!("Cargo reported:\n{}", tail(stderr.trim(), MAX_DETAIL_BYTES)))
    };
    let build_script_detail = || {
        cargo_detail()
            .or_else(|| build_script_stderr(&stderr).map(|excerpt| format!("the build script reported:\n{excerpt}")))
    };
    let view_label = view.label;

    let (class, message, recovery, detail) = match cause {
        Cause::CargoRail(line) => (
            FailureClass::CargoRail,
            format!("Cargo-Rail's compiler adapter failed in compiler evidence view `{view_label}`: {line}"),
            vec![
                "this is a Cargo-Rail defect; report it with the output of the same command run with --verbose"
                    .to_string(),
                without_evidence.to_string(),
            ],
            cargo_detail(),
        ),
        Cause::MissingTargetLibrary => (
            FailureClass::Toolchain,
            format!("the Rust standard library for {target} is not installed (compiler evidence view `{view_label}`)"),
            vec![
                match view.platform {
                    "default" => "repair the selected toolchain installation".to_string(),
                    platform => format!("install it with `rustup target add {platform}`"),
                },
                format!("narrow `unify.compiler_targets` to targets this host can compile, {without_evidence}"),
            ],
            cargo_detail(),
        ),
        Cause::MissingLinker(linker) => (
            FailureClass::Toolchain,
            format!("linker `{linker}` for {target} was not found (compiler evidence view `{view_label}`)"),
            vec![
                "install that linker, or correct the `linker` setting for the target in Cargo configuration"
                    .to_string(),
                format!("narrow `unify.compiler_targets` to targets this host can compile, {without_evidence}"),
            ],
            cargo_detail(),
        ),
        Cause::CompilerProbe => (
            FailureClass::Toolchain,
            format!(
                "Cargo cannot run the configured compiler or rustc wrapper (compiler evidence view `{view_label}`)"
            ),
            std::iter::once(
                "check the selected toolchain and any RUSTC, RUSTC_WRAPPER, or build.rustc-wrapper setting".to_string(),
            )
            .chain(verbose_hint.clone().map(str::to_string))
            .collect(),
            cargo_detail(),
        ),
        Cause::BuildScript {
            package,
            environment,
            missing_tool,
        } => {
            let reads = if environment.is_empty() {
                String::new()
            } else {
                format!(
                    " (the build script declares that it reads {})",
                    environment
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let missing_tool = missing_tool.map(|tool| redact_environment(&tool, std::env::vars_os()));
            (
                FailureClass::BuildScript,
                match &missing_tool {
                    Some(tool) => format!(
                        "the build script of `{package}` cannot run `{tool}`, which is not installed \
                         (compiler evidence view `{view_label}`)"
                    ),
                    None => format!("the build script of `{package}` failed in compiler evidence view `{view_label}`"),
                },
                std::iter::once(match &missing_tool {
                    Some(tool) => format!("install `{tool}`, or point the build script at an installed tool{reads}"),
                    None => format!(
                        "provide the native tools, files, or environment that this build script requires{reads}"
                    ),
                })
                .chain(std::iter::once(match view.platform {
                    // A cross build needs native tools for its target, which this host may lack.
                    "default" => without_evidence.to_string(),
                    _ => format!(
                        "narrow `unify.compiler_targets` to targets this host can build for, {without_evidence}"
                    ),
                }))
                .chain(verbose_hint.clone().map(str::to_string))
                .collect(),
                build_script_detail(),
            )
        }
        Cause::MissingSourceFile(path) => {
            let path = redact_environment(&path, std::env::vars_os());
            (
                FailureClass::Source,
                format!(
                    "{} reads `{path}`, which does not exist (compiler evidence view `{view_label}`)",
                    view.error_targets.join(", ")
                ),
                vec![format!(
                    "create or generate `{path}` before running Unify, the same way your build does; \
                     Unify compiles the checkout as it is"
                )],
                (!diagnostics.is_empty()).then(|| head(&diagnostics.join("\n"), MAX_DIAGNOSTIC_BYTES)),
            )
        }
        Cause::Source => (
            FailureClass::Source,
            format!(
                "{} did not compile in compiler evidence view `{view_label}`",
                view.error_targets.join(", ")
            ),
            vec!["fix the compiler errors; Unify requires every selected view to compile".to_string()],
            (!diagnostics.is_empty()).then(|| head(&diagnostics.join("\n"), MAX_DIAGNOSTIC_BYTES)),
        ),
        Cause::Unclassified => (
            FailureClass::Cargo,
            format!(
                "Cargo failed in compiler evidence view `{view_label}` with {}",
                view.status
            ),
            std::iter::once("run the reproduction command to see Cargo's complete cause".to_string())
                .chain(verbose_hint.map(str::to_string))
                .collect(),
            cargo_detail(),
        ),
    };
    let help = recovery
        .into_iter()
        .chain(std::iter::once(reproduce))
        .collect::<Vec<_>>();
    let detail = detail.map(|detail| redact_environment(&detail, std::env::vars_os()));
    RailError::failure(class, message, help.join("\n"), detail)
}

/// The last lines of the failing build script's own stderr, which Cargo indents by two spaces.
fn build_script_stderr(stderr: &str) -> Option<String> {
    let mut lines = stderr
        .lines()
        .skip_while(|line| !line.starts_with("error: failed to run custom build command for `"))
        .skip_while(|line| line.trim_end() != "  --- stderr")
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with("  "))
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let omitted = lines.len().saturating_sub(MAX_BUILD_SCRIPT_LINES);
    let excerpt = lines.split_off(omitted).join("\n");
    if excerpt.trim().is_empty() {
        return None;
    }
    Some(if omitted > 0 || excerpt.len() > MAX_BUILD_SCRIPT_BYTES {
        tail(&excerpt, MAX_BUILD_SCRIPT_BYTES).replace("[earlier output omitted]", "[earlier lines omitted]")
    } else {
        excerpt
    })
}

/// Replace every exact inherited environment value with its name, longest value first.
fn redact_environment(
    text: &str,
    environment: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> String {
    let mut values = environment
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, value)| {
            value.len() >= MIN_REDACTED_VALUE_BYTES && !UNREDACTED_LOCATIONS.contains(&name.as_str())
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| right.1.len().cmp(&left.1.len()).then_with(|| left.0.cmp(&right.0)));
    values.into_iter().fold(text.to_string(), |text, (name, value)| {
        text.replace(&value, &format!("<env:{name}>"))
    })
}

fn cause(stderr: &str, diagnostics: &[String], error_targets: &[String]) -> Cause {
    // Cargo-Rail's adapters print unindented `cargo-rail <component>: <cause>` lines.
    // Build-script output is indented by Cargo, so it cannot impersonate an adapter.
    if let Some(line) = stderr
        .lines()
        .find(|line| line.starts_with("cargo-rail ") && line.contains(": "))
    {
        return Cause::CargoRail(line.trim().to_string());
    }
    for diagnostic in diagnostics {
        let first = diagnostic.lines().next().unwrap_or_default();
        if first.starts_with("error[E0463]")
            && ["`std`", "`core`", "`alloc`"]
                .iter()
                .any(|crate_name| first.ends_with(crate_name))
        {
            return Cause::MissingTargetLibrary;
        }
        if let Some(linker) = first
            .strip_prefix("error: linker `")
            .and_then(|rest| rest.strip_suffix("` not found"))
        {
            return Cause::MissingLinker(linker.to_string());
        }
        // The OS wording varies; error 2 is "not found" on every supported platform.
        if let Some(path) = first
            .strip_prefix("error: couldn't read `")
            .and_then(|rest| rest.split_once("`: "))
            .filter(|(_, reason)| reason.ends_with("(os error 2)"))
            .and_then(|(path, _)| named_input(path))
        {
            return Cause::MissingSourceFile(lexically_normal(&path));
        }
    }
    if let Some(package) = stderr.lines().find_map(|line| {
        line.strip_prefix("error: failed to run custom build command for `")?
            .split_once('`')
            .map(|(package, _)| package_without_source(package))
    }) {
        return Cause::BuildScript {
            package,
            environment: build_script_environment(stderr),
            missing_tool: build_script_stderr(stderr).as_deref().and_then(missing_tool),
        };
    }
    // Cargo probes `rustc -vV` through the configured wrapper before compiling anything.
    if stderr.lines().any(|line| {
        line.starts_with("error: ")
            && line.contains(" -vV`")
            && (line.contains("could not execute process `") || line.contains("process didn't exit successfully: `"))
    }) {
        return Cause::CompilerProbe;
    }
    if !error_targets.is_empty() {
        return Cause::Source;
    }
    Cause::Unclassified
}

/// A native tool that a build script reported as not installed, from the messages of the
/// `cc`, `cmake`, and `pkg-config` crates that most build scripts use.
fn missing_tool(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        if let Some(rest) = line.split_once("failed to find tool \"").map(|(_, rest)| rest) {
            return rest.split_once('"').and_then(|(tool, _)| named_input(tool));
        }
        if let Some(rest) = line.split_once("is `").map(|(_, rest)| rest)
            && let Some((tool, _)) = rest.split_once("` not installed?")
        {
            return named_input(tool);
        }
        line.contains("The pkg-config command could not be found")
            .then(|| "pkg-config".to_string())
    })
}

/// Accept a tool name or path into a message only when it is short and path-safe.
fn named_input(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_NAMED_INPUT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-+/\\:".contains(&byte)))
    .then(|| value.to_string())
}

/// Remove `.` and `name/..` components without touching the filesystem.
fn lexically_normal(path: &str) -> String {
    let mut parts = Vec::<&str>::new();
    for part in path.split('/') {
        match part {
            "." => {}
            ".." if parts.last().is_some_and(|last| !last.is_empty() && *last != "..") => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// `name v1.2.3 (path+file:///...)` becomes `name v1.2.3`.
fn package_without_source(package: &str) -> String {
    package
        .split_once(" (")
        .map_or(package, |(package, _)| package)
        .to_string()
}

/// Environment variable names from `rerun-if-env-changed` instructions, never their values.
fn build_script_environment(stderr: &str) -> Vec<String> {
    let names = stderr
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("cargo::rerun-if-env-changed=")
                .or_else(|| line.strip_prefix("cargo:rerun-if-env-changed="))
        })
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.')
        })
        .collect::<BTreeSet<_>>();
    names
        .into_iter()
        .take(MAX_ENVIRONMENT_NAMES)
        .map(str::to_string)
        .collect()
}

/// Rendered error diagnostics from Cargo's JSON messages, each distinct one once.
///
/// `--all-targets` compiles a library for both its lib and test targets, so the same
/// error can arrive twice.
fn compiler_errors(stdout: &[u8]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["reason"] == "compiler-message" && event["message"]["level"] == "error")
        .filter_map(|event| {
            event["message"]["rendered"]
                .as_str()
                .or_else(|| event["message"]["message"].as_str())
                .map(|rendered| rendered.trim_end().to_string())
        })
        .filter(|rendered| seen.insert(rendered.clone()))
        .collect()
}

/// The view's Cargo command as a user can run it, without machine message formatting.
///
/// Cargo-Rail runs the view in a private target directory through its compiler adapter.
/// Neither changes which packages, features, targets, or build scripts Cargo runs.
fn reproduction(view: &FailedView<'_>) -> String {
    let command = std::iter::once(view.cargo_program)
        .chain(
            view.cargo_arguments
                .iter()
                .map(std::ffi::OsString::as_os_str)
                .filter(|argument| !argument.to_string_lossy().starts_with("--message-format")),
        )
        .map(|part| crate::utils::shell_quote(&part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "cd {} && {command}",
        crate::utils::shell_quote(&view.workspace_root.to_string_lossy())
    )
}

/// The last `limit` bytes of `text`, starting on a line boundary when one is available.
fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let tail = (text.len() - limit..text.len())
        .find_map(|start| text.get(start..))
        .unwrap_or_default();
    let tail = tail.split_once('\n').map_or(tail, |(_, rest)| rest);
    format!("[earlier output omitted]\n{tail}")
}

/// The first `limit` bytes of `text`, ending on a character boundary.
fn head(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let head = (0..=limit).rev().find_map(|end| text.get(..end)).unwrap_or_default();
    format!("{head}\n[later diagnostics omitted]")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cargo 1.98.1 output for a build script that fails after declaring its environment.
    const BUILD_SCRIPT: &str = r"   Compiling a v0.1.0 (/work/a)
error: failed to run custom build command for `a v0.1.0 (/work/a)`

Caused by:
  process didn't exit successfully: `/sandbox/build/a-42c8/build-script-build` (exit status: 1)
  --- stdout
  cargo:rerun-if-env-changed=NATIVE_SDK_ROOT
  cargo::rerun-if-env-changed=CC_aarch64

  --- stderr
  cargo-rail compiler observation: impersonated
  error: SECRET_TOKEN=hunter2
";
    const WRAPPER: &str = "error: process didn't exit successfully: `/tmp/wrapper /toolchain/bin/rustc -vV` (exit status: 2)\n\
--- stderr\nwrapper: error: connection refused\n";
    const MISSING_WRAPPER: &str =
        "error: could not execute process `/missing /toolchain/bin/rustc -vV` (never executed)\n";

    fn message(rendered: &str) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "package_id": "path+file:///work/a#0.1.0",
            "target": {"name": "a", "kind": ["lib"]},
            "message": {"level": "error", "message": "", "rendered": rendered},
        })
        .to_string()
    }

    fn view<'a>(
        stdout: &'a [u8],
        stderr: &'a [u8],
        error_targets: &'a [String],
        arguments: &'a [std::ffi::OsString],
    ) -> FailedView<'a> {
        FailedView {
            label: "a / default / default-features",
            platform: "default",
            status: "exit status: 101",
            cargo_program: OsStr::new("cargo"),
            cargo_arguments: arguments,
            workspace_root: Path::new("/work space"),
            error_targets,
            stdout,
            stderr,
            show_cargo_output: true,
        }
    }

    fn arguments() -> Vec<std::ffi::OsString> {
        [
            "check",
            "--locked",
            "--all-targets",
            "--message-format=json",
            "--package",
            "a",
        ]
        .into_iter()
        .map(Into::into)
        .collect()
    }

    #[test]
    fn build_script_failures_name_the_package_and_environment_names_only() {
        let arguments = arguments();
        let error = classify(&view(b"", BUILD_SCRIPT.as_bytes(), &[], &arguments));
        let failure = error.classified().expect("classified");
        assert_eq!(failure.class(), FailureClass::BuildScript);
        let message = error.to_string();
        let help = error.help_message().expect("recovery");
        assert!(
            message.contains("`a v0.1.0`") && message.contains("a / default / default-features"),
            "{message}"
        );
        assert!(help.contains("`CC_aarch64`, `NATIVE_SDK_ROOT`"), "{help}");
        assert!(
            help.contains("reproduce with Cargo: cd '/work space' && cargo check --locked --all-targets --package a"),
            "{help}"
        );
        for text in [message.as_str(), help.as_str()] {
            assert!(!text.contains("hunter2") && !text.contains("/sandbox/"), "{text}");
        }
        // Tests run without --verbose: text detail is the build script's own stderr, not Cargo's.
        // Redaction depends on the test process environment, so match only its fixed parts.
        let detail = failure.detail().expect("build-script detail");
        assert!(
            detail.starts_with("the build script reported:\n")
                && detail.contains(" compiler observation: impersonated\nerror: SECRET_TOKEN=hunter2")
                && !detail.contains("/sandbox/"),
            "{detail}"
        );
    }

    #[test]
    fn build_script_excerpt_keeps_only_the_last_lines_of_the_failing_script() {
        let lines = (0..30).map(|index| format!("  line {index}\n")).collect::<String>();
        let stderr = format!(
            "   Compiling a v0.1.0\nerror: failed to run custom build command for `a v0.1.0`\n\nCaused by:\n  --- stdout\n  noise\n\n  --- stderr\n{lines}\nwarning: build failed\n"
        );
        let excerpt = build_script_stderr(&stderr).expect("excerpt");
        assert!(
            excerpt.starts_with("line 10\n") && excerpt.ends_with("line 29"),
            "{excerpt}"
        );
        assert_eq!(
            build_script_stderr("error: failed to run custom build command for `a`\n  --- stderr\n\n"),
            None
        );
    }

    #[test]
    fn missing_native_tools_and_files_are_named_only_from_fixed_patterns() {
        assert_eq!(
            missing_tool(
                "error occurred in cc-rs: failed to find tool \"aarch64-linux-gnu-gcc\": No such file or directory (os error 2)"
            ),
            Some("aarch64-linux-gnu-gcc".to_string())
        );
        assert_eq!(
            missing_tool(
                "failed to execute command: No such file or directory (os error 2)\nis `cmake` not installed?"
            ),
            Some("cmake".to_string())
        );
        assert_eq!(
            missing_tool("The pkg-config command could not be found."),
            Some("pkg-config".to_string())
        );
        assert_eq!(missing_tool("error: SECRET_TOKEN=hunter2"), None);
        assert_eq!(
            missing_tool("failed to find tool \"cc; rm -rf /\": x"),
            None,
            "unsafe characters"
        );
        assert_eq!(named_input(&"a".repeat(MAX_NAMED_INPUT_BYTES + 1)), None);
        assert_eq!(
            lexically_normal("crates/a/src/../generated/data.bin"),
            "crates/a/generated/data.bin"
        );
        assert_eq!(lexically_normal("../outside/./x"), "../outside/x");

        let arguments = arguments();
        let targets = vec!["a (lib)".to_string()];
        let missing = message(
            "error: couldn't read `a/src/../generated/data.bin`: No such file or directory (os error 2)\n --> a/src/lib.rs:1:26\n",
        );
        let twice = format!("{missing}\n{missing}\n");
        let error = classify(&view(twice.as_bytes(), b"", &targets, &arguments));
        let failure = error.classified().expect("classified");
        assert_eq!(failure.class(), FailureClass::Source);
        assert!(
            error
                .to_string()
                .starts_with("a (lib) reads `a/generated/data.bin`, which does not exist"),
            "{error}"
        );
        assert!(
            error
                .help_message()
                .expect("recovery")
                .starts_with("create or generate `a/generated/data.bin`"),
            "{error:?}"
        );
        let detail = failure.detail().expect("diagnostics");
        assert_eq!(
            detail.matches("couldn't read").count(),
            1,
            "identical errors appear once:\n{detail}"
        );

        let stderr = BUILD_SCRIPT.replace(
            "error: SECRET_TOKEN=hunter2",
            "error occurred in cc-rs: failed to find tool \"rail-fixture-cc\": No such file or directory (os error 2)",
        );
        let error = classify(&view(b"", stderr.as_bytes(), &[], &arguments));
        assert_eq!(
            error.classified().expect("classified").class(),
            FailureClass::BuildScript
        );
        assert!(
            error
                .to_string()
                .starts_with("the build script of `a v0.1.0` cannot run `rail-fixture-cc`, which is not installed"),
            "{error}"
        );
        assert!(
            error
                .help_message()
                .expect("recovery")
                .starts_with("install `rail-fixture-cc`, or point the build script at an installed tool (the build script declares that it reads `CC_aarch64`, `NATIVE_SDK_ROOT`)"),
            "{error:?}"
        );
    }

    #[test]
    fn inherited_environment_values_are_replaced_by_their_names() {
        let environment = [
            ("SHORT", "abc"),
            ("API_TOKEN", "token-value-1234"),
            ("API_TOKEN_PREFIX", "token-value"),
            ("HOME", "/home/builder"),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()));
        let redacted = redact_environment("abc token-value-1234 token-value /home/builder/x", environment);
        assert_eq!(redacted, "abc <env:API_TOKEN> <env:API_TOKEN_PREFIX> /home/builder/x");
    }

    #[test]
    fn toolchain_failures_are_distinct_from_source_and_build_scripts() {
        let arguments = arguments();
        let no_std = message(
            "error[E0463]: can't find crate for `std`\n  |\n  = note: the `riscv32imc-unknown-none-elf` target may not be installed\n",
        );
        let linker = message(
            "error: linker `definitely-missing-ld` not found\n  |\n  = note: No such file or directory (os error 2)\n",
        );
        let source = message("error: expected one of `:`, `@`, or `|`, found `{`\n --> a/src/lib.rs:1:15\n");
        let targets = vec!["a (lib)".to_string()];
        let cases = [
            (
                no_std.as_bytes(),
                b"".as_slice(),
                &targets[..],
                FailureClass::Toolchain,
                "standard library",
            ),
            (
                linker.as_bytes(),
                b"".as_slice(),
                &targets[..],
                FailureClass::Toolchain,
                "`definitely-missing-ld`",
            ),
            (
                b"".as_slice(),
                WRAPPER.as_bytes(),
                &[][..],
                FailureClass::Toolchain,
                "rustc wrapper",
            ),
            (
                b"".as_slice(),
                MISSING_WRAPPER.as_bytes(),
                &[][..],
                FailureClass::Toolchain,
                "rustc wrapper",
            ),
            (
                source.as_bytes(),
                b"".as_slice(),
                &targets[..],
                FailureClass::Source,
                "a (lib) did not compile",
            ),
            (
                b"".as_slice(),
                b"error: something new\n".as_slice(),
                &[][..],
                FailureClass::Cargo,
                "exit status: 101",
            ),
        ];
        for (stdout, stderr, error_targets, class, expected) in cases {
            let error = classify(&view(stdout, stderr, error_targets, &arguments));
            assert_eq!(
                error.classified().map(|failure| failure.class()),
                Some(class),
                "{error}"
            );
            assert!(error.to_string().contains(expected), "{error}");
        }
        let source = classify(&view(source.as_bytes(), b"", &targets, &arguments));
        assert!(
            source
                .classified()
                .and_then(|failure| failure.detail())
                .is_some_and(|detail| detail.contains("a/src/lib.rs:1:15")),
            "source diagnostics are the cause and stay visible in text output"
        );
    }

    #[test]
    fn only_unindented_adapter_lines_identify_a_cargo_rail_defect() {
        let arguments = arguments();
        let adapter = "error: could not compile `a` (lib)\ncargo-rail compiler observation: incompatible private invocation context\n";
        let error = classify(&view(b"", adapter.as_bytes(), &[], &arguments));
        assert_eq!(
            error.classified().map(|failure| failure.class()),
            Some(FailureClass::CargoRail)
        );
        assert!(
            error.to_string().contains("incompatible private invocation context"),
            "{error}"
        );
        // The indented line in BUILD_SCRIPT is build-script output, not an adapter failure.
        let error = classify(&view(b"", BUILD_SCRIPT.as_bytes(), &[], &arguments));
        assert_eq!(
            error.classified().map(|failure| failure.class()),
            Some(FailureClass::BuildScript)
        );
    }

    #[test]
    fn retained_output_is_bounded_on_character_and_line_boundaries() {
        let text = format!("{}\nlast line é\n", "é".repeat(20));
        let tail = tail(&text, 16);
        assert_eq!(tail, "[earlier output omitted]\nlast line é\n");
        let head = head(&"é".repeat(10), 5);
        assert_eq!(head, "éé\n[later diagnostics omitted]");
    }

    #[test]
    fn environment_names_are_bounded_and_values_are_rejected() {
        let stderr = (0..20)
            .map(|index| format!("  cargo:rerun-if-env-changed=NAME_{index:02}\n"))
            .chain(std::iter::once(
                "  cargo:rerun-if-env-changed=TOKEN=value\n".to_string(),
            ))
            .collect::<String>();
        let names = build_script_environment(&stderr);
        assert_eq!(names.len(), MAX_ENVIRONMENT_NAMES);
        assert!(names.iter().all(|name| !name.contains('=')), "{names:?}");
    }
}
