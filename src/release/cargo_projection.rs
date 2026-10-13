//! Cargo resolution of proposed releases in isolated captured source.

use crate::error::{RailError, RailResult};
use crate::release::planner::{CrateReleasePlan, PlannedCargoWrite};
use crate::release::version::{BumpType, VersionBumper};
use crate::source::{ContentDigest, SourceEntryKind};
use crate::workspace::WorkspaceContext;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct ProjectedWorkspace {
    manifest_path: PathBuf,
    lockfile_path: PathBuf,
    isolated_manifest: PathBuf,
    isolated_lockfile: PathBuf,
}

#[derive(serde::Deserialize)]
struct CargoMetadataPaths {
    packages: Vec<CargoMetadataPackage>,
}

#[derive(serde::Deserialize)]
struct CargoMetadataPackage {
    name: String,
    version: semver::Version,
    manifest_path: PathBuf,
    source: Option<String>,
    dependencies: Vec<CargoMetadataDependency>,
}

#[derive(serde::Deserialize)]
struct CargoMetadataDependency {
    path: Option<PathBuf>,
}

enum CargoMetadataScope {
    Resolved,
    Declarations,
}

pub(crate) fn plan_writes(
    ctx: &WorkspaceContext,
    crates: &[CrateReleasePlan],
    manifests: &[PathBuf],
) -> RailResult<Vec<PlannedCargoWrite>> {
    if crates.is_empty() {
        return Ok(Vec::new());
    }
    ctx.validate_snapshot_unchanged()?;
    let snapshot = ctx.snapshot()?;
    let staging = tempfile::Builder::new().prefix("cargo-rail-release-cargo-").tempdir()?;
    let source_root = staging.path().join("source");
    fs::create_dir(&source_root)?;
    let source_root = crate::utils::canonicalize_existing(&source_root)?;
    materialize_snapshot(ctx, &source_root)?;
    let workspace_prefix = crate::utils::path_relative_to(snapshot.source_root(), ctx.workspace_root())?;
    let workspace_root = source_root.join(workspace_prefix);
    let mut configured = manifests.to_vec();
    configured.sort_unstable();
    let mut planned_digests = BTreeMap::new();
    let mut resolved_locks = BTreeSet::from([PathBuf::from("Cargo.lock")]);
    let mut resolved = vec![ProjectedWorkspace {
        manifest_path: PathBuf::from("Cargo.toml"),
        lockfile_path: PathBuf::from("Cargo.lock"),
        isolated_manifest: workspace_root.join("Cargo.toml"),
        isolated_lockfile: workspace_root.join("Cargo.lock"),
    }];
    let mut local_manifests = BTreeSet::new();
    for manifest in configured {
        require_head_file(ctx, &manifest, "auxiliary Cargo manifest")?;
        let isolated_manifest = workspace_root.join(&manifest);
        let isolated_lock = resolve_lockfile(ctx, &workspace_root, &isolated_manifest)?;
        let lockfile = crate::utils::path_relative_to(&workspace_root, &isolated_lock)?;
        if lockfile == Path::new("Cargo.lock") {
            return Err(RailError::with_help(
                format!(
                    "auxiliary Cargo manifest '{}' resolves the primary workspace Cargo.lock",
                    manifest.display()
                ),
                "remove primary workspace members from release.auxiliary_cargo_manifests",
            ));
        }
        if !resolved_locks.insert(lockfile.clone()) {
            return Err(RailError::with_help(
                format!(
                    "more than one auxiliary Cargo manifest resolves lockfile '{}'",
                    lockfile.display()
                ),
                "declare one manifest for each standalone Cargo workspace lockfile",
            ));
        }
        require_head_file(ctx, &lockfile, "auxiliary Cargo lockfile")?;
        resolved.push(ProjectedWorkspace {
            manifest_path: manifest,
            lockfile_path: lockfile,
            isolated_manifest,
            isolated_lockfile: isolated_lock,
        });
    }
    for workspace in &resolved {
        local_manifests.extend(
            validate_local_package_manifests(
                ctx,
                snapshot,
                &source_root,
                &workspace_root,
                &workspace.isolated_manifest,
                &workspace.manifest_path,
            )?
            .into_keys(),
        );
        // A virtual workspace manifest need not appear in Cargo's package list.
        local_manifests.insert(workspace.isolated_manifest.clone());
    }
    validate_auxiliary_workspace_coverage(ctx, &source_root, &workspace_root, crates, &resolved)?;
    audit_isolated_tree(snapshot, &source_root, &planned_digests)?;
    for plan in crates {
        let manifest = isolated_path(ctx, &workspace_root, &plan.manifest_path)?;
        VersionBumper::bump_version(&manifest, BumpType::Exact(plan.new_version.clone()))?;
        bind_planned_digest(&source_root, &manifest, &mut planned_digests)?;
    }
    for manifest in local_manifests {
        if update_local_requirements(ctx, &workspace_root, &manifest, crates)? {
            bind_planned_digest(&source_root, &manifest, &mut planned_digests)?;
        }
    }
    for workspace in resolved {
        let before = read_isolated_lockfile(&workspace.isolated_lockfile, &workspace.lockfile_path)?;
        let specs = local_release_package_specs(&before, crates, &workspace.lockfile_path)?;
        if !specs.is_empty() {
            run_cargo_update(ctx, &workspace_root, &workspace.isolated_manifest, &specs)?;
        }
        validate_local_package_manifests(
            ctx,
            snapshot,
            &source_root,
            &workspace_root,
            &workspace.isolated_manifest,
            &workspace.manifest_path,
        )?;
        let after = read_isolated_lockfile(&workspace.isolated_lockfile, &workspace.lockfile_path)?;
        if after != before {
            bind_planned_digest(&source_root, &workspace.isolated_lockfile, &mut planned_digests)?;
        }
    }
    audit_isolated_tree(snapshot, &source_root, &planned_digests)?;
    let mut writes = Vec::new();
    let mut inputs = projection_inputs(snapshot)?;
    for relative in planned_digests.keys() {
        let entry = snapshot
            .source()
            .tree()
            .entries()
            .iter()
            .find(|entry| entry.path.as_path() == relative);
        let before = if let Some(bytes) = inputs.remove(relative) {
            Some(bytes)
        } else {
            entry
                .map(|entry| snapshot.read_source_file(&entry.path))
                .transpose()?
                .flatten()
        };
        let after = fs::read(source_root.join(relative))?;
        if before.as_deref() == Some(after.as_slice()) {
            continue;
        }
        writes.push(PlannedCargoWrite {
            path: portable_contract_path(&crate::utils::path_relative_to(
                &workspace_root,
                &source_root.join(relative),
            )?),
            before_digest: before.as_deref().map(digest),
            after_digest: digest(&after),
            content: String::from_utf8(after).map_err(|error| {
                RailError::message(format!("Cargo file '{}' is not UTF-8: {error}", relative.display()))
            })?,
        });
    }
    writes.sort_by_cached_key(|write| crate::utils::path_to_git_format(&write.path));
    ctx.validate_snapshot_unchanged()?;
    Ok(writes)
}

