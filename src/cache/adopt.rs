//! Transfer reusable legacy workspace files to the selected user directories.

use crate::error::{RailError, RailResult};
use crate::utils::{canonicalize_existing, is_symlink_or_reparse, private_file_matches_path};
use crate::workspace::{self, WorkspaceStorage};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

pub(crate) struct Adoption {
    pub(crate) source: PathBuf,
    pub(crate) files: Vec<PathBuf>,
}

struct Transfer {
    source: PathBuf,
    destination: PathBuf,
    root: PathBuf,
}

impl Transfer {
    fn preserves_mode(&self) -> bool {
        self.destination
            .strip_prefix(&self.root)
            .is_ok_and(|relative| relative.starts_with("backups"))
    }
}

pub(crate) fn adopt(workspace_root: &Path, from: Option<&Path>, check: bool) -> RailResult<Adoption> {
    if !workspace::uses_user_storage() {
        return Err(RailError::with_help(
            "cache adopt requires user workspace storage",
            "set CARGO_RAIL_WORKSPACE_STORAGE=user and retry",
        ));
    }
    let workspace_root = canonicalize_existing(workspace_root)?;
    let source = from.map_or_else(
        || workspace::cargo_rail_state_root(&workspace_root),
        |from| workspace_root.join(from),
    );
    let metadata = fs::symlink_metadata(&source).map_err(|error| {
        RailError::message(format!(
            "cannot inspect adoption source '{}': {error}",
            source.display()
        ))
    })?;
    if !metadata.is_dir() || is_symlink_or_reparse(&metadata) {
        return Err(not_regular(&source, "directory"));
    }
    // A managed Cargo target can be an alias. Everything beneath the
    // resolved source directory must be an actual file or directory.
    let source = canonicalize_existing(&source)?;
    let cache = workspace::workspace_storage_dir(&workspace_root, WorkspaceStorage::Cache, None, false)?;
    let state = workspace::workspace_storage_dir(&workspace_root, WorkspaceStorage::State, None, false)?;
    let report = workspace::report_dir(&workspace_root)?;
    for root in [&cache, &state, &report] {
        if root.starts_with(&source) || source.starts_with(root) {
            return Err(RailError::message(format!(
                "adoption source '{}' overlaps destination '{}'",
                source.display(),
                root.display()
            )));
        }
    }
    let mut pending = transfers(&source, &cache, &state, &report)?;
    if !check && !pending.is_empty() {
        // Always acquire both locks in path order, even for an explicitly
        // selected source. Keep preview read-only and recheck under the locks.
        let lock_source = || -> RailResult<_> {
            let path = source.join("cache.lock");
            if metadata_at(&path)?.is_none() {
                return Ok(None);
            }
            let file = open_regular(&path)?;
            file.lock_shared()?;
            if !private_file_matches_path(&file, &path, super::WORKSPACE_LOCK_BYTES)? {
                return Err(RailError::message(format!(
                    "adoption source lock '{}' changed",
                    path.display()
                )));
            }
            Ok(Some(file))
        };
        let lock_cache = || super::lock_workspace(&workspace_root);
        let (_source_lock, _cache_lock) = if source < cache {
            (lock_source()?, lock_cache()?)
        } else {
            let cache_lock = lock_cache()?;
            (lock_source()?, cache_lock)
        };
        if workspace::workspace_storage_dir(&workspace_root, WorkspaceStorage::Cache, None, false)? != cache
            || workspace::workspace_storage_dir(&workspace_root, WorkspaceStorage::State, None, false)? != state
        {
            return Err(RailError::message("adoption destination changed during resolution"));
        }
        pending = transfers(&source, &cache, &state, &report)?;
        if pending.iter().any(|transfer| transfer.root == state) {
            workspace::workspace_storage_dir(&workspace_root, WorkspaceStorage::State, None, true)?;
        }
        if pending.iter().any(|transfer| transfer.root == report) {
            workspace::resolve_storage_directory(workspace_root, Path::new(".cargo-rail"), None, true, false)?;
        }
        for transfer in &pending {
            copy(transfer)?;
        }
    }
    Ok(Adoption {
        source,
        files: pending.into_iter().map(|transfer| transfer.destination).collect(),
    })
}

