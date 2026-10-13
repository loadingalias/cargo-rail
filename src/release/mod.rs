//! Release planning and execution from the state Git, the registry, and GitHub record.

pub(crate) mod attribution;
pub(crate) mod cargo_projection;
pub mod change_files;
pub(crate) mod changelog;
pub(crate) mod engine;
mod github;
mod path_serde;
pub mod planner;
pub(crate) mod presentation;
mod process;
mod registry;
pub(crate) mod remote;
pub(crate) mod semver_checks;
pub mod version;

pub use changelog::{CommitDiagnostic, CommitIssue};
pub use presentation::{PlannedChangelog, PlannedPresentation};

pub use planner::ReleasePlanner;
pub use semver_checks::{ApiEvidence, ApiOutcome};
pub use version::VersionBumper;
