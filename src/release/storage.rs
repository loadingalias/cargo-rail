//! Git-backed execution records with one atomic active-release lease.

use std::collections::BTreeMap;
use std::io::{Seek as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;

use super::contract;
use super::state::{ReleaseState, ReleaseStatus};

pub(super) const ACTIVE: &str = "refs/notes/cargo-rail/active";

pub(crate) fn fetch(root: &Path, transaction: Option<&str>) -> RailResult<(ReleaseState, tempfile::TempDir)> {
    let reference = transaction
        .map(reference)
        .transpose()?
        .unwrap_or_else(|| ACTIVE.to_owned());
    let (head, state) =
        read(root, &reference)?.ok_or_else(|| RailError::message("no retained remote release record was found"))?;
    validate_repository(root, &state)?;
    if transaction.is_some_and(|expected| expected != state.transaction_id) {
        return Err(RailError::message(
            "remote release record has another transaction identity",
        ));
    }
    let retained = tempfile::tempdir()?;
    std::fs::create_dir(retained.path().join("attempts"))?;
    if let Some(seal) = &state.package_seal {
        let git = SystemGit::open(root)?;
        let blobs = package_blobs(&git, &head, &state)?;
        for package in &seal.packages {
            let blob = &blobs[&package.filename()];
            let output = git.run_git(&["cat-file", "blob", blob])?;
            std::fs::write(retained.path().join(package.filename()), output.stdout)?;
        }
        seal.verify_archives(retained.path())?;
    }
    Ok((state, retained))
}

pub(crate) fn store(root: &Path, state: &ReleaseState) -> RailResult<()> {
    state.validate_contract()?;
    validate_repository(root, state)?;
    let reference = reference(&state.transaction_id)?;
    let previous = read(root, &reference)?;
    let active = read(root, ACTIVE)?;
    if let Some((_, active)) = &active
        && active.status == ReleaseStatus::Active
        && active.transaction_id != state.transaction_id
    {
        return Err(RailError::message(format!(
            "remote release transaction '{}' is already active",
            active.transaction_id
        )));
    }
    let bytes = contract::canonical(serde_json::to_value(state)?)?;
    if let Some((previous_head, previous)) = &previous {
        super::state::require_successor(previous, state)?;
        if contract::canonical(serde_json::to_value(previous)?)? == bytes
            && active.as_ref().is_some_and(|(head, _)| head == previous_head)
        {
            return Ok(());
        }
    }
    let blob = git_input(root, &["hash-object", "-w", "--stdin"], &bytes)?;
    let mut entries = format!("100644 blob {blob}\trecord.json\n");
    if let Some(seal) = &state.package_seal {
        let directory =
            super::packages::directory(&super::state::state_dir(root).join(format!("{}.json", state.transaction_id)));
        seal.verify_archives(&directory)?;
        let mut packages = String::new();
        for package in &seal.packages {
            let bytes = package.read_verified(&directory)?;
            let blob = git_input(root, &["hash-object", "-w", "--stdin"], &bytes)?;
            packages.push_str(&format!("100644 blob {blob}\t{}\n", package.filename()));
        }
        let tree = git_input(root, &["mktree"], packages.as_bytes())?;
        entries.push_str(&format!("040000 tree {tree}\tpackages\n"));
    }
    let tree = git_input(root, &["mktree"], entries.as_bytes())?;
    let source = state.release_commit().unwrap_or(&state.intent.initial_head);
    let mut arguments = vec!["commit-tree", tree.as_str(), "-p", source];
    if let Some((parent, _)) = &previous {
        arguments.extend(["-p", parent]);
    }
    let commit = git_input(
        root,
        &arguments,
        format!("Retain release {}\n", state.transaction_id).as_bytes(),
    )?;
    let expected = previous.as_ref().map_or("", |(head, _)| head.as_str());
    let active_expected = active.as_ref().map_or("", |(head, _)| head.as_str());
    let git = SystemGit::open(root)?;
    // The record and active pointer move together. A failed acknowledgment is
    // reconciled by comparing the next fetched record, never by blind force.
    git.run_git_observable_with_env(
        &[
            "push",
            "--atomic",
            &format!("--force-with-lease={reference}:{expected}"),
            &format!("--force-with-lease={ACTIVE}:{active_expected}"),
            "origin",
            &format!("{commit}:{reference}"),
            &format!("{commit}:{ACTIVE}"),
        ],
        super::publisher::RELEASE_PUSH_ENV,
    )?;
    Ok(())
}

fn read(root: &Path, reference: &str) -> RailResult<Option<(String, ReleaseState)>> {
    let git = SystemGit::open(root)?;
    let listing = git.run_git_stdout(&["ls-remote", "--refs", "origin", reference])?;
    if listing.is_empty() {
        return Ok(None);
    }
    let rows = listing.lines().collect::<Vec<_>>();
    let (head, actual_ref) = rows
        .first()
        .and_then(|row| row.split_once('\t'))
        .ok_or_else(|| RailError::message("remote release reference is malformed"))?;
    if rows.len() != 1 || actual_ref != reference || !oid(head) {
        return Err(RailError::message("remote release reference is ambiguous"));
    }
    git.run_git(&["fetch", "--no-tags", "--no-write-fetch-head", "origin", head])?;
    let tree = tree_entries(&git, head)?;
    let blob = tree
        .get("record.json")
        .and_then(|entry| entry.strip_prefix("100644 blob "))
        .filter(|value| oid(value))
        .ok_or_else(|| RailError::message("remote release record must be a regular blob"))?;
    let size = git
        .run_git_stdout(&["cat-file", "-s", blob])?
        .parse::<usize>()
        .map_err(|_| RailError::message("remote release record size is invalid"))?;
    if size > contract::MAX_RECORD_BYTES {
        return Err(RailError::message("remote release record exceeds its size limit"));
    }
    let bytes = git.run_git_stdout(&["cat-file", "blob", blob])?;
    let state: ReleaseState = contract::decode(bytes.as_bytes())?;
    state.validate_contract()?;
    if !state.remote_storage {
        return Err(RailError::message(
            "remote record does not retain remote execution authority",
        ));
    }
    package_blobs(&git, head, &state)?;
    Ok(Some((head.to_owned(), state)))
}

fn tree_entries(git: &SystemGit, object: &str) -> RailResult<BTreeMap<String, String>> {
    let output = git.run_git(&["ls-tree", "-z", object])?;
    if output.stdout.len() > contract::MAX_RECORD_BYTES {
        return Err(RailError::message("release record tree exceeds its inventory bound"));
    }
    let mut entries = BTreeMap::new();
    for row in output.stdout.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
        let row = std::str::from_utf8(row).map_err(|_| RailError::message("release record tree is not UTF-8"))?;
        let (entry, name) = row
            .split_once('\t')
            .ok_or_else(|| RailError::message("release record tree is malformed"))?;
        if entries.insert(name.to_owned(), entry.to_owned()).is_some() {
            return Err(RailError::message("release record tree has duplicate entries"));
        }
    }
    Ok(entries)
}

