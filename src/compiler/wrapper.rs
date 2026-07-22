//! Transparent compiler proxies for exact invocation observations.
//!
//! Cargo invokes rustc mode through `RUSTC_WORKSPACE_WRAPPER`, so the unused
//! dependency lint is applied only to workspace members. Rustdoc observation
//! uses Cargo's selected `RUSTDOC` slot and retains that program as the inner
//! executable because Cargo has no rustdoc-wrapper setting.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Marker set by the diagnostics collector when this executable is acting as a
/// rustc workspace wrapper.
pub const WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTC_WRAPPER";

/// Existing workspace wrapper saved by the collector for transparent chaining.
pub const INNER_WRAPPER_ENV: &str = "CARGO_RAIL_INNER_WORKSPACE_WRAPPER";

/// Marker set when this executable is transparently proxying rustdoc.
pub const RUSTDOC_WRAPPER_MARKER: &str = "CARGO_RAIL_RUSTDOC_WRAPPER";

/// Selected rustdoc executable retained behind the cargo-rail observation proxy.
pub const INNER_RUSTDOC_ENV: &str = "CARGO_RAIL_INNER_RUSTDOC";

/// Private directory where diagnostics wrappers publish immutable invocation evidence.
pub const OBSERVATION_DIRECTORY_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY";

/// Physical source root used only to normalize and revalidate observation paths.
pub const OBSERVATION_SOURCE_ROOT_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT";

/// Record invocations without enabling cargo-rail's workspace diagnostic lint.
pub const OBSERVATION_ONLY_ENV: &str = "CARGO_RAIL_COMPILER_OBSERVATION_ONLY";

/// Compose Cargo's stable wrapper order: global wrapper, workspace wrapper, rustc.
pub(crate) fn rustc_command(
  rustc: &std::ffi::OsStr,
  rustc_wrapper: Option<&std::ffi::OsStr>,
  workspace_wrapper: Option<&std::ffi::OsStr>,
) -> Command {
  match (rustc_wrapper, workspace_wrapper) {
    (Some(wrapper), Some(workspace_wrapper)) => {
      let mut command = Command::new(wrapper);
      command.arg(workspace_wrapper).arg(rustc);
      command
    }
    (Some(wrapper), None) => {
      let mut command = Command::new(wrapper);
      command.arg(rustc);
      command
    }
    (None, Some(workspace_wrapper)) => {
      let mut command = Command::new(workspace_wrapper);
      command.arg(rustc);
      command
    }
    (None, None) => Command::new(rustc),
  }
}

/// Run rustc wrapper mode when requested by the diagnostics collector.
///
/// Returns `None` during normal cargo-rail CLI execution.
#[must_use]
pub fn run_if_requested() -> Option<i32> {
  if std::env::var_os(WRAPPER_MARKER).is_some() {
    return Some(run_rustc());
  }
  std::env::var_os(RUSTDOC_WRAPPER_MARKER)?;
  Some(run_rustdoc())
}

fn run_rustc() -> i32 {
  let mut args = std::env::args_os().skip(1);
  let Some(rustc) = args.next() else {
    eprintln!("cargo-rail rustc wrapper: missing rustc executable");
    return 1;
  };

  let remaining: Vec<OsString> = args.collect();
  let inner_wrapper = std::env::var_os(INNER_WRAPPER_ENV);
  let recorder = (!is_rustc_information_request(&remaining))
    .then(|| {
      std::env::var_os(OBSERVATION_DIRECTORY_ENV)
        .zip(std::env::var_os(OBSERVATION_SOURCE_ROOT_ENV))
        .and_then(|(directory, source_root)| {
          crate::compiler::observation::begin_invocation(
            &PathBuf::from(directory),
            &PathBuf::from(source_root),
            &rustc,
            &remaining,
          )
          .ok()
        })
    })
    .flatten();
  let mut command = rustc_command(&rustc, None, inner_wrapper.as_deref());
  command.args(&remaining);
  if std::env::var_os(OBSERVATION_ONLY_ENV).is_none() {
    command.arg("--warn=unused-crate-dependencies");
  }
  let status = command
    .env_remove(WRAPPER_MARKER)
    .env_remove(INNER_WRAPPER_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .env_remove(OBSERVATION_ONLY_ENV)
    .status();

  if let Some(recorder) = recorder {
    let _ = recorder.finish(status.as_ref().is_ok_and(std::process::ExitStatus::success));
  }

  match status {
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("cargo-rail rustc wrapper: failed to execute compiler: {error}");
      1
    }
  }
}

fn is_rustc_information_request(arguments: &[OsString]) -> bool {
  arguments.iter().any(|argument| {
    matches!(
      argument.to_str(),
      Some("-h" | "--help" | "-V" | "--version" | "-vV" | "--print")
    ) || argument
      .to_str()
      .is_some_and(|argument| argument.starts_with("--print="))
  })
}