fn validate_auxiliary_workspace_coverage(
    ctx: &WorkspaceContext,
    source_root: &Path,
    workspace_root: &Path,
    crates: &[CrateReleasePlan],
    declared: &[ProjectedWorkspace],
) -> RailResult<()> {
    let snapshot = ctx.snapshot()?;
    let capture = ctx
        .source_capture()
        .ok_or_else(|| RailError::message("release lockfile discovery requires captured Git source"))?;
    let released_manifests = crates
        .iter()
        .map(|package| {
            isolated_path(ctx, workspace_root, &package.manifest_path)
                .and_then(|path| crate::utils::canonicalize_existing(&path).map_err(Into::into))
        })
        .collect::<RailResult<BTreeSet<_>>>()?;
    for entry in snapshot.source().tree().entries() {
        if entry.path.as_path().file_name().is_none_or(|name| name != "Cargo.lock")
            || !matches!(entry.kind, SourceEntryKind::RegularFile { .. })
            || capture.is_untracked(&entry.path)
        {
            continue;
        }
        let lockfile = source_root.join(entry.path.as_path());
        if declared.iter().any(|workspace| workspace.isolated_lockfile == lockfile) {
            continue;
        }
        let manifest_path = entry.path.as_path().with_file_name("Cargo.toml");
        let Some(manifest_entry) = snapshot
            .source()
            .tree()
            .entries()
            .iter()
            .find(|entry| entry.path.as_path() == manifest_path)
        else {
            continue;
        };
        if capture.is_untracked(&manifest_entry.path) {
            continue;
        }
        let bytes = snapshot
            .read_source_file(&entry.path)?
            .ok_or_else(|| RailError::message(format!("captured Cargo lockfile '{}' disappeared", entry.path)))?;
        let specs = local_release_package_specs(&bytes, crates, entry.path.as_path())?;
        if specs.is_empty() {
            continue;
        }
        let manifest = source_root.join(&manifest_path);
        if resolve_lockfile(ctx, workspace_root, &manifest)? != lockfile {
            // Cargo ignores a member's stale lockfile in favor of its workspace root.
            continue;
        }
        let local =
            validate_local_package_manifests(ctx, snapshot, source_root, workspace_root, &manifest, &manifest_path)?;
        let resolves_release = local.keys().any(|manifest| released_manifests.contains(manifest));
        let unattributed = crates.iter().any(|package| {
            specs.contains(&format!("{}@{}", package.name, package.current_version))
                && !local
                    .values()
                    .any(|local| (&local.name, &local.version) == (&package.name, &package.current_version))
        });
        if resolves_release || unattributed {
            let help = match manifest.strip_prefix(workspace_root) {
                Ok(relative) => format!(
                    "add '{}' to release.auxiliary_cargo_manifests before planning the release",
                    relative.display()
                ),
                Err(_) => format!(
                    "move independent Cargo workspace '{}' inside the primary workspace root and declare it in release.auxiliary_cargo_manifests",
                    manifest_path.display()
                ),
            };
            let message = if resolves_release {
                format!(
                    "tracked independent Cargo lockfile '{}' resolves a released path package but is not declared for release",
                    entry.path
                )
            } else {
                format!(
                    "tracked independent Cargo lockfile '{}' contains a local release-package identity that Cargo metadata could not attribute",
                    entry.path
                )
            };
            return Err(RailError::with_help(message, help));
        }
    }
    Ok(())
}

