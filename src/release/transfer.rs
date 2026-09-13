//! Bounded record transport between authorized release executors.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::Path;

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;

use super::state::{ReleaseState, StepStatus, lock, state_dir};
use super::{contract, packages};

pub(crate) fn store(root: &Path, transaction: &str) -> RailResult<serde_json::Value> {
    let _lock = lock(root)?;
    let path = super::state::resolve_active(root, Some(transaction))?;
    let mut state = ReleaseState::load(&path)?;
    state.remote_storage = true;
    state.save(&path)?;
    Ok(summary(&state))
}

pub(crate) fn fetch(root: &Path, transaction: Option<&str>) -> RailResult<serde_json::Value> {
    let _lock = lock(root)?;
    let (state, retained) = super::storage::fetch(root, transaction)?;
    let path = state_dir(root).join(format!("{}.json", state.transaction_id));
    if path.try_exists()? {
        super::state::require_successor(&ReleaseState::load(&path)?, &state)?;
    }
    restore_packages(&state, retained.path(), &path)?;
    state.retain_local(&path)?;
    Ok(summary(&state))
}

pub(crate) fn export(root: &Path, transaction: &str, output: &Path) -> RailResult<serde_json::Value> {
    let _lock = lock(root)?;
    if !transaction.starts_with("release-")
        || !transaction
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(RailError::message("invalid release transaction identity"));
    }
    let path = state_dir(root).join(format!("{transaction}.json"));
    let mut state = ReleaseState::load_for_recovery(&path)?;
    let artifacts = packages::directory(&path);
    if let Some(seal) = &state.package_seal {
        seal.verify_archives(&artifacts)?;
        for archive in &seal.packages {
            if let Some(attempt) = archive.attempted(&artifacts)? {
                let index = state.crate_index(&archive.name)?;
                if state.crates[index].publication.status != StepStatus::Complete {
                    state.crates[index].publication.status = StepStatus::InProgress;
                    state.crates[index].publication.object = Some(archive.sha256.clone());
                    state.crates[index].publication_attempt = Some(attempt);
                }
            }
        }
    }
    state.save(&path)?;
    fs::create_dir(output)?;
    fs::create_dir(output.join("attempts"))?;
    if let Some(seal) = &state.package_seal {
        for archive in &seal.packages {
            archive.restore(&artifacts, output)?;
        }
        seal.verify_archives(output)?;
    }
    crate::utils::write_file_atomic(
        &output.join("record.json"),
        &contract::canonical(serde_json::to_value(&state)?)?,
    )?;
    Ok(summary(&state))
}

pub(crate) fn import(
    root: &Path,
    input: &Path,
    expected_intent: &str,
    expected_source: &str,
) -> RailResult<serde_json::Value> {
    let _lock = lock(root)?;
    require_directory(input)?;
    let record = input.join("record.json");
    let metadata = fs::symlink_metadata(&record)?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() > contract::MAX_RECORD_BYTES as u64
    {
        return Err(RailError::message("release transfer has no bounded regular record"));
    }
    let file = fs::File::open(&record)?;
    if !crate::utils::private_file_matches_path(&file, &record, metadata.len())? {
        return Err(RailError::message("release transfer record changed while opening"));
    }
    let mut bytes = Vec::new();
    file.take(contract::MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let state: ReleaseState = contract::decode(&bytes)?;
    state.validate_contract()?;
    if state.intent.identity != expected_intent {
        return Err(RailError::message(
            "release transfer does not match the authorized intent",
        ));
    }
    let source = state.release_commit().unwrap_or(&state.intent.initial_head);
    let git = SystemGit::open(root)?;
    if source != expected_source || git.head_commit()? != expected_source {
        return Err(RailError::message(
            "release transfer does not match the authorized checkout commit",
        ));
    }
    state.validate_preparation_binding(&git)?;
    let source_tree = git.run_git_stdout(&["show", "-s", "--format=%T", &state.intent.initial_head])?;
    if source_tree != state.intent.source_tree {
        return Err(RailError::message(
            "release transfer source tree does not match its intent",
        ));
    }
    if let Some(expected) = &state.intent.remote_repository
        && &super::remote::release_repository(root)? != expected
    {
        return Err(RailError::message("release transfer targets another repository"));
    }
    state.validate_recovery_paths(root)?;
    require_directory(&input.join("attempts"))?;
    if fs::read_dir(input.join("attempts"))?.next().is_some() {
        return Err(RailError::message(
            "transferred upload attempts must be bound in the release record",
        ));
    }
    let mut expected = BTreeSet::from(["record.json".to_owned(), "attempts".to_owned()]);
    if let Some(seal) = &state.package_seal {
        seal.verify_archives(input)?;
        expected.extend(seal.packages.iter().map(|archive| archive.filename()));
    }
    for entry in fs::read_dir(input)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("release transfer filename is not UTF-8"))?;
        if !expected.remove(&name) {
            return Err(RailError::message("release transfer contains an unexpected artifact"));
        }
    }
    if !expected.is_empty() {
        return Err(RailError::message("release transfer is missing a required artifact"));
    }
    let path = state_dir(root).join(format!("{}.json", state.transaction_id));
    if path.try_exists()? {
        let existing = ReleaseState::load(&path)?;
        if contract::canonical(serde_json::to_value(&existing)?)? != contract::canonical(serde_json::to_value(&state)?)?
        {
            return Err(RailError::message(
                "release transfer conflicts with existing execution progress",
            ));
        }
    }
    restore_packages(&state, input, &path)?;
    state.retain_local(&path)?;
    Ok(summary(&state))
}

fn restore_packages(state: &ReleaseState, input: &Path, path: &Path) -> RailResult<()> {
    let Some(seal) = &state.package_seal else {
        return Ok(());
    };
    let artifacts = packages::directory(path);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| RailError::message("release record has no parent"))?,
    )?;
    for directory in [&artifacts, &artifacts.join("attempts")] {
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        require_directory(directory)?;
    }
    for archive in &seal.packages {
        archive.restore(input, &artifacts)?;
    }
    seal.verify_archives(&artifacts)
}

fn require_directory(path: &Path) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message("release transfer requires a real directory"));
    }
    Ok(())
}

fn summary(state: &ReleaseState) -> serde_json::Value {
    serde_json::json!({
        "schema": "cargo-rail-release-transfer-v1",
        "transaction_id": state.transaction_id,
        "intent": state.intent.identity,
        "source": state.release_commit().unwrap_or(&state.intent.initial_head),
    })
}
