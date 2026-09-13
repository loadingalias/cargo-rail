//! Release planning, validation, durable execution, and recovery.

mod aliases;
pub(crate) mod artifacts;
pub(crate) mod attribution;
pub(crate) mod auxiliary;
pub mod change_files;
pub(crate) mod changelog;
mod contract;
mod credentials;
mod github_release;
pub(crate) mod hosted;
mod packages;
mod path_serde;
pub mod planner;
pub(crate) mod presentation;
mod process;
pub mod publisher;
mod registry;
pub(crate) mod remote;
mod review;
pub(crate) mod semver_checks;
pub(crate) mod state;
mod storage;
mod tags;
pub(crate) mod transfer;
mod validation;
pub mod validator;
pub mod version;

pub use changelog::{CommitDiagnostic, CommitIssue};
pub use presentation::{PlannedChangelog, PlannedPresentation, ReleaseNoteInput};

pub use artifacts::{ArtifactRequirement, AssetRequirement};
pub use planner::ReleasePlanner;
pub use publisher::ReleasePublisher;
pub use semver_checks::{ApiEvidence, ApiOutcome};
pub use validator::ReleaseValidator;
pub use version::VersionBumper;

#[doc(hidden)]
pub use credentials::dispatch_credential_provider;