fn materialize_snapshot(ctx: &WorkspaceContext, destination: &Path) -> RailResult<()> {
    let snapshot = ctx.snapshot()?;
    for entry in snapshot.source().tree().entries() {
        let path = destination.join(entry.path.as_path());
        let parent = path
            .parent()
            .ok_or_else(|| RailError::message(format!("captured source path '{}' has no parent", entry.path)))?;
        fs::create_dir_all(parent)?;
        match &entry.kind {
            SourceEntryKind::RegularFile {
                executable: _executable,
                ..
            } => {
                let bytes = snapshot
                    .read_source_file(&entry.path)?
                    .ok_or_else(|| RailError::message(format!("captured source file '{}' disappeared", entry.path)))?;
                let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
                file.write_all(&bytes)?;
                #[cfg(unix)]
                if *_executable {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
                }
            }
            SourceEntryKind::Symlink { target } => {
                materialize_contained_symlink(destination, &path, target)?;
            }
            SourceEntryKind::Deleted => {
                return Err(RailError::message(format!(
                    "captured source contains invalid deleted entry '{}'",
                    entry.path
                )));
            }
        }
    }
    for (relative, bytes) in projection_inputs(snapshot)? {
        let path = destination.join(relative);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| RailError::message("captured Cargo input has no parent"))?,
        )?;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
    }
    Ok(())
}

fn projection_inputs(snapshot: &crate::workspace::WorkspaceSnapshot) -> RailResult<BTreeMap<PathBuf, Vec<u8>>> {
    let mut inputs = BTreeMap::new();
    if let Some(lockfile) = snapshot.lockfile() {
        inputs.insert(
            lockfile.file().path().as_path().to_path_buf(),
            lockfile.file().bytes().to_vec(),
        );
    }
    for config in snapshot.cargo_config().provenance() {
        if let Ok(relative) = config.path().strip_prefix(snapshot.source_root()) {
            inputs.insert(
                relative.to_path_buf(),
                toml_edit::ser::to_string(config.settings())?.into_bytes(),
            );
        }
    }
    Ok(inputs)
}

fn materialize_contained_symlink(root: &Path, path: &Path, target: &str) -> RailResult<()> {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return Err(RailError::with_help(
            format!("source symbolic link '{}' escapes release staging", path.display()),
            "replace absolute source links before planning auxiliary Cargo lockfiles",
        ));
    }
    let parent = path
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
        .ok_or_else(|| RailError::message("staged source symbolic link has no contained parent"))?;
    let mut depth = parent.components().count();
    for component in target_path.components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if depth > 0 => depth -= 1,
            std::path::Component::ParentDir | std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(RailError::with_help(
                    format!("source symbolic link '{}' escapes release staging", path.display()),
                    "replace escaping source links before planning auxiliary Cargo lockfiles",
                ));
            }
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).map_err(|error| {
        RailError::message(format!(
            "failed to materialize source symbolic link '{}': {error}",
            path.display()
        ))
    })?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, path).map_err(|error| {
        RailError::with_help(
            format!(
                "failed to materialize source symbolic link '{}': {error}",
                path.display()
            ),
            "enable symbolic-link creation or remove source links from the auxiliary Cargo workspace",
        )
    })?;
    Ok(())
}

fn update_local_requirements(
    ctx: &WorkspaceContext,
    root: &Path,
    manifest: &Path,
    crates: &[CrateReleasePlan],
) -> RailResult<bool> {
    let primary_manifest = crate::utils::canonicalize_existing(manifest)?
        == crate::utils::canonicalize_existing(&root.join("Cargo.toml"))?;
    let selected_names = crates.iter().map(|plan| plan.name.as_str()).collect::<BTreeSet<_>>();
    let versions = crates
        .iter()
        .map(|plan| {
            Ok((
                crate::utils::canonicalize_existing(&isolated_path(ctx, root, &plan.manifest_path)?)?,
                (
                    &plan.new_version,
                    primary_manifest
                        && plan
                            .affected_dependents
                            .iter()
                            .any(|name| selected_names.contains(name.as_str())),
                ),
            ))
        })
        .collect::<RailResult<BTreeMap<_, _>>>()?;
    let mut editor = crate::toml::editor::TomlEditor::open(manifest)?;
    let mut changed = false;
    let selected_member = versions.contains_key(&crate::utils::canonicalize_existing(manifest)?);
    update_dependency_tables(
        manifest,
        editor.doc_mut().as_table_mut(),
        &versions,
        selected_member,
        false,
        &mut changed,
    )?;
    if changed {
        editor.write()?;
    }
    Ok(changed)
}

