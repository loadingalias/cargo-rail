//! Retained Cargo archives and checksum-bound publication.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};
use crate::release::planner::ReleasePlan;
use crate::release::registry::{RegistryObservation, RegistryObserver, valid_checksum, valid_name};
use crate::source::ContentDigest;
use crate::workspace::WorkspaceContext;

const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const INDEX: &str = "https://github.com/rust-lang/crates.io-index";
const SPARSE_INDEX: &str = "https://index.crates.io/";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationAttempt {
    pub identity: String,
    pub registry: String,
    pub name: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageSeal {
    pub source_commit: String,
    pub cargo_version: String,
    pub registry_index: String,
    pub packages: Vec<PackageArchive>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageArchive {
    pub name: String,
    pub version: String,
    pub bytes: u64,
    pub sha256: String,
}

impl PackageArchive {
    pub(crate) fn filename(&self) -> String {
        format!("{}-{}.crate", self.name, self.version)
    }

    pub(crate) fn restore(&self, source: &Path, destination: &Path) -> RailResult<()> {
        let bytes = self.read_verified(source)?;
        if destination.join(self.filename()).try_exists()? {
            self.read_verified(destination)?;
            return Ok(());
        }
        crate::utils::write_file_atomic(&destination.join(self.filename()), &bytes)
    }

    pub(crate) fn read_verified(&self, directory: &Path) -> RailResult<Vec<u8>> {
        validate_directory(directory)?;
        let bytes = read_archive(&directory.join(self.filename()))?;
        if bytes.len() as u64 != self.bytes || digest(&bytes) != self.sha256 {
            return Err(RailError::message("retained package differs from sealed evidence"));
        }
        Ok(bytes)
    }

    pub(crate) fn attempt_path(&self, directory: &Path) -> PathBuf {
        directory
            .join("attempts")
            .join(format!("{}@{}.json", self.name, self.version))
    }

    pub(crate) fn attempted(&self, directory: &Path) -> RailResult<Option<String>> {
        let path = self.attempt_path(directory);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > 4096 {
            return Err(RailError::message("original upload attempt evidence is invalid"));
        }
        let file = fs::File::open(&path)?;
        if !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
            return Err(RailError::message("upload attempt changed while opening"));
        }
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        let attempt: PublicationAttempt = super::contract::decode(&bytes)?;
        if bytes.len() > 4096
            || attempt.registry != INDEX
            || attempt.name != self.name
            || attempt.version != self.version
            || attempt.sha256 != self.sha256
            || !valid_checksum(&attempt.identity)
        {
            return Err(RailError::message("upload attempt does not match the sealed package"));
        }
        Ok(Some(attempt.identity))
    }
}

impl PackageSeal {
    pub(crate) fn validate(&self, plan: &ReleasePlan, source: &str) -> RailResult<()> {
        let selected = plan.crates.iter().filter(|package| package.publish).collect::<Vec<_>>();
        if self.source_commit != source
            || self.registry_index != INDEX
            || self.cargo_version.is_empty()
            || self.packages.len() != selected.len()
        {
            return Err(RailError::message(
                "package evidence does not match the prepared release",
            ));
        }
        for (archive, package) in self.packages.iter().zip(selected) {
            if archive.name != package.name
                || archive.version != package.new_version.to_string()
                || !valid_name(&archive.name)
                || !valid_checksum(&archive.sha256)
                || archive.bytes == 0
                || archive.bytes > MAX_PACKAGE_BYTES
            {
                return Err(RailError::message(
                    "package evidence contains an invalid or mismatched archive",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn verify_archives(&self, directory: &Path) -> RailResult<()> {
        validate_directory(directory)?;
        validate_directory(&directory.join("attempts"))?;
        for archive in &self.packages {
            let bytes = read_archive(&directory.join(archive.filename()))?;
            if bytes.len() as u64 != archive.bytes || digest(&bytes) != archive.sha256 {
                return Err(RailError::message(format!(
                    "sealed archive for '{}' is missing or changed; recover the original evidence",
                    archive.name
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn observe(&self) -> RailResult<Vec<RegistryObservation>> {
        let observer = RegistryObserver::new(SPARSE_INDEX)?;
        Ok(self
            .packages
            .iter()
            .map(|archive| observer.observe(&archive.name, &archive.version, &archive.sha256))
            .collect())
    }
}

pub(crate) fn directory(state_path: &Path) -> PathBuf {
    state_path.with_extension("artifacts")
}

pub(crate) fn prepare(
    ctx: &WorkspaceContext,
    plan: &ReleasePlan,
    source: &str,
    directory: &Path,
) -> RailResult<PackageSeal> {
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_directory(directory)?;
    let selected = plan.crates.iter().filter(|package| package.publish).collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(RailError::message("cannot seal an empty registry publication"));
    }
    let build = tempfile::tempdir()?;
    let mut command = Command::new(ctx.snapshot()?.toolchain().cargo_program());
    command
        .current_dir(ctx.workspace_root())
        .args(["package", "--locked", "--registry", "crates-io", "--target-dir"])
        .arg(build.path());
    for package in &selected {
        command.args(["--package", &package.name]);
    }
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name == "CARGO_RAIL_RELEASE_TOKEN"
            || name == "CARGO_REGISTRY_TOKEN"
            || name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN")
        {
            command.env_remove(key);
        }
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "Cargo could not validate the complete release package set: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut packages = Vec::new();
    for package in selected {
        let filename = format!("{}-{}.crate", package.name, package.new_version);
        let bytes = read_archive(&build.path().join("package").join(&filename))?;
        let archive = PackageArchive {
            name: package.name.clone(),
            version: package.new_version.to_string(),
            bytes: bytes.len() as u64,
            sha256: digest(&bytes),
        };
        crate::utils::write_file_atomic(&directory.join(filename), &bytes)?;
        packages.push(archive);
    }
    let attempts = directory.join("attempts");
    match fs::create_dir(&attempts) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_directory(&attempts)?;
    let seal = PackageSeal {
        source_commit: source.to_owned(),
        cargo_version: ctx.snapshot()?.toolchain().cargo_verbose_version().to_owned(),
        registry_index: INDEX.to_owned(),
        packages,
    };
    seal.validate(plan, source)?;
    Ok(seal)
}

pub(crate) fn publish(
    ctx: &WorkspaceContext,
    seal: &PackageSeal,
    directory: &Path,
    remaining: &[&str],
) -> RailResult<Output> {
    seal.verify_archives(directory)?;
    if ctx.snapshot()?.toolchain().cargo_verbose_version() != seal.cargo_version {
        return Err(RailError::message(
            "publication requires the Cargo version bound in package evidence",
        ));
    }
    let isolated = tempfile::tempdir()?;
    for parent in isolated.path().ancestors().skip(1) {
        if parent.join(".cargo/config").exists() || parent.join(".cargo/config.toml").exists() {
            return Err(RailError::message(
                "isolated release execution found an ancestor Cargo configuration",
            ));
        }
    }
    let cargo_home = isolated.path().join("cargo-home");
    fs::create_dir(&cargo_home)?;
    let executable = std::env::current_exe()?;
    let mut provider = vec![
        executable.to_string_lossy().into_owned(),
        "cargo-rail-sealed-publish-v1".into(),
        INDEX.into(),
    ];
    for archive in &seal.packages {
        if remaining.contains(&archive.name.as_str()) {
            provider.extend([archive.name.clone(), archive.version.clone(), archive.sha256.clone()]);
        }
    }
    let config = format!(
        "[registry]\ncredential-provider = {}\n",
        serde_json::to_string(&provider)?
    );
    fs::write(cargo_home.join("config.toml"), config)?;
    let mut command = Command::new(ctx.snapshot()?.toolchain().cargo_program());
    command
        .current_dir(isolated.path())
        .args([
            "publish",
            "--locked",
            "--no-verify",
            "--registry",
            "crates-io",
            "--manifest-path",
        ])
        .arg(ctx.workspace_root().join("Cargo.toml"));
    for package in remaining {
        command.args(["--package", package]);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_REGISTR") {
            command.env_remove(key);
        }
    }
    command
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_RAIL_RELEASE_ATTEMPT_DIRECTORY", directory.join("attempts"))
        .env("CARGO_RAIL_RELEASE_STATE_PATH", directory.with_extension("json"))
        .env(
            "CARGO_RAIL_RELEASE_CARGO_HOME",
            crate::cargo::CargoConfigSnapshot::cargo_home(ctx.workspace_root())?,
        );
    if std::env::var_os("CARGO_RAIL_RELEASE_TOKEN").is_none()
        && std::env::var_os("CARGO_RAIL_RELEASE_CREDENTIAL_PROVIDER").is_none()
        && let Some(token) = std::env::var_os("CARGO_REGISTRY_TOKEN")
    {
        command.env("CARGO_RAIL_RELEASE_TOKEN", token);
    }
    command.output().map_err(Into::into)
}

fn read_archive(path: &Path) -> RailResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PACKAGE_BYTES {
        return Err(RailError::message("sealed package must be a bounded regular archive"));
    }
    let file = fs::File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "sealed archive is not a private unchanged regular file",
        ));
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_PACKAGE_BYTES + 1).read_to_end(&mut bytes)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message("sealed archive changed while reading"));
    }
    if bytes.len() as u64 != metadata.len() {
        return Err(RailError::message("sealed package changed while reading"));
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    ContentDigest::sha256(bytes).to_string()
}

fn validate_directory(path: &Path) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message("release artifact storage must be a real directory"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("release artifact storage has no parent"))?;
    let expected = crate::utils::canonicalize_existing(parent)?.join(
        path.file_name()
            .ok_or_else(|| RailError::message("release artifact storage has no name"))?,
    );
    if crate::utils::canonicalize_existing(path)? != expected {
        return Err(RailError::message("release artifact storage changed its containment"));
    }
    Ok(())
}
