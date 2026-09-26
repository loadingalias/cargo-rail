//! Platforms where the driver cannot observe what procedural macros read.
//!
//! Any procedural macro that rustc loads makes the observation incomplete, so its consumers bypass reuse.

use crate::native_input_protocol::{NativeMacroObservation, NativeMacroUnobservable};

pub(crate) fn install() {}

/// `dynamic_crate_loaded` reports whether rustc's crate graph holds a dynamic library outside the sysroot.
pub(crate) fn finish(dynamic_crate_loaded: bool) -> Option<NativeMacroObservation> {
    dynamic_crate_loaded.then(|| NativeMacroObservation {
        paths: Vec::new(),
        environment: Vec::new(),
        spawns: Vec::new(),
        unobservable: vec![NativeMacroUnobservable::ObservationUnavailable],
    })
}