fn transfers(source: &Path, cache: &Path, state: &Path, report: &Path) -> RailResult<Vec<Transfer>> {
    let journal = crate::compiler::acquisition::journal::JOURNAL_DIRECTORY;
    let readiness = Path::new("compiler-artifacts-v1").join(super::READINESS_RECEIPT_FILE);
    let mut pending = Vec::new();
    for (relative, root, tree) in [
        (Path::new("receipts"), state, true),
        (Path::new("backups"), state, true),
        (Path::new(journal), cache, true),
        (Path::new("generated-lockfile-v1"), cache, false),
        (&readiness, cache, false),
        (Path::new("unify-report.md"), report, false),
    ] {
        let path = source.join(relative);
        let Some(metadata) = metadata_at(&path)? else {
            continue;
        };
        if is_symlink_or_reparse(&metadata) || (tree && !metadata.is_dir()) || (!tree && !metadata.is_file()) {
            return Err(not_regular(&path, if tree { "directory" } else { "file" }));
        }
        if relative == Path::new("receipts") || relative == Path::new(journal) {
            if canonicalize_existing(&path)? != path {
                return Err(not_regular(&path, "directory"));
            }
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "json") {
                    plan_file(&path, source, root, &mut pending)?;
                }
            }
        } else if relative == Path::new("backups") {
            plan_backups(&path, source, root, &mut pending)?;
        } else {
            plan_file(&path, source, root, &mut pending)?;
        }
    }
    // A backup is discoverable as soon as its metadata is present.
    let backups = state.join("backups");
    pending.sort_by(|left, right| {
        let manifest = |transfer: &Transfer| {
            transfer
                .destination
                .file_name()
                .is_some_and(|name| name == "metadata.json")
                && transfer.destination.parent().and_then(Path::parent) == Some(backups.as_path())
        };
        manifest(left)
            .cmp(&manifest(right))
            .then_with(|| left.destination.cmp(&right.destination))
    });
    pending.dedup_by(|left, right| left.destination == right.destination);
    Ok(pending)
}

fn plan_backups(path: &Path, source: &Path, root: &Path, pending: &mut Vec<Transfer>) -> RailResult<()> {
    if canonicalize_existing(path)? != path {
        return Err(not_regular(path, "directory"));
    }
    for entry in fs::read_dir(path)? {
        let directory = entry?.path();
        let metadata = fs::symlink_metadata(&directory)?;
        if is_symlink_or_reparse(&metadata) {
            return Err(not_regular(&directory, "backup directory"));
        }
        if !metadata.is_dir() {
            continue;
        }
        let manifest = directory.join("metadata.json");
        if metadata_at(&manifest)?.is_none() {
            // Legacy backup creation publishes its metadata after copying the payloads.
            continue;
        }
        let metadata: crate::backup::BackupMetadata =
            serde_json::from_reader(open_regular(&manifest)?).map_err(|error| {
                RailError::message(format!("invalid backup metadata '{}': {error}", manifest.display()))
            })?;
        for relative in metadata.files_modified {
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || relative == Path::new("metadata.json")
            {
                return Err(RailError::message(format!(
                    "invalid backup path '{}' in '{}'",
                    relative.display(),
                    manifest.display()
                )));
            }
            plan_file(&directory.join(relative), source, root, pending)?;
        }
        plan_file(&manifest, source, root, pending)?;
    }
    Ok(())
}

