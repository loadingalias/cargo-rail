//! The inputs one compiler-evidence view consumed, and whether they still hold.
//!
//! Unify evidence entries and typed compiler-fact sets store the same proof and revalidate it
//! through this module, so every reused compiler result obeys one rule. The cache key binds
//! sources, manifests, the lockfile, the toolchain, targets, and Cargo configuration. The proof
//! adds what the key cannot know in advance:
//!
//! - every file and environment variable that rustc reported reading for the view's units;
//! - the declared rerun inputs of every build script in the view's Cargo graph.
//!
//! Proc-macro reads are bound as Cargo binds them: through the consuming unit's dep-info.

use std::collections::BTreeSet;
use std::path::Path;

use crate::build_script::freshness::BuildScriptFreshness;
use crate::compiler::observation::{CompilationObservationManifest, CompilationTargetKind, ObservationPath};

/// Native-cache bypasses that do not leave an analysis input unobserved.
///
/// Build-script output is bound through Cargo's rerun inputs, and proc-macro reads through
/// the consuming unit's dep-info files and tracked environment. Linker SDK inputs affect only
/// the linked output, after analysis; a failed link fails the unit. Every other bypass,
/// including an unknown one, still rejects reuse.
const SUPERSEDED_NATIVE_BYPASSES: [&str; 5] = [
    "build_script_result_unavailable",
    "build_script_action_key_unavailable",
    "build_script_dependency_graph_incomplete",
    "proc_macro_filesystem_observations_unavailable",
    "native_link_sdk_inputs_unavailable",
];

/// Borrowed proof of one stored view's inputs.
pub(crate) struct CompilerInputProof<'a> {
    observations: &'a [CompilationObservationManifest],
    build_scripts: &'a [BuildScriptFreshness],
}

impl<'a> CompilerInputProof<'a> {
    pub(crate) fn new(
        observations: &'a [CompilationObservationManifest],
        build_scripts: &'a [BuildScriptFreshness],
    ) -> Self {
        Self {
            observations,
            build_scripts,
        }
    }

    /// Why the stored view no longer matches its inputs, or `None` when it still does.
    ///
    /// Observed paths are relative to `source_root`, the repository root they were captured
    /// against; the acquisition sandbox lives below `workspace_root`.
    pub(crate) fn revalidation_reason(&self, source_root: &Path, workspace_root: &Path) -> Option<String> {
        if self.observations.is_empty() {
            return Some("compilation_observations_absent".to_string());
        }
        // A build script's own compilation cannot change the view's output: its sources and
        // dependencies are keyed, and its output is bound by its rerun inputs.
        if let Some(reason) = self
            .observations
            .iter()
            .filter(|manifest| manifest.unit.target_kind != CompilationTargetKind::BuildScript)
            .flat_map(|manifest| manifest.bypasses.iter().map(String::as_str))
            .find(|reason| !SUPERSEDED_NATIVE_BYPASSES.contains(reason))
        {
            return Some(reason.to_string());
        }
        // Files that build scripts generate live in the recycled acquisition sandbox, and the
        // variables they set with `rustc-env` are script output; both are bound by the scripts'
        // rerun inputs below.
        let generated_root = ObservationPath::capture(
            &crate::workspace::cargo_rail_state_root(workspace_root).join("compiler-artifacts-v1"),
            source_root,
            source_root,
        );
        let script_environment = self
            .build_scripts
            .iter()
            .flat_map(BuildScriptFreshness::rustc_environment)
            .collect::<BTreeSet<_>>();
        if let Some(reason) = self.observations.iter().find_map(|manifest| {
            manifest.input_revalidation_reason(source_root, Some(&generated_root), &script_environment)
        }) {
            return Some(reason.to_string());
        }
        self.build_scripts
            .iter()
            .find_map(|script| script.revalidation_reason(source_root))
            .map(str::to_string)
    }
}
