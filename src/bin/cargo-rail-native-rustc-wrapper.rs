//! Thin process entry for cargo-rail's native compiler cache wrapper.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

fn main() {
  let exit_code = cargo_rail::compiler::wrapper::run_if_requested().unwrap_or_else(|| {
    eprintln!("cargo-rail compiler cache wrapper: missing private invocation context");
    2
  });
  std::process::exit(exit_code);
}