fn plan_file(path: &Path, source: &Path, root: &Path, pending: &mut Vec<Transfer>) -> RailResult<()> {
    let relative = path.strip_prefix(source).expect("planned a source descendant");
    let transfer = Transfer {
        destination: root.join(relative),
        source: path.to_path_buf(),
        root: root.to_path_buf(),
    };
    validate_parent(root, &transfer.destination, false)?;
    let incoming = open_regular(&transfer.source)?;
    if metadata_at(&transfer.destination)?.is_some() {
        if same_contents(
            incoming,
            open_regular(&transfer.destination)?,
            transfer.preserves_mode(),
        )? {
            return Ok(());
        }
        return Err(conflict(&transfer.destination));
    }
    pending.push(transfer);
    Ok(())
}

fn validate_parent(root: &Path, destination: &Path, create: bool) -> RailResult<PathBuf> {
    let parent = destination.parent().expect("destination has a parent");
    let relative = parent
        .strip_prefix(root)
        .expect("destination is under its selected root");
    workspace::resolve_storage_directory(root.to_path_buf(), relative, None, create, true)
}

fn copy(transfer: &Transfer) -> RailResult<()> {
    let parent = validate_parent(&transfer.root, &transfer.destination, true)?;
    let mut source = open_regular(&transfer.source)?;
    let before = crate::utils::stable_file_generation(&transfer.source);
    let expected = source.metadata()?.len();
    let mut staged = tempfile::NamedTempFile::new_in(&parent)?;
    let copied = io::copy(&mut source, staged.as_file_mut())?;
    if copied != expected
        || !private_file_matches_path(&source, &transfer.source, expected)?
        || before != crate::utils::stable_file_generation(&transfer.source)
        || canonicalize_existing(&transfer.source)? != transfer.source
    {
        return Err(RailError::message(format!(
            "adoption source '{}' changed during copy",
            transfer.source.display()
        )));
    }
    #[cfg(unix)]
    if transfer.preserves_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = source.metadata()?.permissions().mode() & 0o777;
        staged.as_file().set_permissions(fs::Permissions::from_mode(mode))?;
    }
    staged.as_file().sync_all()?;
    validate_parent(&transfer.root, &transfer.destination, false)?;
    match staged.persist_noclobber(&transfer.destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if !same_contents(
                error.file.reopen()?,
                open_regular(&transfer.destination)?,
                transfer.preserves_mode(),
            )? {
                return Err(conflict(&transfer.destination));
            }
        }
        Err(error) => return Err(error.error.into()),
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn metadata_at(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn open_regular(path: &Path) -> RailResult<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || is_symlink_or_reparse(&metadata) || canonicalize_existing(path)? != path {
        return Err(not_regular(path, "file"));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        options.custom_flags(flags.bits() as i32);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !private_file_matches_path(&file, path, metadata.len())? {
        return Err(not_regular(path, "file with one link"));
    }
    Ok(file)
}

fn same_contents(left: File, right: File, preserve_mode: bool) -> io::Result<bool> {
    #[cfg(not(unix))]
    let _ = preserve_mode;
    #[cfg(unix)]
    if preserve_mode {
        use std::os::unix::fs::PermissionsExt as _;
        if left.metadata()?.permissions().mode() & 0o777 != right.metadata()?.permissions().mode() & 0o777 {
            return Ok(false);
        }
    }
    let (mut left, mut right) = (BufReader::new(left), BufReader::new(right));
    loop {
        let (a, b) = (left.fill_buf()?, right.fill_buf()?);
        let size = a.len().min(b.len());
        if size == 0 {
            return Ok(a.is_empty() && b.is_empty());
        }
        if a[..size] != b[..size] {
            return Ok(false);
        }
        left.consume(size);
        right.consume(size);
    }
}

fn not_regular(path: &Path, kind: &str) -> RailError {
    RailError::message(format!("adoption path '{}' must be a real {kind}", path.display()))
}

fn conflict(path: &Path) -> RailError {
    RailError::message(format!(
        "adoption destination '{}' has different contents or permissions",
        path.display()
    ))
}
