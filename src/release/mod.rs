//! Release planning, validation, durable execution, and recovery.

pub(crate) mod attribution;
pub(crate) mod auxiliary;
pub mod change_files;
pub(crate) mod changelog;
mod path_serde;
pub mod planner;
pub(crate) mod presentation;
mod process;
pub mod publisher;
pub(crate) mod remote;
pub(crate) mod semver_checks;
pub(crate) mod state;
pub mod validator;
pub mod version;

pub use changelog::{CommitDiagnostic, CommitIssue};
pub use presentation::{PlannedChangelog, PlannedPresentation, ReleaseNoteInput};

pub use planner::ReleasePlanner;
pub use publisher::ReleasePublisher;
pub use validator::ReleaseValidator;
pub use version::VersionBumper;