fn update_dependency_tables(
    manifest: &Path,
    table: &mut toml_edit::Table,
    versions: &BTreeMap<PathBuf, (&semver::Version, bool)>,
    refresh_member: bool,
    refresh_shared: bool,
    changed: &mut bool,
) -> RailResult<()> {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get_mut(section).and_then(toml_edit::Item::as_table_like_mut) {
            for (key, dependency) in dependencies.iter_mut() {
                let Some(dependency) = dependency.as_table_like_mut() else {
                    continue;
                };
                let Some(path) = dependency.get("path").and_then(toml_edit::Item::as_str) else {
                    continue;
                };
                let target = crate::utils::canonicalize_existing(
                    &manifest
                        .parent()
                        .expect("manifest parent")
                        .join(path)
                        .join("Cargo.toml"),
                )?;
                let Some((version, selected_dependent)) = versions.get(&target) else {
                    continue;
                };
                let Some(requirement) = dependency.get("version").and_then(toml_edit::Item::as_str) else {
                    continue;
                };
                let replacement = retarget_requirement(
                    requirement,
                    version,
                    refresh_member || refresh_shared && *selected_dependent,
                )
                .map_err(|error| {
                    RailError::with_help(
                        format!(
                            "cannot update local dependency '{key}' in '{}': {error}",
                            manifest.display()
                        ),
                        "set a requirement compatible with the planned version, then regenerate the release plan",
                    )
                })?;
                if let Some(replacement) = replacement {
                    let value = dependency
                        .get_mut("version")
                        .and_then(toml_edit::Item::as_value_mut)
                        .ok_or_else(|| RailError::message("dependency version is not a TOML value"))?;
                    let decor = value.decor().clone();
                    *value = replacement.into();
                    *value.decor_mut() = decor;
                    *changed = true;
                }
            }
        }
    }
    if let Some(workspace) = table.get_mut("workspace").and_then(toml_edit::Item::as_table_mut) {
        // A shared table may serve a released dependent. Refresh that minimum
        // while leaving compatible requirements of unselected packages unchanged.
        update_dependency_tables(manifest, workspace, versions, refresh_member, true, changed)?;
    }
    if let Some(targets) = table.get_mut("target").and_then(toml_edit::Item::as_table_mut) {
        for (_, target) in targets.iter_mut() {
            if let Some(target) = target.as_table_mut() {
                update_dependency_tables(manifest, target, versions, refresh_member, refresh_shared, changed)?;
            }
        }
    }
    Ok(())
}

fn retarget_requirement(requirement: &str, version: &semver::Version, refresh: bool) -> RailResult<Option<String>> {
    let parsed = semver::VersionReq::parse(requirement).map_err(|error| RailError::message(error.to_string()))?;
    if parsed.matches(version) && !refresh {
        return Ok(None);
    }
    let [comparator] = parsed.comparators.as_slice() else {
        return Err(RailError::message(format!(
            "requirement '{requirement}' is a compound range"
        )));
    };
    if !matches!(comparator.op, semver::Op::Caret | semver::Op::Tilde | semver::Op::Exact) {
        return Err(RailError::message(format!(
            "requirement '{requirement}' cannot be retargeted without changing its constraint policy"
        )));
    }
    let original = requirement.trim();
    let prefix_len = original
        .find(|character: char| character.is_ascii_digit())
        .ok_or_else(|| RailError::message("requirement has no version"))?;
    let prefix = original
        .get(..prefix_len)
        .ok_or_else(|| RailError::message("invalid requirement prefix"))?;
    let replacement = format!("{prefix}{version}");
    if replacement == original {
        return Ok(None);
    }
    if !semver::VersionReq::parse(&replacement).is_ok_and(|requirement| requirement.matches(version)) {
        return Err(RailError::message(
            "retargeted requirement does not accept the planned version",
        ));
    }
    Ok(Some(replacement))
}

fn isolated_path(ctx: &WorkspaceContext, root: &Path, original: &Path) -> RailResult<PathBuf> {
    let relative = crate::utils::path_relative_to(ctx.workspace_root(), original).map_err(|error| {
        RailError::message(format!(
            "release manifest '{}' is outside workspace '{}': {error}",
            original.display(),
            ctx.workspace_root().display()
        ))
    })?;
    Ok(root.join(relative))
}

