//! Release identity responder shared by installed compiler executables.

#[path = "../../compiler/component_version.rs"]
mod protocol;

pub(crate) fn report_if_requested() -> bool {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(protocol::ARGUMENT)) || arguments.next().is_some() {
        return false;
    }
    println!("{}\t{}", protocol::RESPONSE_PREFIX, env!("CARGO_PKG_VERSION"));
    true
}
