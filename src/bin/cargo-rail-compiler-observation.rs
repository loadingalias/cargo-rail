//! Dedicated compiler-observation process for surface acquisition.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

fn main() {
    if std::env::args_os().len() == 2
        && std::env::args_os()
            .nth(1)
            .is_some_and(|argument| argument == cargo_rail::compiler::invocation::OBSERVATION_PROTOCOL_ARGUMENT)
    {
        println!("{}", cargo_rail::compiler::invocation::OBSERVATION_PROTOCOL_VERSION);
        return;
    }
    std::process::exit(cargo_rail::compiler::invocation::dispatch_observation_required());
}