fn run_rustdoc() -> i32 {
  let Some(rustdoc) = std::env::var_os(INNER_RUSTDOC_ENV) else {
    eprintln!("cargo-rail rustdoc proxy: missing selected rustdoc executable");
    return 1;
  };
  let original_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
  let records_compilation = !is_rustdoc_information_request(&original_arguments);
  let arguments = rustdoc_observation_arguments(&rustdoc, original_arguments);
  let recorder = if records_compilation {
    std::env::var_os(OBSERVATION_DIRECTORY_ENV)
      .zip(std::env::var_os(OBSERVATION_SOURCE_ROOT_ENV))
      .and_then(|(directory, source_root)| {
        crate::compiler::observation::begin_rustdoc_invocation(
          &PathBuf::from(directory),
          &PathBuf::from(source_root),
          &arguments,
        )
        .ok()
      })
  } else {
    None
  };
  let status = Command::new(&rustdoc)
    .args(&arguments)
    .env("RUSTDOC", &rustdoc)
    .env_remove(RUSTDOC_WRAPPER_MARKER)
    .env_remove(INNER_RUSTDOC_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .status();

  if let Some(recorder) = recorder {
    let _ = recorder.finish(status.as_ref().is_ok_and(std::process::ExitStatus::success));
  }

  match status {
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("cargo-rail rustdoc proxy: failed to execute rustdoc: {error}");
      1
    }
  }
}

fn rustdoc_observation_arguments(rustdoc: &std::ffi::OsStr, mut arguments: Vec<OsString>) -> Vec<OsString> {
  if is_rustdoc_information_request(&arguments)
    || arguments
      .iter()
      .any(|argument| argument.as_encoded_bytes().starts_with(b"@"))
    || arguments
      .iter()
      .any(|argument| matches!(argument.to_str(), Some("--test" | "--check")))
    || uses_non_html_output_format(&arguments)
    || !is_cargo_rustdoc_crate_invocation(&arguments)
  {
    return arguments;
  }

  let mut index = 0usize;
  while index < arguments.len() {
    if arguments[index] == "--emit" {
      if let Some(value) = arguments.get_mut(index + 1)
        && let Some(value) = value.to_str()
        && let Some(extended) = rustdoc_emit_with_dep_info(rustdoc, value)
      {
        arguments[index + 1] = extended.into();
      }
      return arguments;
    }
    if let Some(value) = arguments[index]
      .to_str()
      .and_then(|argument| argument.strip_prefix("--emit="))
    {
      if let Some(extended) = rustdoc_emit_with_dep_info(rustdoc, value) {
        arguments[index] = format!("--emit={extended}").into();
      }
      return arguments;
    }
    index += 1;
  }

  if let Some(modes) = rustdoc_default_emit_modes(rustdoc) {
    arguments.push(format!("--emit={modes}").into());
  }
  arguments
}

fn is_cargo_rustdoc_crate_invocation(arguments: &[OsString]) -> bool {
  has_option_value(arguments, "--crate-name")
    && (has_option_value(arguments, "-o")
      || has_option_value(arguments, "--out-dir")
      || has_option_value(arguments, "--output"))
    && arguments.iter().any(|argument| {
      argument
        .to_str()
        .is_some_and(|argument| !argument.starts_with('-') && argument.ends_with(".rs"))
    })
}

fn has_option_value(arguments: &[OsString], option: &str) -> bool {
  arguments.iter().enumerate().any(|(index, argument)| {
    (argument == option && arguments.get(index + 1).is_some_and(|value| !value.is_empty()))
      || argument
        .to_str()
        .and_then(|argument| argument.strip_prefix(option))
        .is_some_and(|value| value.starts_with('=') && value.len() > 1)
  })
}

fn is_rustdoc_information_request(arguments: &[OsString]) -> bool {
  arguments
    .iter()
    .any(|argument| matches!(argument.to_str(), Some("-h" | "--help" | "-V" | "--version" | "-vV")))
}

fn uses_non_html_output_format(arguments: &[OsString]) -> bool {
  arguments.iter().enumerate().any(|(index, argument)| {
    let Some(argument) = argument.to_str() else {
      return true;
    };
    match argument {
      "-w" | "--output-format" => arguments
        .get(index + 1)
        .and_then(|value| value.to_str())
        .is_none_or(|value| value != "html"),
      _ => argument
        .strip_prefix("--output-format=")
        .is_some_and(|value| value != "html"),
    }
  })
}

fn rustdoc_emit_with_dep_info(rustdoc: &std::ffi::OsStr, value: &str) -> Option<String> {
  if emit_contains_dep_info(value) {
    return extend_rustdoc_emit(value, "");
  }
  extend_rustdoc_emit(value, &rustdoc_default_emit_modes(rustdoc)?)
}