fn relative_isolated(root: &Path, path: &Path) -> RailResult<PathBuf> {
    crate::utils::path_relative_to(root, path).map_err(|error| {
        RailError::message(format!(
            "isolated release path '{}' escaped '{}': {error}",
            path.display(),
            root.display()
        ))
    })
}

fn portable_contract_path(path: &Path) -> PathBuf {
    PathBuf::from(crate::utils::path_to_git_format(path))
}

fn bind_planned_digest(
    source_root: &Path,
    path: &Path,
    planned_digests: &mut BTreeMap<PathBuf, ContentDigest>,
) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "planned release manifest '{}' is not a regular file",
            path.display()
        )));
    }
    planned_digests.insert(
        relative_isolated(source_root, path)?,
        ContentDigest::sha256(&fs::read(path)?),
    );
    Ok(())
}

fn resolve_lockfile(ctx: &WorkspaceContext, root: &Path, manifest: &Path) -> RailResult<PathBuf> {
    let output = cargo_command(ctx, root)?
        .args([
            "locate-project",
            "--workspace",
            "--message-format",
            "plain",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .map_err(|error| RailError::message(format!("failed to start Cargo workspace resolution: {error}")))?;
    require_success(output, "cargo locate-project")
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .map_err(|error| RailError::message(format!("Cargo workspace path is not UTF-8: {error}")))
        })
        .and_then(|workspace_manifest| {
            let workspace_manifest = PathBuf::from(workspace_manifest.trim());
            workspace_manifest
                .parent()
                .map(|parent| parent.join("Cargo.lock"))
                .ok_or_else(|| RailError::message("Cargo workspace manifest has no parent directory"))
        })
}

fn validate_local_package_manifests(
    ctx: &WorkspaceContext,
    snapshot: &crate::workspace::WorkspaceSnapshot,
    source_root: &Path,
    workspace_root: &Path,
    manifest: &Path,
    manifest_path: &Path,
) -> RailResult<BTreeMap<PathBuf, CargoMetadataPackage>> {
    let metadata = read_cargo_metadata(ctx, workspace_root, manifest, CargoMetadataScope::Resolved)?;
    let mut pending = BTreeMap::new();
    for package in metadata.packages.into_iter().filter(|package| package.source.is_none()) {
        let isolated = validate_local_manifest(snapshot, source_root, &package.manifest_path, manifest_path)?;
        pending.insert(isolated, package);
    }
    let mut packages = BTreeMap::new();
    while let Some((isolated, package)) = pending.pop_first() {
        for dependency in package
            .dependencies
            .iter()
            .filter_map(|dependency| dependency.path.as_ref())
        {
            let dependency =
                validate_local_manifest(snapshot, source_root, &dependency.join("Cargo.toml"), manifest_path)?;
            if dependency == isolated || packages.contains_key(&dependency) || pending.contains_key(&dependency) {
                continue;
            }
            // Cargo's declarations include inactive optional and target-specific paths.
            // --no-deps reads that package without resolving or creating its own lockfile.
            let metadata = read_cargo_metadata(ctx, workspace_root, &dependency, CargoMetadataScope::Declarations)?;
            let package = metadata
                .packages
                .into_iter()
                .filter(|package| package.source.is_none())
                .find(|package| {
                    crate::utils::canonicalize_existing(&package.manifest_path).is_ok_and(|path| path == dependency)
                })
                .ok_or_else(|| {
                    RailError::message(format!(
                        "Cargo metadata omitted local path package '{}'",
                        dependency.display()
                    ))
                })?;
            pending.insert(dependency, package);
        }
        packages.insert(isolated, package);
    }
    Ok(packages)
}

fn read_cargo_metadata(
    ctx: &WorkspaceContext,
    root: &Path,
    manifest: &Path,
    scope: CargoMetadataScope,
) -> RailResult<CargoMetadataPaths> {
    let mut command = cargo_command(ctx, root)?;
    command
        .args(["metadata", "--format-version", "1", "--locked", "--manifest-path"])
        .arg(manifest);
    if matches!(scope, CargoMetadataScope::Declarations) {
        command.arg("--no-deps");
    }
    let output = command
        .output()
        .map_err(|error| RailError::message(format!("failed to start auxiliary Cargo metadata: {error}")))?;
    let output = require_success(output, "cargo metadata --locked")?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        RailError::message(format!(
            "failed to parse Cargo metadata for auxiliary manifest '{}': {error}",
            manifest.display()
        ))
    })
}

