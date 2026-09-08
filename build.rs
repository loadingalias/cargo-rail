//! Declares the target identity and compiler-fact inputs consumed while building cargo-rail.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if let Err(error) = embed_benchmark_workload() {
        println!("cargo::error=failed to embed cache benchmark workload: {error}");
        return;
    }
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
        "CARGO_RAIL_FACT_DRIVER_SOURCE_FILE",
        "CARGO_RAIL_FACT_DRIVER_SOURCE_SHA256",
        "CARGO_RAIL_FACT_DRIVER_SOURCE_PROVENANCE",
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

fn embed_benchmark_workload() -> std::io::Result<()> {
    use std::fmt::Write as _;
    use std::path::Path;

    fn visit(root: &Path, directory: &Path, source: &mut String) -> std::io::Result<()> {
        println!("cargo::rerun-if-changed={}", directory.display());
        let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &path, source)?;
            } else if kind.is_file() {
                let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
                let relative = relative
                    .to_str()
                    .ok_or_else(|| std::io::Error::other("non-UTF-8 workload path"))?;
                let relative = relative.replace('\\', "/");
                // Cargo excludes nested packages even when the parent explicitly includes their directory.
                // Templates keep the installed benchmark's complete workload in the source package.
                let relative = relative.strip_suffix(".in").unwrap_or(&relative);
                writeln!(source, "({relative:?}, include_bytes!({path:?})),").map_err(std::io::Error::other)?;
            } else {
                return Err(std::io::Error::other("workload contains a non-regular input"));
            }
        }
        Ok(())
    }

    let root =
        std::env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| std::io::Error::other("missing manifest directory"))?;
    let root = Path::new(&root).join("tests/fixtures/native_cache/real_world");
    let mut source = String::from("const FILES: &[(&str, &[u8])] = &[\n");
    visit(&root, &root, &mut source)?;
    source.push_str("];\n");
    let output = std::env::var_os("OUT_DIR").ok_or_else(|| std::io::Error::other("missing build output directory"))?;
    std::fs::write(Path::new(&output).join("benchmark-workload.rs"), source)
}
