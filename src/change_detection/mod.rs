//! Internal change interpretation shared by release attribution and planning.

mod classify;
pub(crate) mod semantic;

pub(crate) use classify::classify_path;