fn validate_local_manifest(
    snapshot: &crate::workspace::WorkspaceSnapshot,
    source_root: &Path,
    package_manifest: &Path,
    manifest_path: &Path,
) -> RailResult<PathBuf> {
    let resolved = crate::utils::canonicalize_existing(package_manifest).map_err(|error| {
        RailError::message(format!(
            "failed to resolve local package manifest '{}' for auxiliary manifest '{}': {error}",
            package_manifest.display(),
            manifest_path.display()
        ))
    })?;
    let relative = crate::utils::path_relative_to(source_root, &resolved).map_err(|_| {
        RailError::with_help(
            format!(
                "auxiliary Cargo manifest '{}' resolves local package manifest '{}' outside the captured source",
                manifest_path.display(),
                package_manifest.display()
            ),
            "keep every path dependency inside the captured Git worktree",
        )
    })?;
    let captured_regular = snapshot
        .source()
        .tree()
        .entries()
        .iter()
        .any(|entry| entry.path.as_path() == relative && matches!(entry.kind, SourceEntryKind::RegularFile { .. }));
    if !captured_regular {
        return Err(RailError::with_help(
            format!(
                "local package manifest '{}' resolved by auxiliary Cargo manifest '{}' is not a captured regular file",
                package_manifest.display(),
                manifest_path.display()
            ),
            "commit a regular package manifest inside the captured Git worktree",
        ));
    }
    let isolated = source_root.join(&relative);
    let isolated_metadata = fs::symlink_metadata(&isolated).map_err(|error| {
            RailError::with_help(
                format!(
                    "local package manifest '{}' resolved by auxiliary Cargo manifest '{}' is not available in the isolated captured source: {error}",
                    package_manifest.display(),
                    manifest_path.display()
                ),
                "keep every path dependency inside the captured Git worktree",
            )
        })?;
    if !isolated_metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&isolated_metadata) {
        return Err(RailError::with_help(
            format!(
                "local package manifest '{}' resolved by auxiliary Cargo manifest '{}' is not a regular file in the isolated captured source",
                package_manifest.display(),
                manifest_path.display()
            ),
            "commit a regular package manifest inside the captured Git worktree",
        ));
    }
    Ok(isolated)
}

fn require_head_file(ctx: &WorkspaceContext, workspace_path: &Path, description: &str) -> RailResult<()> {
    let absolute = ctx.workspace_root().join(workspace_path);
    let metadata = fs::symlink_metadata(&absolute).map_err(|error| {
        head_file_mismatch(
            description,
            workspace_path,
            &format!("cannot inspect the worktree file: {error}"),
        )
    })?;
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(head_file_mismatch(
            description,
            workspace_path,
            "the worktree path is not a regular file",
        ));
    }
    let git = ctx.git()?.git();
    let repository_path = crate::utils::path_relative_to(&git.worktree_root, &absolute).map_err(|error| {
        RailError::message(format!(
            "{description} '{}' is outside Git worktree '{}': {error}",
            absolute.display(),
            git.worktree_root.display()
        ))
    })?;
    let repository_path = crate::utils::path_to_git_format(&repository_path);

    let head = git.run_git_at_worktree_root(&["ls-tree", "-z", "HEAD", "--", &repository_path])?;
    let (head_mode, head_object) = parse_head_entry(&head.stdout, &repository_path)
        .ok_or_else(|| head_file_mismatch(description, workspace_path, "HEAD has no matching regular-file entry"))?;
    let index = git.run_git_at_worktree_root(&[
        "--literal-pathspecs",
        "ls-files",
        "--stage",
        "-z",
        "--",
        &repository_path,
    ])?;
    let (index_mode, index_object) = parse_index_entry(&index.stdout, &repository_path).ok_or_else(|| {
        head_file_mismatch(
            description,
            workspace_path,
            "the index has no single stage-zero regular-file entry",
        )
    })?;
    if index_mode != head_mode || index_object != head_object {
        return Err(head_file_mismatch(
            description,
            workspace_path,
            "the index entry differs from HEAD",
        ));
    }

    let worktree = fs::read(&absolute).map_err(|error| {
        head_file_mismatch(
            description,
            workspace_path,
            &format!("cannot read the worktree file: {error}"),
        )
    })?;
    let worktree_object = git.hash_path_bytes(&repository_path, &worktree)?;
    if worktree_object != head_object || !worktree_mode_matches(&metadata, &head_mode) {
        return Err(head_file_mismatch(
            description,
            workspace_path,
            "the filter-cleaned worktree content or executable mode differs from HEAD",
        ));
    }
    Ok(())
}

fn parse_head_entry(output: &[u8], expected_path: &str) -> Option<(String, String)> {
    let record = only_nul_record(output)?;
    let separator = record.iter().position(|byte| *byte == b'\t')?;
    if record.get(separator + 1..)? != expected_path.as_bytes() {
        return None;
    }
    let fields = std::str::from_utf8(record.get(..separator)?)
        .ok()?
        .split_whitespace()
        .collect::<Vec<_>>();
    match fields.as_slice() {
        [mode @ ("100644" | "100755"), "blob", object] => Some(((*mode).to_string(), (*object).to_string())),
        _ => None,
    }
}

