//! Embedded workload materialization, with frozen registry resolution.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{RailError, RailResult};

include!(concat!(env!("OUT_DIR"), "/benchmark-workload.rs"));

pub(super) fn materialize(destination: &Path, git_source: &Path, offline: bool) -> RailResult<PathBuf> {
    let destination = new_path(destination)?;
    let git_source = selected_path(git_source)?;
    if destination == git_source || destination.starts_with(&git_source) || git_source.starts_with(&destination) {
        return Err(RailError::message(
            "workload and Git source directories must be disjoint",
        ));
    }
    checked(Command::new("git").arg("--version"))?;
    checked(Command::new("cargo").arg("--version"))?;
    let lock = template("Cargo.lock")?;
    let revision = lock
        .split_once("?rev=")
        .and_then(|(_, tail)| tail.split_once('#').map(|(revision, _)| revision))
        .filter(|revision| revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| RailError::message("bundled workload lockfile has no exact Git revision"))?;
    if git_source.try_exists()? {
        verify_git_source(&git_source, revision)?;
    }
    private_directory(&destination)?;
    if !git_source.try_exists()? {
        private_directory(&git_source)?;
        for (name, bytes) in FILES {
            if let Some(relative) = name.strip_prefix("git-source/") {
                write_new(&git_source.join(relative), bytes)?;
            }
        }
        initialize_git(
            &git_source,
            "2000-01-01T00:00:00Z",
            "Create native-cache Git dependency",
        )?;
    }
    verify_git_source(&git_source, revision)?;
    let url = file_url(&git_source)?;
    for (name, bytes) in FILES {
        if name.starts_with("git-source/") || name.starts_with("git-prefetch/") {
            continue;
        }
        let path = destination.join(name);
        if *name == "Cargo.toml" || *name == "Cargo.lock" {
            let rendered = template(name)?
                .replace("__FIXTURE_GIT_URL__", &url)
                .replace("__FIXTURE_GIT_REV__", revision);
            write_new(&path, rendered.as_bytes())?;
        } else {
            write_new(&path, bytes)?;
        }
    }
    // Cargo's offline Git resolver requires a local checkout even for file URLs.
    // This manifest has only the bundled Git dependency, so its fetch cannot resolve registry inputs.
    let prefetch = tempfile::Builder::new()
        .prefix(".git-prefetch-")
        .tempdir_in(&destination)?;
    for (name, bytes) in FILES {
        if let Some(relative) = name.strip_prefix("git-prefetch/") {
            let path = prefetch.path().join(relative);
            if relative == "Cargo.toml" || relative == "Cargo.lock" {
                let rendered = template(name)?
                    .replace("__FIXTURE_GIT_URL__", &url)
                    .replace("__FIXTURE_GIT_REV__", revision);
                write_new(&path, rendered.as_bytes())?;
            } else {
                write_new(&path, bytes)?;
            }
        }
    }
    verify_git_source(&git_source, revision)?;
    checked(
        Command::new("cargo")
            .current_dir(prefetch.path())
            .args(["fetch", "--locked"]),
    )?;
    prefetch.close()?;
    let lock_before = fs::read(destination.join("Cargo.lock"))?;
    let mut fetch = Command::new("cargo");
    fetch.current_dir(&destination).args(["fetch", "--locked"]);
    if offline {
        fetch.arg("--offline");
    }
    checked(&mut fetch)?;
    if fs::read(destination.join("Cargo.lock"))? != lock_before {
        return Err(RailError::message("Cargo changed the frozen workload lockfile"));
    }
    initialize_git(
        &destination,
        "2000-01-02T00:00:00Z",
        "Materialize native-cache workload",
    )?;
    Ok(destination)
}

fn template(name: &str) -> RailResult<&'static str> {
    let bytes = FILES
        .iter()
        .find_map(|(path, bytes)| (*path == name).then_some(*bytes))
        .ok_or_else(|| RailError::message(format!("missing bundled workload input: {name}")))?;
    std::str::from_utf8(bytes)
        .map_err(|error| RailError::message(format!("invalid bundled workload text {name}: {error}")))
}

fn selected_path(path: &Path) -> RailResult<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| RailError::message("workload path has no parent"))?;
    let name = absolute
        .file_name()
        .ok_or_else(|| RailError::message("workload path has no directory name"))?;
    let selected = crate::utils::canonicalize_existing(parent)?.join(name);
    if fs::symlink_metadata(&selected).is_ok_and(|metadata| crate::utils::is_symlink_or_reparse(&metadata)) {
        return Err(RailError::message(format!(
            "workload path is a symlink: {}",
            selected.display()
        )));
    }
    Ok(selected)
}

