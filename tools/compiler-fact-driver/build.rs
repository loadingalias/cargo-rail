//! Portable loader metadata for the separately distributed companion.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(cargo_rail_rustc_local_mod_id)");
    println!("cargo::rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo::rerun-if-env-changed=RUSTC");
    if selected_rustc_minor().is_some_and(|minor| minor >= 99) {
        println!("cargo::rustc-cfg=cargo_rail_rustc_local_mod_id");
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        let library = selected_rustc_sysroot().join("lib");
        println!("cargo::rustc-link-search=native={}", library.display());
        println!("cargo::rustc-link-arg-bin=cargo-rail-fact-driver=-Wl,-rpath,$ORIGIN/../lib");
    }
}

fn selected_rustc_minor() -> Option<u64> {
    let rustc = std::env::var_os("RUSTC")?;
    let output = Command::new(rustc).arg("-vV").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let verbose = String::from_utf8(output.stdout).ok()?;
    let release = verbose.lines().find_map(|line| line.strip_prefix("release: "))?;
    let mut fields = release.split(['.', '-']);
    (fields.next()? == "1").then_some(())?;
    fields.next()?.parse().ok()
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
