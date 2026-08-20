//! Minimal opt-out launcher for cargo-rail's native compiler cache worker.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
        eprintln!("cargo-rail compiler cache wrapper: missing compiler executable");
        std::process::exit(1);
    };
    if std::env::var_os("CARGO_RAIL_CACHE").as_deref() == Some(std::ffi::OsStr::new("off")) && !has_non_cache_role() {
        let mut command = std::process::Command::new(program);
        command.args(arguments);
        exit_with(command, "cargo-rail compiler cache wrapper");
    }
    let launcher = match std::env::current_exe() {
        Ok(launcher) => launcher,
        Err(error) => {
            eprintln!("cargo-rail compiler cache launcher: failed to resolve executable: {error}");
            std::process::exit(1);
        }
    };
    let Some(directory) = launcher.parent() else {
        eprintln!("cargo-rail compiler cache launcher: executable has no parent directory");
        std::process::exit(1);
    };
    #[cfg(not(windows))]
    let worker = directory.join("cargo-rail-native-rustc-worker");
    #[cfg(windows)]
    let worker = directory.join("cargo-rail-native-rustc-worker.exe");
    let mut command = std::process::Command::new(worker);
    command
        .arg(program)
        .args(arguments)
        .env("CARGO_RAIL_DIRECT_CACHE_LAUNCHER", launcher);
    exit_with(command, "cargo-rail compiler cache launcher");
}

fn has_non_cache_role() -> bool {
    [
        "CARGO_RAIL_APPLE_LINK_ADAPTER",
        "CARGO_RAIL_COMPILER_CACHE_WRAPPER",
        "CARGO_RAIL_COMPILER_FACT_DOCTEST_BUILDER",
        "CARGO_RAIL_COMPILER_FACT_DOCTEST_RUNNER",
        "CARGO_RAIL_ELF_LINK_ADAPTER",
        "CARGO_RAIL_RUSTC_WRAPPER",
        "CARGO_RAIL_RUSTDOC_WRAPPER",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
}

#[cfg(unix)]
fn exit_with(mut command: std::process::Command, context: &str) -> ! {
    use std::os::unix::process::CommandExt as _;

    let error = command.exec();
    eprintln!("{context}: failed to execute compiler: {error}");
    std::process::exit(1);
}

#[cfg(not(unix))]
fn exit_with(mut command: std::process::Command, context: &str) -> ! {
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("{context}: failed to execute compiler: {error}");
            std::process::exit(1);
        }
    }
}
