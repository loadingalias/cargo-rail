//! Retained annotated tag objects, including their original signatures.

use std::io::Write as _;
use std::path::Path;

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;

use super::state::{ReleaseState, StepStatus, TagObject};

pub(crate) fn reconcile(git: &SystemGit, state: &mut ReleaseState, path: &Path) -> RailResult<()> {
    if state.intent.skip_tag {
        return Ok(());
    }
    let source = state
        .release_commit()
        .ok_or_else(|| RailError::message("release tag requires a prepared commit"))?
        .to_owned();
    for index in 0..state.crates.len() {
        let package = &state.intent.plan.crates[index];
        let reference = format!("refs/tags/{}", package.tag_name);
        if let Some(retained) = &state.crates[index].tag_object {
            restore(git, &reference, retained)?;
            verify_signature(git, &reference, state.intent.release_config.sign_tags)?;
        } else {
            state.crates[index].tag.status = StepStatus::InProgress;
            state.crates[index].tag.object = Some(source.clone());
            state.save(path)?;
            if !git.run_git_check(&["show-ref", "--verify", "--quiet", &reference]) {
                git.create_tag(
                    &package.tag_name,
                    Some(&format!("Release {} v{}", package.name, package.new_version)),
                    state.intent.release_config.sign_tags,
                )?;
            }
            let id = git.run_git_stdout(&["rev-parse", "--verify", &reference])?;
            let content = String::from_utf8(git.run_git(&["cat-file", "tag", &id])?.stdout)
                .map_err(|_| RailError::message("release tag object is not UTF-8"))?;
            verify_signature(git, &reference, state.intent.release_config.sign_tags)?;
            state.crates[index].tag_object = Some(TagObject {
                id: id.clone(),
                content,
            });
            state.crates[index].tag.status = StepStatus::Complete;
            state.crates[index].tag.object = Some(id);
            state.save(path)?;
        }
    }
    Ok(())
}

fn restore(git: &SystemGit, reference: &str, retained: &TagObject) -> RailResult<()> {
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(retained.content.as_bytes())?;
    let filename = file
        .path()
        .to_str()
        .ok_or_else(|| RailError::message("tag object path is not UTF-8"))?;
    let observed = git.run_git_stdout(&["hash-object", "-t", "tag", "--", filename])?;
    if observed != retained.id {
        return Err(RailError::message(
            "retained tag bytes do not match their Git object identity",
        ));
    }
    if git.run_git_check(&["show-ref", "--verify", "--quiet", reference]) {
        let actual = git.run_git_stdout(&["rev-parse", "--verify", reference])?;
        if actual != retained.id {
            return Err(RailError::message(
                "local release tag conflicts with its retained object",
            ));
        }
    } else {
        git.run_git_stdout(&["hash-object", "-t", "tag", "-w", "--", filename])?;
        git.run_git(&["update-ref", reference, &retained.id, ""])?;
    }
    Ok(())
}

fn verify_signature(git: &SystemGit, reference: &str, required: bool) -> RailResult<()> {
    if required {
        git.run_git(&["verify-tag", reference])?;
    }
    Ok(())
}
