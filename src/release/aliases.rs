//! Compare-and-swap promotion after the immutable forge release is verified.

use super::{
    review,
    state::{ReleaseState, StepStatus},
};
use crate::{
    error::{RailError, RailResult},
    git::SystemGit,
};
use std::path::Path;

pub(crate) fn reconcile(root: &Path, state: &mut ReleaseState, path: &Path) -> RailResult<()> {
    let git = SystemGit::open(root)?;
    for (package, alias) in state.intent.release_config.aliases.clone() {
        if !state.intent.alias_previous.contains_key(&package) {
            continue;
        }
        let index = state.crate_index(&package)?;
        let progress = &state.crates[index];
        if !progress.forge_publication.is_complete() || !state.tag_push.is_complete() {
            return Err(RailError::message(
                "alias promotion requires a verified immutable forge release",
            ));
        }
        let target = progress
            .tag_object
            .as_ref()
            .ok_or_else(|| RailError::message("alias has no immutable tag object"))?
            .id
            .clone();
        let reference = format!("refs/tags/{alias}");
        let actual = review::remote_head(&git, &reference)?;
        if actual.as_deref() != Some(&target) {
            if progress.alias.is_complete() || actual != state.intent.alias_previous[&package] {
                return Err(RailError::message(format!(
                    "release alias {alias} moved from its authorized prior object"
                )));
            }
            state.crates[index].alias.status = StepStatus::InProgress;
            state.crates[index].alias.object = Some(target.clone());
            state.save(path)?;
            let previous = state.intent.alias_previous[&package].as_deref().unwrap_or("");
            git.run_git_observable_with_env(
                &[
                    "push",
                    "--atomic",
                    &format!("--force-with-lease={reference}:{previous}"),
                    "origin",
                    &format!("{target}:{reference}"),
                ],
                super::publisher::RELEASE_PUSH_ENV,
            )?;
        }
        if review::remote_head(&git, &reference)?.as_deref() != Some(&target) {
            return Err(RailError::message(format!(
                "release alias {alias} promotion is not observable"
            )));
        }
        state.crates[index].alias.status = StepStatus::Complete;
        state.crates[index].alias.object = Some(target);
        state.save(path)?;
    }
    Ok(())
}