fn parse_index_entry(output: &[u8], expected_path: &str) -> Option<(String, String)> {
    let record = only_nul_record(output)?;
    let separator = record.iter().position(|byte| *byte == b'\t')?;
    if record.get(separator + 1..)? != expected_path.as_bytes() {
        return None;
    }
    let fields = std::str::from_utf8(record.get(..separator)?)
        .ok()?
        .split_whitespace()
        .collect::<Vec<_>>();
    match fields.as_slice() {
        [mode @ ("100644" | "100755"), object, "0"] => Some(((*mode).to_string(), (*object).to_string())),
        _ => None,
    }
}

fn only_nul_record(output: &[u8]) -> Option<&[u8]> {
    let mut records = output.split(|byte| *byte == 0).filter(|record| !record.is_empty());
    let record = records.next()?;
    records.next().is_none().then_some(record)
}

fn worktree_mode_matches(metadata: &fs::Metadata, head_mode: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        (metadata.permissions().mode() & 0o111 != 0) == (head_mode == "100755")
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, head_mode);
        true
    }
}

fn head_file_mismatch(description: &str, workspace_path: &Path, reason: &str) -> RailError {
    let workspace_path = crate::utils::path_to_git_format(workspace_path);
    RailError::with_help(
        format!(
            "{description} '{}' does not exactly match HEAD: {reason}",
            workspace_path
        ),
        "commit or restore the manifest and lockfile before planning a release",
    )
}

fn read_isolated_lockfile(path: &Path, workspace_path: &Path) -> RailResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!(
            "failed to inspect isolated auxiliary Cargo lockfile '{}': {error}",
            workspace_path.display()
        ))
    })?;
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::with_help(
            format!(
                "Cargo replaced auxiliary Cargo lockfile '{}' with a non-regular file",
                workspace_path.display()
            ),
            "Cargo lockfile projection may replace only the bytes of a regular committed Cargo.lock file",
        ));
    }
    fs::read(path).map_err(|error| {
        RailError::message(format!(
            "failed to read isolated auxiliary Cargo lockfile '{}': {error}",
            workspace_path.display()
        ))
    })
}

