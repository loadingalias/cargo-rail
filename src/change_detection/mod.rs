//! Internal change interpretation shared by release attribution and planning.

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use rustc_hash::FxHashSet;

mod classify;
pub(crate) mod semantic;

pub(crate) use classify::classify_path;

/// Return workspace crates with changed inputs that seed build/test work.
pub(crate) fn changed_code_crate_names(ctx: &WorkspaceContext, base: Option<&str>) -> RailResult<FxHashSet<String>> {
    let mut crates = FxHashSet::default();
    for repository_path in ctx.source_paths_since(base)? {
        let Some(workspace_path) = ctx.to_workspace_path(&repository_path) else {
            continue;
        };
        if !classify_path(&workspace_path).seeds_build_test_transitive() {
            continue;
        }
        if let Some(owner) = ctx.graph().file_to_crate(&workspace_path) {
            crates.insert(owner);
        }
    }
    Ok(crates)
}
