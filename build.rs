//! Declares the target identity and compiler-fact inputs consumed while building cargo-rail.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let target = match std::env::var("TARGET") {
        Ok(target) => target,
        Err(error) => {
            println!("cargo::error=failed to determine cargo-rail build target: {error}");
            return;
        }
    };
    println!("cargo::rustc-env=CARGO_RAIL_COMPILED_TARGET={target}");
    for name in [
        "CARGO_RAIL_FACT_DRIVER_FILE",
        "CARGO_RAIL_FACT_DRIVER_SHA256",
        "CARGO_RAIL_FACT_DRIVER_PROVENANCE",
        "CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE",
        "CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT",
        "CARGO_RAIL_FACT_DRIVER_RUSTC_HOST",
        "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY",
        "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256",
    ] {
        println!("cargo::rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(name) {
            println!("cargo::rustc-env={name}={value}");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Clap's debug command builder needs more headroom than MSVC's 1 MiB default.
        // PE reserves virtual address space here; physical pages remain demand-committed.
        println!("cargo::rustc-link-arg-bin=cargo-rail=/STACK:4194304");
    }
}
