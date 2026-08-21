//! Portable loader metadata for the separately distributed companion.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo::rerun-if-env-changed=RUSTC");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        let library = selected_rustc_sysroot().join("lib");
        println!("cargo::rustc-link-search=native={}", library.display());
        println!("cargo::rustc-link-arg-bin=cargo-rail-fact-driver=-Wl,-rpath,$ORIGIN/../lib");
    }
}

fn selected_rustc_sysroot() -> PathBuf {
    let Some(rustc) = std::env::var_os("RUSTC") else {
        panic!("Cargo did not identify the selected rustc executable");
    };
    let output = match Command::new(rustc).args(["--print", "sysroot"]).output() {
        Ok(output) => output,
        Err(error) => panic!("failed to query the selected rustc sysroot: {error}"),
    };
    if !output.status.success() {
        panic!("selected rustc did not report its sysroot");
    }
    let stdout = match String::from_utf8(output.stdout) {
        Ok(stdout) => stdout,
        Err(error) => panic!("selected rustc sysroot is not UTF-8: {error}"),
    };
    let sysroot = PathBuf::from(stdout.trim());
    if !sysroot.is_absolute() || !sysroot.join("lib").is_dir() {
        panic!("selected rustc reported an invalid sysroot");
    }
    sysroot
}
