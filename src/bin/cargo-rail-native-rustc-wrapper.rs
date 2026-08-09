//! Thin process entry for cargo-rail's native compiler cache wrapper.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

fn main() {
  std::process::exit(cargo_rail::compiler::invocation::dispatch_required());
}
