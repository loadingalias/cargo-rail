//! Installed cache benchmark entry point.

#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = cargo_rail::benchmark::run(std::env::args_os()) {
        eprintln!("cargo-rail-bench: {error}");
        std::process::exit(2);
    }
}
