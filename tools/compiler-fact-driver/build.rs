//! Portable loader metadata and `rustc_private` API selection for the separately distributed companion.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Command;

/// `rustc_private` changes since the compiler adapter floor. Each entry names its cfg, the release whose nightly
/// cycle merged it, and the UTC merge date. A nightly contains a change from its commit date on; a beta or stable
/// release contains every change of its own and earlier cycles.
const RUSTC_API_CHANGES: &[(&str, u64, &str)] = &[
    // rust-lang/rust#155697: `c_variadic` is stable.
    ("rail_stable_c_variadic", 99, "2026-07-22"),
    // rust-lang/rust#158823: `FileSearch` indexes the files of every search path together.
    ("rail_flat_file_search", 99, "2026-08-01"),
    // rust-lang/rust#160559: `CodegenBackend::join_codegen` receives the incremental session.
    ("rail_incremental_session", 99, "2026-08-05"),
    // rust-lang/rust#161458: `CrateType` moved to `rustc_structures`.
    ("rail_structures_crate_type", 100, "2026-08-21"),
    // rust-lang/rust#161772: `DefPathData::TestBinderConstraints`.
    ("rail_test_binder_constraints", 100, "2026-08-25"),
    // rust-lang/rust#161432: `CodegenBackend::init` returns the backend's settings from an `EarlySession`.
    ("rail_early_session_backend", 100, "2026-09-16"),
    // rust-lang/rust#161349: HIR keeps each `use` item's tree; nested imports are no longer items.
    ("rail_nested_use_trees", 100, "2026-09-25"),
];

fn main() {
    println!("cargo::rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo::rerun-if-env-changed=RUSTC");
    let identity = rustc_output(&["-vV"]);
    let field = |name: &str| {
        identity
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
            .unwrap_or_else(|| panic!("selected rustc did not report its {name}"))
    };
    let release = field("release");
    let (version, channel) = release.split_once('-').unwrap_or((release, ""));
    let minor = match version.split('.').collect::<Vec<_>>()[..] {
        ["1", minor, _] => minor.parse::<u64>().ok(),
        _ => None,
    }
    .unwrap_or_else(|| panic!("selected rustc reported an invalid release: {release}"));
    // Beta and stable releases are cut from the end of their nightly cycle.
    let cycle_complete = !(channel == "nightly" || channel == "dev");
    for &(name, changed_minor, merged) in RUSTC_API_CHANGES {
        println!("cargo::rustc-check-cfg=cfg({name})");
        if minor > changed_minor || (minor == changed_minor && (cycle_complete || commit_date(field) >= merged)) {
            println!("cargo::rustc-cfg={name}");
        }
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "macos") {
        let library = selected_rustc_sysroot().join("lib");
        println!("cargo::rustc-link-search=native={}", library.display());
    }
    if target_os == "linux" {
        println!("cargo::rustc-link-arg-bin=cargo-rail-fact-driver=-Wl,-rpath,$ORIGIN/../lib");
    } else if target_os == "macos" {
        println!("cargo::rustc-link-arg-bin=cargo-rail-fact-driver=-Wl,-rpath,@loader_path/../lib");
    }
}

/// The ISO date of a nightly's newest merge; it orders lexically.
fn commit_date<'a>(field: impl Fn(&str) -> &'a str) -> &'a str {
    let date = field("commit-date");
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        panic!("selected rustc reported no usable commit date: {date}");
    }
    date
}

fn selected_rustc_sysroot() -> PathBuf {
    let sysroot = PathBuf::from(rustc_output(&["--print", "sysroot"]).trim());
    if !sysroot.is_absolute() || !sysroot.join("lib").is_dir() {
        panic!("selected rustc reported an invalid sysroot");
    }
    sysroot
}

fn rustc_output(arguments: &[&str]) -> String {
    let Some(rustc) = std::env::var_os("RUSTC") else {
        panic!("Cargo did not identify the selected rustc executable");
    };
    let output = match Command::new(rustc).args(arguments).output() {
        Ok(output) => output,
        Err(error) => panic!("failed to run the selected rustc {arguments:?}: {error}"),
    };
    if !output.status.success() {
        panic!(
            "selected rustc {arguments:?} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    match String::from_utf8(output.stdout) {
        Ok(stdout) => stdout,
        Err(error) => panic!("selected rustc output is not UTF-8: {error}"),
    }
}