fn package_blobs(git: &SystemGit, head: &str, state: &ReleaseState) -> RailResult<BTreeMap<String, String>> {
    let mut root = tree_entries(git, head)?;
    root.remove("record.json");
    let Some(seal) = &state.package_seal else {
        if !root.is_empty() {
            return Err(RailError::message("remote release record has unsealed package objects"));
        }
        return Ok(BTreeMap::new());
    };
    let tree = root
        .remove("packages")
        .and_then(|entry| entry.strip_prefix("040000 tree ").map(str::to_owned))
        .filter(|value| oid(value))
        .ok_or_else(|| RailError::message("remote release record has no retained package tree"))?;
    if !root.is_empty() {
        return Err(RailError::message("remote release record has unexpected entries"));
    }
    let mut entries = tree_entries(git, &tree)?;
    let mut blobs = BTreeMap::new();
    for package in &seal.packages {
        let blob = entries
            .remove(&package.filename())
            .and_then(|entry| entry.strip_prefix("100644 blob ").map(str::to_owned))
            .filter(|value| oid(value))
            .ok_or_else(|| RailError::message("remote release package is missing or not a regular blob"))?;
        let size = git
            .run_git_stdout(&["cat-file", "-s", &blob])?
            .parse::<u64>()
            .map_err(|_| RailError::message("remote release package size is invalid"))?;
        if size != package.bytes {
            return Err(RailError::message("remote release package has the wrong size"));
        }
        blobs.insert(package.filename(), blob);
    }
    if !entries.is_empty() {
        return Err(RailError::message("remote release package tree has extra entries"));
    }
    Ok(blobs)
}

fn validate_repository(root: &Path, state: &ReleaseState) -> RailResult<()> {
    let expected = state
        .intent
        .remote_repository
        .as_ref()
        .ok_or_else(|| RailError::message("remote record storage requires a bound release repository"))?;
    if &super::remote::release_repository(root)? != expected {
        return Err(RailError::message("remote release record repository changed"));
    }
    Ok(())
}

pub(super) fn reference(transaction: &str) -> RailResult<String> {
    if !transaction.starts_with("release-")
        || transaction.len() > 128
        || !transaction
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(RailError::message("invalid remote release transaction identity"));
    }
    Ok(format!("refs/notes/cargo-rail/{transaction}"))
}

fn git_input(root: &Path, arguments: &[&str], input: &[u8]) -> RailResult<String> {
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.rewind()?;
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "could not retain a release Git object: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let value =
        String::from_utf8(output.stdout).map_err(|_| RailError::message("release Git object identity is not UTF-8"))?;
    let value = value.trim();
    if !oid(value) {
        return Err(RailError::message("release Git object identity is invalid"));
    }
    Ok(value.to_owned())
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