fn emit_contains_dep_info(value: &str) -> bool {
  value
    .split(',')
    .map(str::trim)
    .any(|mode| mode.split_once('=').map_or(mode, |(name, _)| name) == "dep-info")
}

fn extend_rustdoc_emit(value: &str, supported: &str) -> Option<String> {
  let mut modes = value.split(',').map(str::trim).collect::<Vec<_>>();
  if modes.is_empty() || modes.iter().any(|mode| mode.is_empty()) {
    return None;
  }
  if emit_contains_dep_info(value) {
    return Some(modes.join(","));
  }
  let supported = supported.split(',').collect::<Vec<_>>();
  if !modes.iter().all(|mode| {
    let name = mode.split_once('=').map_or(*mode, |(name, _)| name);
    supported.contains(&name)
  }) {
    return None;
  }
  modes.push("dep-info");
  Some(modes.join(","))
}

fn rustdoc_default_emit_modes(rustdoc: &std::ffi::OsStr) -> Option<String> {
  let output = Command::new(rustdoc)
    .arg("--help")
    .env("RUSTDOC", rustdoc)
    .env_remove(RUSTDOC_WRAPPER_MARKER)
    .env_remove(INNER_RUSTDOC_ENV)
    .env_remove(OBSERVATION_DIRECTORY_ENV)
    .env_remove(OBSERVATION_SOURCE_ROOT_ENV)
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  rustdoc_emit_modes_from_help(&String::from_utf8(output.stdout).ok()?)
}

fn rustdoc_emit_modes_from_help(help: &str) -> Option<String> {
  let modes = help.lines().find_map(|line| {
    let (_, remainder) = line.split_once("--emit [")?;
    remainder.split_once(']').map(|(modes, _)| modes)
  })?;
  let modes = modes.split(',').map(str::trim).collect::<Vec<_>>();
  matches!(
    modes.as_slice(),
    ["toolchain-shared-resources", "invocation-specific", "dep-info"]
      | ["html-static-files", "html-non-static-files", "dep-info"]
  )
  .then(|| modes.join(","))
}

#[cfg(test)]
mod tests {
  use std::ffi::OsStr;

  use super::*;

  #[test]
  fn rustc_information_requests_do_not_become_compilation_units() {
    for arguments in [
      vec![OsString::from("-vV")],
      vec![OsString::from("--print"), OsString::from("cfg")],
      vec![OsString::from("--print=file-names")],
    ] {
      assert!(is_rustc_information_request(&arguments));
    }
    assert!(!is_rustc_information_request(&[
      OsString::from("--crate-name"),
      OsString::from("unit"),
      OsString::from("src/lib.rs"),
    ]));
  }

  #[test]
  fn rustdoc_emit_discovery_accepts_both_msrv_and_current_names() {
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [toolchain-shared-resources,invocation-specific,dep-info]\n"),
      Some("toolchain-shared-resources,invocation-specific,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [html-static-files,html-non-static-files,dep-info]\n"),
      Some("html-static-files,html-non-static-files,dep-info".to_string())
    );
    assert_eq!(rustdoc_emit_modes_from_help("        --emit [html]\n"), None);
    assert_eq!(
      rustdoc_emit_modes_from_help("        --emit [html-static-files,html-non-static-files,json,dep-info]\n"),
      None
    );
  }

  #[test]
  fn existing_rustdoc_emit_modes_gain_dep_info_without_losing_outputs() {
    assert_eq!(
      extend_rustdoc_emit(
        "html-static-files,html-non-static-files",
        "html-static-files,html-non-static-files,dep-info"
      ),
      Some("html-static-files,html-non-static-files,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_with_dep_info(OsStr::new("missing-rustdoc"), "html-static-files,dep-info"),
      Some("html-static-files,dep-info".to_string())
    );
    assert_eq!(
      rustdoc_emit_with_dep_info(OsStr::new("missing-rustdoc"), "dep-info=unit.d"),
      Some("dep-info=unit.d".to_string())
    );
    assert_eq!(
      extend_rustdoc_emit("json", "html-static-files,html-non-static-files,dep-info"),
      None
    );
    assert_eq!(extend_rustdoc_emit("", "html-static-files,dep-info"), None);
  }

  #[test]
  fn non_html_and_non_rendering_rustdoc_invocations_remain_unchanged() {
    let original = vec!["--crate-name".into(), "unit".into(), "--output-format=json".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["--crate-name".into(), "unit".into(), "--test".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["@arguments".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );

    let original = vec!["README.md".into(), "-o".into(), "doc".into()];
    assert_eq!(
      rustdoc_observation_arguments(OsStr::new("rustdoc"), original.clone()),
      original
    );
  }
}
