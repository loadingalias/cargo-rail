//! Authenticated compiler-cache worker behind the minimal opt-out launcher.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

#[path = "shared/component_version.rs"]
mod component_version;

fn main() {
    if component_version::report_if_requested() {
        return;
    }
    std::process::exit(cargo_rail::compiler::invocation::dispatch_required());
}