fn local_release_package_specs(lockfile: &[u8], crates: &[CrateReleasePlan], path: &Path) -> RailResult<Vec<String>> {
    let content = std::str::from_utf8(lockfile).map_err(|error| {
        RailError::message(format!(
            "auxiliary Cargo lockfile '{}' is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let document = content.parse::<toml_edit::DocumentMut>().map_err(|error| {
        RailError::message(format!(
            "failed to parse auxiliary Cargo lockfile '{}': {error}",
            path.display()
        ))
    })?;
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .ok_or_else(|| RailError::message(format!("auxiliary Cargo lockfile '{}' has no packages", path.display())))?;
    let mut specs = Vec::new();
    for plan in crates {
        let current_version = plan.current_version.to_string();
        let present = packages.iter().any(|package| {
            package.get("source").is_none()
                && package.get("name").and_then(toml_edit::Item::as_str) == Some(plan.name.as_str())
                && package.get("version").and_then(toml_edit::Item::as_str) == Some(current_version.as_str())
        });
        if present {
            specs.push(format!("{}@{}", plan.name, plan.current_version));
        }
    }
    Ok(specs)
}

fn run_cargo_update(ctx: &WorkspaceContext, root: &Path, manifest: &Path, packages: &[String]) -> RailResult<()> {
    let mut command = cargo_command(ctx, root)?;
    command.arg("update").arg("--manifest-path").arg(manifest);
    for package in packages {
        command.args(["--package", package]);
    }
    let output = command
        .output()
        .map_err(|error| RailError::message(format!("failed to start auxiliary Cargo update: {error}")))?;
    require_success(
        output,
        &format!("cargo update --package {}", packages.join(" --package ")),
    )?;
    Ok(())
}

fn cargo_command(ctx: &WorkspaceContext, root: &Path) -> RailResult<Command> {
    let mut command = Command::new(ctx.snapshot()?.toolchain().cargo_program());
    command.current_dir(root);
    Ok(command)
}

fn require_success(output: Output, command: &str) -> RailResult<Output> {
    if output.status.success() {
        return Ok(output);
    }
    Err(RailError::with_help(
        format!(
            "{command} failed while planning auxiliary Cargo lockfiles: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        "fix the standalone Cargo workspace without changing the primary release worktree, then retry",
    ))
}

fn audit_isolated_tree(
    snapshot: &crate::workspace::WorkspaceSnapshot,
    root: &Path,
    planned_digests: &BTreeMap<PathBuf, ContentDigest>,
) -> RailResult<()> {
    let mut expected = snapshot
        .source()
        .tree()
        .entries()
        .iter()
        .map(|entry| (entry.path.as_path().to_path_buf(), entry.kind.clone()))
        .collect::<BTreeMap<_, _>>();
    for (path, bytes) in projection_inputs(snapshot)? {
        expected.insert(
            path,
            SourceEntryKind::RegularFile {
                digest: ContentDigest::sha256(&bytes),
                executable: false,
            },
        );
    }
    let mut actual = BTreeSet::new();
    collect_tree_paths(root, root, &mut actual)?;
    let unexpected = actual
        .iter()
        .filter(|path| !expected.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(RailError::with_help(
            format!(
                "auxiliary Cargo planning created undeclared paths: {}",
                display_paths(&unexpected)
            ),
            "Cargo lockfile projection may mutate only committed auxiliary Cargo.lock files",
        ));
    }
    let missing = expected
        .keys()
        .filter(|path| !actual.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(RailError::with_help(
            format!(
                "auxiliary Cargo planning removed undeclared paths: {}",
                display_paths(&missing)
            ),
            "Cargo lockfile projection may mutate only committed auxiliary Cargo.lock files",
        ));
    }
    for (path, kind) in expected {
        if let Some(planned_digest) = planned_digests.get(&path) {
            let absolute = root.join(&path);
            let metadata = fs::symlink_metadata(&absolute)?;
            let exact_candidate = match &kind {
                SourceEntryKind::RegularFile { executable, .. } => {
                    regular_metadata_matches(&metadata, *executable)
                        && ContentDigest::sha256(&fs::read(&absolute)?) == *planned_digest
                }
                SourceEntryKind::Symlink { .. } | SourceEntryKind::Deleted => false,
            };
            if !exact_candidate {
                return Err(RailError::with_help(
                    format!(
                        "auxiliary Cargo planning mutated planned path '{}' after binding its candidate bytes",
                        path.display()
                    ),
                    "Cargo lockfile projection may produce only the exact planned manifest and lockfile bytes",
                ));
            }
            continue;
        }
        let absolute = root.join(&path);
        let unchanged = match &kind {
            SourceEntryKind::RegularFile {
                digest: expected,
                executable,
            } => {
                let metadata = fs::symlink_metadata(&absolute)?;
                regular_metadata_matches(&metadata, *executable)
                    && ContentDigest::sha256(&fs::read(&absolute)?) == *expected
            }
            SourceEntryKind::Symlink { target } => {
                let metadata = fs::symlink_metadata(&absolute)?;
                crate::utils::is_symlink_or_reparse(&metadata) && fs::read_link(&absolute)? == Path::new(target)
            }
            SourceEntryKind::Deleted => false,
        };
        if !unchanged {
            return Err(RailError::with_help(
                format!("auxiliary Cargo planning mutated undeclared path '{}'", path.display()),
                "Cargo lockfile projection may mutate only committed auxiliary Cargo.lock files",
            ));
        }
    }
    Ok(())
}

fn regular_metadata_matches(metadata: &fs::Metadata, executable: bool) -> bool {
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(metadata) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        (metadata.permissions().mode() & 0o111 != 0) == executable
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
        true
    }
}

fn collect_tree_paths(root: &Path, directory: &Path, output: &mut BTreeSet<PathBuf>) -> RailResult<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if crate::utils::is_symlink_or_reparse(&metadata) {
            output.insert(
                path.strip_prefix(root)
                    .map_err(|_| {
                        RailError::message(format!("isolated Cargo path '{}' escaped staging", path.display()))
                    })?
                    .to_path_buf(),
            );
        } else if metadata.file_type().is_dir() {
            collect_tree_paths(root, &path, output)?;
        } else if metadata.file_type().is_file() {
            output.insert(
                path.strip_prefix(root)
                    .map_err(|_| {
                        RailError::message(format!("isolated Cargo path '{}' escaped staging", path.display()))
                    })?
                    .to_path_buf(),
            );
        } else {
            return Err(RailError::message(format!(
                "isolated Cargo planning created unsupported entry '{}'",
                path.display()
            )));
        }
    }
    Ok(())
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ContentDigest::sha256(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_entry_parsers_use_portable_repository_paths() {
        let path = crate::utils::path_to_git_format(Path::new("aux\\Cargo.toml"));
        assert_eq!(path, "aux/Cargo.toml");
        assert_eq!(
            portable_contract_path(Path::new("aux\\Cargo.lock")),
            Path::new("aux/Cargo.lock")
        );
        assert_eq!(
            parse_head_entry(b"100644 blob head-object\taux/Cargo.toml\0", &path),
            Some(("100644".to_string(), "head-object".to_string()))
        );
        assert_eq!(
            parse_index_entry(b"100644 index-object 0\taux/Cargo.toml\0", &path),
            Some(("100644".to_string(), "index-object".to_string()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_lockfile_reader_rejects_symlink_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let lockfile = directory.path().join("Cargo.lock");
        fs::write(&target, b"sensitive bytes").unwrap();
        std::os::unix::fs::symlink(&target, &lockfile).unwrap();

        let error = read_isolated_lockfile(&lockfile, Path::new("aux/Cargo.lock")).unwrap_err();
        assert!(error.to_string().contains("non-regular file"), "{error}");
    }
}