pub(super) fn new_path(path: &Path) -> RailResult<PathBuf> {
    let path = selected_path(path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(error.into()),
        Ok(_) => Err(RailError::message(format!(
            "workload output already exists: {}",
            path.display()
        ))),
    }
}

pub(super) fn private_directory(path: &Path) -> RailResult<()> {
    require_canonical_parent(path)?;
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path)?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> RailResult<()> {
    use std::io::Write as _;
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("bundled input has no parent"))?;
    require_canonical_parent(path)?;
    fs::create_dir_all(parent)?;
    require_canonical_parent(path)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn require_canonical_parent(path: &Path) -> RailResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("workload path has no parent"))?;
    if crate::utils::canonicalize_allow_missing(parent)? != parent {
        return Err(RailError::message(format!(
            "workload parent was redirected: {}",
            parent.display()
        )));
    }
    Ok(())
}

fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(name);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.filemode=false",
            "-c",
            "commit.gpgSign=false",
        ]);
    command
}

fn initialize_git(root: &Path, date: &str, message: &str) -> RailResult<()> {
    checked(git(root).args([
        "init",
        "--quiet",
        "--template=",
        "--initial-branch=main",
        "--object-format=sha1",
    ]))?;
    checked(git(root).args(["config", "core.autocrlf", "false"]))?;
    checked(git(root).args(["config", "core.filemode", "false"]))?;
    checked(git(root).args(["add", "--all"]))?;
    let tree = checked(git(root).arg("write-tree"))?;
    let tree = String::from_utf8_lossy(&tree.stdout);
    let commit = checked(
        git(root)
            .args(["commit-tree", tree.trim(), "-m", message])
            .env("GIT_AUTHOR_NAME", "cargo-rail fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@cargo-rail.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_NAME", "cargo-rail fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@cargo-rail.invalid")
            .env("GIT_COMMITTER_DATE", date),
    )?;
    checked(git(root).args([
        "update-ref",
        "refs/heads/main",
        String::from_utf8_lossy(&commit.stdout).trim(),
    ]))?;
    Ok(())
}

fn verify_git_source(root: &Path, revision: &str) -> RailResult<()> {
    for directory in [root.to_path_buf(), root.join(".git")] {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::message(
                "shared workload Git source is not one real repository",
            ));
        }
    }
    let head = checked(git(root).args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
    if String::from_utf8_lossy(&head.stdout).trim() != revision {
        return Err(RailError::message(
            "shared workload Git source does not match the frozen revision",
        ));
    }
    Ok(())
}

fn file_url(path: &Path) -> RailResult<String> {
    use std::fmt::Write as _;
    let path = path
        .to_str()
        .ok_or_else(|| RailError::message("workload Git path is not UTF-8"))?;
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    if path.starts_with("//") {
        return Err(RailError::message("workload Git source must be on a local filesystem"));
    }
    let mut url = if path.starts_with('/') { "file://" } else { "file:///" }.to_string();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            write!(url, "%{byte:02X}").map_err(|error| RailError::message(error.to_string()))?;
        }
    }
    Ok(url)
}

fn checked(command: &mut Command) -> RailResult<Output> {
    if let Some(directory) = command.get_current_dir()
        && crate::utils::canonicalize_existing(directory)? != directory
    {
        return Err(RailError::message("workload process directory was redirected"));
    }
    let output = command.output().map_err(|error| {
        RailError::from(error).context(format!("failed to launch {}", command.get_program().to_string_lossy()))
    })?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "{} failed ({}): {}",
            command.get_program().to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

pub(super) fn identity() -> String {
    let mut framed = Vec::new();
    for (name, bytes) in FILES {
        framed.extend_from_slice(&(name.len() as u64).to_le_bytes());
        framed.extend_from_slice(name.as_bytes());
        framed.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        framed.extend_from_slice(bytes);
    }
    crate::source::ContentDigest::sha256(&framed).to_string()
}

pub(super) fn input_paths(workspace: &Path) -> Vec<PathBuf> {
    FILES
        .iter()
        .filter_map(|(name, _)| {
            if name.starts_with("git-prefetch/") {
                None
            } else if let Some(relative) = name.strip_prefix("git-source/") {
                Some(workspace.with_file_name("workspace.git-source").join(relative))
            } else {
                Some(workspace.join(name))
            }
        })
        .collect()
}
