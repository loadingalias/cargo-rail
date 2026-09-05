//! Additional operations for SystemGit (commit walking, remotes, etc.)

use super::SystemGit;
use super::system::{CommitInfo, CommitMetadata};
use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::mutation::git_effect::GitCommitEffect;
use crate::progress;
use crate::source::RepositoryPath;
use crate::utils;
use std::path::{Path, PathBuf};

/// One exact entry from a Git tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitTreeEntry {
    /// Git file mode (`100644`, `100755`, `120000`, or `160000`).
    pub mode: String,
    /// Object ID referenced by the tree entry.
    pub object_id: String,
    /// Repository-relative entry path.
    pub path: PathBuf,
}

/// Exact index and worktree images captured together for one repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitPathImages {
    pub(crate) index: Option<GitTreeEntry>,
    pub(crate) worktree: Option<GitTreeEntry>,
}

/// Exact index mutation used to synthesize one commit tree.
pub(crate) enum GitIndexChange {
    /// Insert or replace an entry with an exact Git mode and object ID.
    Upsert(GitTreeEntry),
    /// Remove one repository-relative path.
    Delete(PathBuf),
}

/// How an exact prepared pack converged its selected local branch ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitRefInstallOutcome {
    /// The helper performed the exact old-to-new ref compare-and-swap.
    Updated,
    /// The exact result ref was already present and was adopted after validation.
    Adopted,
}

/// Private object-writing view used to prepare one Git effect without changing
/// the repository's real object database.
pub(crate) struct GitObjectQuarantine {
    directory: tempfile::TempDir,
    git: SystemGit,
    object_format: String,
    empty_shallow_file: PathBuf,
    exact_tree_index: tempfile::TempPath,
    exact_tree_index_initialized: std::cell::Cell<bool>,
    exact_tree_entries: std::cell::RefCell<std::collections::BTreeSet<PathBuf>>,
}

impl GitObjectQuarantine {
    /// Create a Git command whose object reads and writes are both confined to
    /// this private object database.
    ///
    /// Real repositories are deliberately not exposed as alternates. Git may
    /// freshen an object found in an alternate when an object writer observes a
    /// duplicate, which would make preparation mutate the source object store.
    pub(crate) fn git_cmd(&self) -> std::process::Command {
        let mut command = self.git.git_cmd();
        command
            .env("GIT_OBJECT_DIRECTORY", self.directory.path())
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env("GIT_SHALLOW_FILE", &self.empty_shallow_file)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        command
    }

    /// Copy the complete closure of exact objects from one readable repository
    /// into this private object database.
    ///
    /// The source-side pack is read-only. It is indexed without alternates, so
    /// a missing source object fails before any real repository is mutated.
    pub(crate) fn import_object_closure(&self, source: &SystemGit, object_ids: &[&str]) -> RailResult<()> {
        use std::collections::BTreeSet;
        use std::io::{Seek as _, Write as _};
        use std::process::Stdio;

        if object_ids.is_empty() {
            return Ok(());
        }
        let source_format = source.object_format()?;
        if source_format != self.object_format {
            return Err(RailError::message(format!(
                "cannot import {source_format} Git objects into a {} object database",
                self.object_format
            )));
        }
        let object_ids = object_ids
            .iter()
            .copied()
            .map(|object_id| {
                validate_exact_object_id(object_id, &self.object_format)?;
                Ok(object_id)
            })
            .collect::<RailResult<BTreeSet<_>>>()?;
        let mut pack = tempfile::tempfile().context("Failed to allocate private Git input pack")?;
        let mut source_command = source.git_cmd();
        source_command.env("GIT_SHALLOW_FILE", &self.empty_shallow_file);
        let mut command = deterministic_pack_command(source_command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(pack.try_clone()?))
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to start private Git input packing")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("private Git input pack stdin was unavailable"))?;
        for object_id in &object_ids {
            writeln!(stdin, "{object_id}").context("Failed to select private Git input closure")?;
        }
        drop(stdin);
        let packed = child
            .wait_with_output()
            .context("Failed to finish private Git input packing")?;
        if !packed.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git pack-objects --stdout --revs (private input closure)".to_string(),
                stderr: git_command_diagnostics(&packed.stdout, &packed.stderr),
            }));
        }
        pack.rewind()?;
        let output = deterministic_index_pack_command(self.git_cmd(), None)
            .stdin(Stdio::from(pack))
            .output()
            .context("Failed to index private Git input closure")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git index-pack --stdin (private input closure)".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        self.require_complete_closures(object_ids)?;
        Ok(())
    }

    fn import_prepared_pack(&self, bundle: &std::fs::File) -> RailResult<()> {
        use std::process::Stdio;

        let output = deterministic_index_pack_command(self.git_cmd(), None)
            .stdin(Stdio::from(bundle.try_clone()?))
            .output()
            .context("Failed to inspect prepared Git object pack privately")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git index-pack --stdin (prepared verification)".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(())
    }

    fn verify_prepared_commit(&self, expected: &GitCommitEffect) -> RailResult<()> {
        validate_exact_object_id(expected.oid(), &self.object_format)?;
        self.require_complete_closure(expected.oid())?;
        let mut command = self.git_cmd();
        command.args(["cat-file", "commit", expected.oid()]);
        verify_prepared_commit_output(command, expected)
    }

    /// Write one blob into the private quarantine and return its object ID.
    pub(crate) fn write_blob(&self, content: &[u8]) -> RailResult<String> {
        use std::io::Write as _;
        use std::process::Stdio;

        let mut command = self.git_cmd();
        command
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to start quarantined Git blob write")?;
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("quarantined Git blob input was unavailable"))?
            .write_all(content)
            .context("Failed to write quarantined Git blob")?;
        let output = child
            .wait_with_output()
            .context("Failed to finish quarantined Git blob write")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git hash-object -w --stdin (quarantine)".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    /// Write a bounded set of independent blobs in one Git process.
    pub(crate) fn write_blobs(&self, contents: &[Vec<u8>]) -> RailResult<Vec<String>> {
        if contents.is_empty() {
            return Ok(Vec::new());
        }
        let inputs = tempfile::Builder::new()
            .prefix("cargo-rail-quarantine-blobs-")
            .tempdir()
            .context("Failed to allocate private Git blob inputs")?;
        let mut paths = Vec::with_capacity(contents.len());
        for (index, content) in contents.iter().enumerate() {
            let path = inputs.path().join(format!("blob-{index}"));
            std::fs::write(&path, content).context("Failed to stage private Git blob input")?;
            paths.push(path);
        }
        let output = self
            .git_cmd()
            .args(["hash-object", "-w", "--no-filters"])
            .args(&paths)
            .output()
            .context("Failed to write private Git blob batch")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git hash-object -w --no-filters <bounded-inputs>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let object_ids = String::from_utf8(output.stdout)?
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if object_ids.len() != contents.len() {
            return Err(RailError::message("Git returned an incomplete private blob batch"));
        }
        for object_id in &object_ids {
            validate_exact_object_id(object_id, &self.object_format)?;
        }
        Ok(object_ids)
    }

    /// Write one exact flat file tree in the private object database.
    pub(crate) fn write_exact_tree(&self, entries: &[GitTreeEntry]) -> RailResult<String> {
        use std::io::Write as _;
        use std::process::Stdio;

        if !self.exact_tree_index_initialized.get() {
            let initialized = self
                .git_cmd()
                .env("GIT_INDEX_FILE", &self.exact_tree_index)
                .args(["read-tree", "--empty"])
                .output()
                .context("Failed to initialize private exact-tree index")?;
            if !initialized.status.success() {
                return Err(RailError::Git(GitError::CommandFailed {
                    command: "git read-tree --empty (quarantine)".to_string(),
                    stderr: git_command_diagnostics(&initialized.stdout, &initialized.stderr),
                }));
            }
            self.exact_tree_index_initialized.set(true);
        }

        let mut ordered = entries.to_vec();
        ordered.sort_by(|left, right| left.path.cmp(&right.path));
        reject_platform_tree_aliases(&ordered)?;
        let mut previous = None::<String>;
        let mut index_input = Vec::new();
        let current_paths = ordered
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let zero = "0".repeat(object_id_hex_len(&self.object_format)?);
        for deleted in self.exact_tree_entries.borrow().difference(&current_paths) {
            let path = RepositoryPath::new(deleted)?;
            write!(index_input, "0 {zero}\t{}\0", path.as_str())
                .context("Failed to encode private exact-tree index deletion")?;
        }
        for entry in &ordered {
            let path = RepositoryPath::new(&entry.path)?;
            if previous.as_deref() == Some(path.as_str()) {
                return Err(RailError::message(format!(
                    "private exact tree repeats path '{}'",
                    path.as_str()
                )));
            }
            previous = Some(path.as_str().to_string());
            if !matches!(entry.mode.as_str(), "100644" | "100755" | "120000") {
                return Err(RailError::message(format!(
                    "private exact tree path '{}' has unsupported mode '{}'",
                    path.as_str(),
                    entry.mode
                )));
            }
            validate_exact_object_id(&entry.object_id, &self.object_format)?;
            write!(index_input, "{} {}\t{}\0", entry.mode, entry.object_id, path.as_str())
                .context("Failed to encode private exact-tree index entry")?;
        }
        let mut update = self.git_cmd();
        update
            .env("GIT_INDEX_FILE", &self.exact_tree_index)
            .env_remove("GIT_GLOB_PATHSPECS")
            .env_remove("GIT_NOGLOB_PATHSPECS")
            .env_remove("GIT_ICASE_PATHSPECS")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .args(["update-index", "-z", "--index-info"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = update.spawn().context("Failed to start private exact-tree update")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("private exact-tree index input was unavailable"))?;
        stdin
            .write_all(&index_input)
            .context("Failed to write private exact-tree index entries")?;
        drop(stdin);
        let updated = child
            .wait_with_output()
            .context("Failed to finish private exact-tree update")?;
        if !updated.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git update-index --index-info (quarantine)".to_string(),
                stderr: git_command_diagnostics(&updated.stdout, &updated.stderr),
            }));
        }
        let written = self
            .git_cmd()
            .env("GIT_INDEX_FILE", &self.exact_tree_index)
            .arg("write-tree")
            .output()
            .context("Failed to write private exact tree")?;
        if !written.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git write-tree (quarantine)".to_string(),
                stderr: git_command_diagnostics(&written.stdout, &written.stderr),
            }));
        }
        self.exact_tree_entries.replace(current_paths);
        Ok(String::from_utf8(written.stdout)?.trim().to_string())
    }

    /// Write one deterministic commit in the private object database.
    pub(crate) fn write_commit(
        &self,
        tree: &str,
        parents: &[String],
        message: &str,
        metadata: &CommitMetadata,
    ) -> RailResult<String> {
        use std::io::Write as _;
        use std::process::Stdio;

        validate_exact_object_id(tree, &self.object_format)?;
        for parent in parents {
            validate_exact_object_id(parent, &self.object_format)?;
        }
        let author_date = format!("{} {}", metadata.author_timestamp, metadata.author_timezone);
        let committer_date = format!("{} {}", metadata.committer_timestamp, metadata.committer_timezone);
        let mut command = self.git_cmd();
        command
            .env("GIT_AUTHOR_NAME", &metadata.author)
            .env("GIT_AUTHOR_EMAIL", &metadata.author_email)
            .env("GIT_AUTHOR_DATE", &author_date)
            .env("GIT_COMMITTER_NAME", &metadata.committer)
            .env("GIT_COMMITTER_EMAIL", &metadata.committer_email)
            .env("GIT_COMMITTER_DATE", &committer_date)
            .arg("commit-tree")
            .arg(tree)
            .args(["-F", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for parent in parents {
            command.arg("-p").arg(parent);
        }
        let mut child = command.spawn().context("Failed to start private Git commit")?;
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("private Git commit input was unavailable"))?
            .write_all(message.as_bytes())
            .context("Failed to write private Git commit message")?;
        let committed = child
            .wait_with_output()
            .context("Failed to finish private Git commit")?;
        if !committed.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git commit-tree (quarantine)".to_string(),
                stderr: git_command_diagnostics(&committed.stdout, &committed.stderr),
            }));
        }
        Ok(String::from_utf8(committed.stdout)?.trim().to_string())
    }

    /// Write a deterministic, self-contained pack for the prepared result.
    /// Objects reachable from `expected` are excluded because they are already
    /// required to exist in the target repository.
    pub(crate) fn write_pack(
        &self,
        result: &str,
        expected: Option<&str>,
        output: &mut std::fs::File,
    ) -> RailResult<String> {
        use rscrypto::Sha256;
        use std::io::{Read as _, Seek as _, Write as _};
        use std::process::Stdio;

        validate_exact_object_id(result, &self.object_format)?;
        if let Some(expected) = expected {
            validate_exact_object_id(expected, &self.object_format)?;
            self.require_complete_closure(expected)?;
        }
        self.require_complete_closure(result)?;
        output.set_len(0)?;
        output.rewind()?;
        let mut command = deterministic_pack_command(self.git_cmd());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to start prepared Git object packing")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("prepared Git pack input was unavailable"))?;
        writeln!(stdin, "{result}").context("Failed to select prepared Git result objects")?;
        if let Some(expected) = expected {
            writeln!(stdin, "^{expected}").context("Failed to exclude existing Git result ancestry")?;
        }
        drop(stdin);
        let packed = child
            .wait_with_output()
            .context("Failed to finish prepared Git object packing")?;
        if !packed.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git pack-objects --stdout --revs".to_string(),
                stderr: git_command_diagnostics(&packed.stdout, &packed.stderr),
            }));
        }
        output.sync_all()?;
        output.rewind()?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = output.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        output.rewind()?;
        let hex = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!("sha256-{hex}"))
    }

    fn require_complete_closure(&self, object_id: &str) -> RailResult<()> {
        require_complete_object_closure(
            self.git_cmd(),
            object_id,
            &self.object_format,
            "private Git object input",
        )
    }

    fn require_complete_closures<'a>(&self, object_ids: impl IntoIterator<Item = &'a str>) -> RailResult<()> {
        require_complete_object_closures(
            self.git_cmd(),
            object_ids,
            &self.object_format,
            "private Git object input",
        )
    }
}

impl SystemGit {
    /// Prepare an isolated object-writing view. Required input closures must be
    /// copied explicitly with [`GitObjectQuarantine::import_object_closure`].
    pub(crate) fn object_quarantine(&self) -> RailResult<GitObjectQuarantine> {
        let directory = tempfile::Builder::new()
            .prefix("cargo-rail-git-objects-")
            .tempdir()
            .context("Failed to allocate private Git object quarantine")?;
        let object_format = self.object_format()?;
        validate_object_format(&object_format)?;
        let empty_shallow_file = create_empty_shallow_file(directory.path())?;
        let exact_tree_index = tempfile::Builder::new()
            .prefix("cargo-rail-quarantine-index-")
            .tempfile()
            .context("Failed to allocate private exact-tree index")?
            .into_temp_path();
        std::fs::remove_file(&exact_tree_index).context("Failed to initialize private exact-tree index")?;
        Ok(GitObjectQuarantine {
            directory,
            git: self.clone(),
            object_format,
            empty_shallow_file,
            exact_tree_index,
            exact_tree_index_initialized: std::cell::Cell::new(false),
            exact_tree_entries: std::cell::RefCell::new(std::collections::BTreeSet::new()),
        })
    }

    fn object_directory(&self) -> RailResult<PathBuf> {
        let value = self.run_git_stdout(&["rev-parse", "--git-path", "objects"])?;
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            self.repo_path.join(path)
        };
        utils::canonicalize_existing(&path).map_err(Into::into)
    }

    pub(crate) fn remove_owned_pack_keeps(&self, effect_id: &str) -> RailResult<()> {
        validate_effect_keep_id(effect_id)?;
        let pack_directory = self.object_directory()?.join("pack");
        let entries = match std::fs::read_dir(&pack_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let message = format!("cargo-rail prepared effect {effect_id}");
        let mut removed = false;
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "keep") {
                continue;
            }
            if inspect_pack_keep(&path, &message)? {
                remove_owned_pack_keep(&path, &message)?;
                removed = true;
            }
        }
        if removed {
            #[cfg(unix)]
            std::fs::File::open(&pack_directory)?.sync_all()?;
        }
        Ok(())
    }

    /// Install one retained prepared pack and reconcile an exact branch ref.
    ///
    /// The supplied file is the already no-follow, identity-checked handle from
    /// the effect store. Git receives a duplicate of that same handle; the path
    /// is diagnostic only. `index-pack --keep` protects the unreachable result
    /// until the exact ref compare-and-swap succeeds. A pre-existing third ref
    /// state is rejected before the object database is touched.
    #[expect(
        clippy::too_many_arguments,
        reason = "the Git mutation boundary exposes every retained-handle and compare-and-swap input"
    )]
    pub(crate) fn install_prepared_object_pack_and_update_ref(
        &self,
        mut bundle: std::fs::File,
        bundle_path: &Path,
        expected_digest: &str,
        expected_commit: &GitCommitEffect,
        ref_name: &str,
        expected_ref: Option<&str>,
        effect_id: &str,
    ) -> RailResult<GitRefInstallOutcome> {
        use std::io::Seek as _;
        use std::process::Stdio;

        validate_branch_ref_name(ref_name)?;
        validate_effect_keep_id(effect_id)?;
        let object_format = self.object_format()?;
        validate_object_format(&object_format)?;
        let result_commit = expected_commit.oid();
        validate_exact_object_id(result_commit, &object_format)?;
        if let Some(expected) = expected_ref {
            validate_exact_object_id(expected, &object_format)?;
            if expected == result_commit {
                return Err(RailError::message(
                    "prepared Git result ref must differ from its expected ref",
                ));
            }
        }
        validate_sha256_digest(expected_digest)?;
        let observed = self.exact_ref_oid(ref_name, Some(&object_format))?;
        if observed.as_deref() != expected_ref && observed.as_deref() != Some(result_commit) {
            return Err(RailError::message(format!(
                "prepared Git ref '{}' changed before object installation: expected {}, found {}",
                ref_name,
                expected_ref.unwrap_or("absent"),
                observed.as_deref().unwrap_or("absent")
            )));
        }
        let shallow_directory = tempfile::Builder::new()
            .prefix("cargo-rail-git-shallow-")
            .tempdir()
            .context("Failed to allocate complete Git object view")?;
        let empty_shallow_file = create_empty_shallow_file(shallow_directory.path())?;
        if observed.as_deref() == expected_ref
            && let Some(expected) = expected_ref
        {
            self.require_complete_local_closure(
                expected,
                &object_format,
                "expected local Git ref",
                &empty_shallow_file,
            )?;
        }

        let metadata = bundle.metadata().map_err(|error| {
            RailError::message(format!(
                "failed to inspect prepared Git object bundle '{}': {error}",
                bundle_path.display()
            ))
        })?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(RailError::message(format!(
                "prepared Git object bundle '{}' is not a non-empty regular file",
                bundle_path.display()
            )));
        }
        let actual_digest = digest_opened_pack(&mut bundle)?;
        if actual_digest != expected_digest {
            return Err(RailError::message(format!(
                "prepared Git object bundle '{}' digest changed: expected {expected_digest}, found {actual_digest}",
                bundle_path.display()
            )));
        }
        bundle.rewind()?;
        let verification = self.object_quarantine()?;
        if let Some(expected) = expected_ref {
            verification.import_object_closure(self, &[expected])?;
        }
        verification.import_prepared_pack(&bundle)?;
        verification.verify_prepared_commit(expected_commit)?;
        bundle.rewind()?;
        let keep_message = format!("cargo-rail prepared effect {effect_id}");
        let mut install_command = self.git_cmd();
        install_command.env("GIT_SHALLOW_FILE", &empty_shallow_file);
        let output = deterministic_index_pack_command(install_command, Some(&keep_message))
            .stdin(Stdio::from(bundle.try_clone()?))
            .output()
            .context("Failed to install prepared Git object pack")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git index-pack --stdin".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let (pack_disposition, pack_hash) = parse_index_pack_result(&output.stdout, &object_format)?;
        let keep_path = self
            .object_directory()?
            .join("pack")
            .join(format!("pack-{pack_hash}.keep"));
        let keep_owned = inspect_pack_keep(&keep_path, &keep_message)?;
        if pack_disposition == "keep" && !keep_owned {
            return Err(RailError::message(format!(
                "Git reported a retained prepared pack but '{}' is not owned by this effect",
                keep_path.display()
            )));
        }
        let repeated_digest = digest_opened_pack(&mut bundle)?;
        if repeated_digest != expected_digest || bundle.metadata()?.len() != metadata.len() {
            return Err(RailError::message(format!(
                "prepared Git object bundle '{}' changed while Git installed it",
                bundle_path.display()
            )));
        }
        self.require_complete_local_closure(
            result_commit,
            &object_format,
            "prepared Git result",
            &empty_shallow_file,
        )?;

        let current = self.exact_ref_oid(ref_name, Some(&object_format))?;
        let outcome = if current.as_deref() == Some(result_commit) {
            GitRefInstallOutcome::Adopted
        } else if current.as_deref() == expected_ref {
            let old = match expected_ref {
                Some(expected) => expected.to_string(),
                None => "0".repeat(object_id_hex_len(&object_format)?),
            };
            let output = self
                .git_cmd()
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .args([
                    "update-ref",
                    "--no-deref",
                    "-m",
                    &keep_message,
                    ref_name,
                    result_commit,
                    &old,
                ])
                .output()
                .context("Failed to compare-and-swap prepared Git result ref")?;
            if !output.status.success() {
                return Err(RailError::Git(GitError::CommandFailed {
                    command: "git update-ref <exact-branch> <result> <expected>".to_string(),
                    stderr: git_command_diagnostics(&output.stdout, &output.stderr),
                }));
            }
            GitRefInstallOutcome::Updated
        } else {
            return Err(RailError::message(format!(
                "prepared Git ref '{}' changed during object installation: expected {} or {}, found {}",
                ref_name,
                expected_ref.unwrap_or("absent"),
                result_commit,
                current.as_deref().unwrap_or("absent")
            )));
        };
        if self.exact_ref_oid(ref_name, Some(&object_format))?.as_deref() != Some(result_commit) {
            return Err(RailError::message(format!(
                "prepared Git ref '{}' did not retain exact result {}",
                ref_name, result_commit
            )));
        }
        if keep_owned {
            remove_owned_pack_keep(&keep_path, &keep_message)?;
        }
        Ok(outcome)
    }

    /// Reconstruct and verify every field of one journaled commit object.
    ///
    /// The object ID alone is a cryptographic binding, but an explicit raw
    /// comparison keeps the recovery contract auditable and prevents callers
    /// from accidentally validating only the tree or only the commit type.
    pub(crate) fn verify_prepared_commit(&self, expected: &GitCommitEffect) -> RailResult<()> {
        let object_format = self.object_format()?;
        validate_object_format(&object_format)?;
        validate_exact_object_id(expected.oid(), &object_format)?;
        self.require_exact_commit(expected.oid(), &object_format)?;
        let mut command = self.git_cmd();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(["cat-file", "commit", expected.oid()]);
        verify_prepared_commit_output(command, expected)
    }

    /// Observe one exact direct branch ref without resolving symbolic or
    /// broken refs and without treating an arbitrary Git failure as absence.
    pub(crate) fn exact_branch_ref_oid(&self, ref_name: &str) -> RailResult<Option<String>> {
        validate_branch_ref_name(ref_name)?;
        self.exact_ref_oid(ref_name, None)
    }

    pub(crate) fn exact_branch_ref_oid_with_format(
        &self,
        ref_name: &str,
        object_format: &str,
    ) -> RailResult<Option<String>> {
        validate_branch_ref_name(ref_name)?;
        validate_object_format(object_format)?;
        self.exact_ref_oid(ref_name, Some(object_format))
    }

    fn exact_ref_oid(&self, ref_name: &str, object_format: Option<&str>) -> RailResult<Option<String>> {
        let output = self
            .git_cmd()
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args([
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(symref)%00",
                "--",
                ref_name,
            ])
            .output()
            .context("Failed to inspect exact prepared Git ref")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git for-each-ref <exact-branch>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let mut found = None;
        for record in output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|record| !record.is_empty())
        {
            let fields = record.split(|byte| *byte == 0).collect::<Vec<_>>();
            if fields.len() != 4 || !fields[3].is_empty() {
                return Err(RailError::message("Git returned an invalid exact branch record"));
            }
            if fields[0] != ref_name.as_bytes() || found.is_some() {
                return Err(RailError::message(format!(
                    "Git returned an unexpected ref while inspecting exact branch '{ref_name}'"
                )));
            }
            if !fields[2].is_empty() {
                return Err(RailError::message(format!(
                    "prepared Git branch ref '{ref_name}' is symbolic"
                )));
            }
            let object_id = std::str::from_utf8(fields[1])?;
            if let Some(object_format) = object_format {
                validate_exact_object_id(object_id, object_format)?;
            } else {
                validate_exact_object_id(object_id, object_format_for_id(object_id)?)?;
            }
            found = Some(object_id.to_string());
        }
        Ok(found)
    }

    fn require_exact_commit(&self, object_id: &str, object_format: &str) -> RailResult<()> {
        validate_exact_object_id(object_id, object_format)?;
        let output = self
            .git_cmd()
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(["cat-file", "-t", object_id])
            .output()
            .context("Failed to inspect prepared Git result type")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git cat-file -t <prepared-result>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        if output.stdout != b"commit\n" {
            return Err(RailError::message(format!(
                "prepared Git result {object_id} is not an exact commit object"
            )));
        }
        Ok(())
    }

    fn require_complete_local_closure(
        &self,
        object_id: &str,
        object_format: &str,
        context: &str,
        empty_shallow_file: &Path,
    ) -> RailResult<()> {
        let mut command = self.git_cmd();
        command
            .env("GIT_SHALLOW_FILE", empty_shallow_file)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        require_complete_object_closure(command, object_id, object_format, context)
    }

    /// Canonical Git common directory shared by all linked worktrees.
    pub(crate) fn common_dir(&self) -> RailResult<PathBuf> {
        let value = self.run_git_stdout(&["rev-parse", "--git-common-dir"])?;
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            self.repo_path.join(path)
        };
        Ok(utils::canonicalize_existing(&path)?)
    }

    /// Materialize one immutable Git tree into an empty directory without
    /// changing repository state or creating a Git worktree.
    pub(crate) fn materialize_tree(&self, commit_sha: &str, destination: &Path) -> RailResult<()> {
        let destination_metadata = std::fs::symlink_metadata(destination).map_err(|error| {
            RailError::message(format!(
                "failed to inspect historical planning root '{}': {error}",
                destination.display()
            ))
        })?;
        if !destination_metadata.file_type().is_dir() {
            return Err(RailError::message(format!(
                "historical planning root '{}' is not a directory",
                destination.display()
            )));
        }
        if std::fs::read_dir(destination)?.next().is_some() {
            return Err(RailError::message(format!(
                "historical planning root '{}' is not empty",
                destination.display()
            )));
        }

        let entries = self.collect_tree_entries_for_paths(commit_sha, &[PathBuf::from(".")])?;
        let object_ids = entries.iter().map(|entry| entry.object_id.as_str()).collect::<Vec<_>>();
        let blobs = self.read_blobs_bulk(&object_ids)?;
        for (entry, bytes) in entries.into_iter().zip(blobs) {
            let relative = crate::source::RepositoryPath::new(&entry.path)?;
            let path = destination.join(relative.as_path());
            let parent = path
                .parent()
                .ok_or_else(|| RailError::message(format!("historical tree path '{}' has no parent", relative)))?;
            std::fs::create_dir_all(parent).map_err(|error| {
                RailError::message(format!(
                    "failed to create historical tree directory '{}': {error}",
                    parent.display()
                ))
            })?;
            match entry.mode.as_str() {
                "100644" | "100755" => {
                    use std::io::Write as _;

                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|error| {
                            RailError::message(format!(
                                "failed to create historical tree file '{}': {error}",
                                path.display()
                            ))
                        })?;
                    file.write_all(&bytes).map_err(|error| {
                        RailError::message(format!(
                            "failed to write historical tree file '{}': {error}",
                            path.display()
                        ))
                    })?;
                    #[cfg(unix)]
                    if entry.mode == "100755" {
                        use std::os::unix::fs::PermissionsExt as _;

                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
                    }
                }
                "120000" => materialize_historical_symlink(destination, &path, &bytes)?,
                "160000" => {
                    return Err(RailError::with_help(
                        format!("historical planning does not support Git submodule '{}'", relative),
                        "select a revision whose Cargo workspace is fully represented by repository files",
                    ));
                }
                mode => {
                    return Err(RailError::message(format!(
                        "historical tree entry '{}' has unsupported Git mode '{mode}'",
                        relative
                    )));
                }
            }
        }
        Ok(())
    }

    /// Normalize a path to be relative to the work tree
    ///
    /// If the path is absolute, resolve both representations before stripping the
    /// worktree prefix. This handles Windows drive casing, separators, and the
    /// verbatim prefix returned by `canonicalize` without accepting outside paths.
    fn normalize_path(&self, path: &Path) -> RailResult<PathBuf> {
        if path.is_absolute() {
            utils::path_relative_to(&self.worktree_root, path).map_err(|error| {
                RailError::message(format!(
                    "failed to make '{}' relative to git worktree '{}': {}",
                    path.display(),
                    self.worktree_root.display(),
                    error
                ))
            })
        } else {
            Ok(path.to_path_buf())
        }
    }

    /// Get commit history from HEAD with optional limit
    ///
    /// Returns commits in reverse chronological order (newest first).
    /// Loads commit records in one bounded batch stream.
    pub fn commit_history(&self, limit: Option<usize>) -> RailResult<Vec<CommitInfo>> {
        let mut args = vec!["log", "--format=%H"];
        let limit_str;
        if let Some(max) = limit {
            limit_str = format!("-{}", max);
            args.push(&limit_str);
        }

        let output = self.run_git(&args)?;
        let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Fetch commit info in parallel using bulk operation
        self.get_commits_bulk(&shas)
    }

    /// Get commits reachable from ordinary local, remote, and tag refs.
    ///
    /// Notes are intentionally excluded: origin recovery must work from normal
    /// refs, including a local sync branch that is not currently checked out.
    pub fn ordinary_commit_history(&self) -> RailResult<Vec<CommitInfo>> {
        let output = self.run_git(&["rev-list", "--branches", "--remotes", "--tags"])?;
        let mut seen = std::collections::HashSet::new();
        let shas = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .filter(|sha| seen.insert((*sha).to_string()))
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.get_commits_bulk(&shas)
    }

    /// Get commits reachable from one exact selected history head.
    pub(crate) fn ordinary_commit_history_at(&self, revision: &str) -> RailResult<Vec<CommitInfo>> {
        let output = self.run_git(&["rev-list", revision])?;
        let shas = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.get_commits_bulk(&shas)
    }

    /// Get files changed in a specific commit
    ///
    /// Returns list of (path, change_type) where change_type is A(dded), M(odified), D(eleted),
    /// R(enamed), or C(opied).
    ///
    /// For renames and copies, both the old and new paths are returned:
    /// - Old path with 'D' (deleted from old location)
    /// - New path with 'A' (added at new location)
    pub fn get_changed_files(&self, commit_sha: &str) -> RailResult<Vec<(PathBuf, char)>> {
        let output = self.run_git(&["diff-tree", "--no-commit-id", "--name-status", "-r", "-z", commit_sha])?;
        parse_name_status_output_z(&output.stdout)
    }

    /// Get per-commit path changes through one bounded `diff-tree --stdin` stream.
    pub(crate) fn get_changed_files_bulk(&self, commit_shas: &[String]) -> RailResult<Vec<Vec<(PathBuf, char)>>> {
        use std::io::Write as _;
        use std::process::Stdio;

        if commit_shas.is_empty() {
            return Ok(Vec::new());
        }
        crate::instrumentation::record_git_path_change_batch(commit_shas.len());
        let mut command = self.git_cmd();
        command
            .args([
                "diff-tree",
                "--stdin",
                "--root",
                "--cc",
                "--no-renames",
                "--name-status",
                "-r",
                "-z",
                "--format=cargo-rail-commit:%H",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to start batched git diff-tree")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("git diff-tree stdin was unavailable"))?;
        for commit in commit_shas {
            stdin
                .write_all(commit.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .context("Failed to write batched git diff-tree input")?;
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .context("Failed to finish batched git diff-tree")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git diff-tree --stdin".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }

        let records = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        let mut index = 0;
        let mut results = Vec::with_capacity(commit_shas.len());
        for expected in commit_shas {
            let commit = records
                .get(index)
                .ok_or_else(|| RailError::message(format!("git diff-tree omitted commit '{expected}'")))?;
            let expected_marker = format!("cargo-rail-commit:{expected}");
            if *commit != expected_marker.as_bytes() {
                return Err(RailError::message(format!(
                    "git diff-tree returned commit '{}' while '{}' was expected",
                    String::from_utf8_lossy(commit),
                    expected
                )));
            }
            index += 1;
            let start = index;
            while index < records.len() && !records[index].starts_with(b"cargo-rail-commit:") {
                if index + 1 >= records.len() {
                    return Err(RailError::message(
                        "git diff-tree returned an incomplete status/path pair",
                    ));
                }
                index += 2;
            }
            results.push(parse_name_status_records(&records[start..index])?);
        }
        if index != records.len() {
            return Err(RailError::message("git diff-tree returned unexpected trailing records"));
        }
        Ok(results)
    }

    /// Get all files that changed between two refs.
    ///
    /// Returns list of (path, change_type) where change_type is A(dded), M(odified), D(eleted),
    /// R(enamed), or C(opied).
    ///
    /// For renames and copies, both the old and new paths are returned:
    /// - Old path with 'D' (deleted from old location)
    /// - New path with 'A' (added at new location)
    ///
    /// Uses `git diff --name-status` to list changes without reading file contents.
    pub fn get_changed_files_between(
        &self,
        base_ref: &str,
        head_ref: Option<&str>,
    ) -> RailResult<Vec<(PathBuf, char)>> {
        let mut args = vec!["diff", "--name-status", "-z", "--end-of-options", base_ref];
        if let Some(head) = head_ref {
            args.push(head);
        }
        args.push("--");
        let output = self.run_git(&args)?;
        parse_name_status_output_z(&output.stdout)
    }

    /// Get the merge-base (common ancestor) between two refs
    ///
    /// This is useful for finding the point where a feature branch diverged
    /// from the main branch, which gives more accurate change detection in CI.
    pub fn get_merge_base(&self, ref1: &str, ref2: &str) -> RailResult<String> {
        let output = self.run_git(&["merge-base", ref1, ref2])?;
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Err(crate::error::RailError::message(format!(
                "no common ancestor between {} and {}",
                ref1, ref2
            )));
        }
        Ok(sha)
    }

    /// Get commits touching a specific path in a range
    ///
    /// Returns commits in chronological order (oldest first).
    pub fn get_commits_touching_path(
        &self,
        path: &Path,
        since_sha: Option<&str>,
        until_ref: &str,
    ) -> RailResult<Vec<CommitInfo>> {
        let relative_path = self.normalize_path(path)?;
        let git_path = utils::path_to_git_format(&relative_path);

        let mut args = vec!["log", "--reverse", "--format=%H"];
        let range_arg;

        // Add range
        if let Some(since) = since_sha {
            range_arg = format!("{}..{}", since, until_ref);
            args.push(&range_arg);
        } else {
            args.push(until_ref);
        }

        // Add path filter
        args.push("--");
        args.push(&git_path);

        let output = self.run_git(&args)?;
        let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        self.get_commits_bulk(&shas)
    }

    /// Get commits touching any of the given paths (batched for performance)
    /// Returns commits in chronological order (oldest first), deduplicated
    pub fn get_commits_touching_paths(
        &self,
        paths: &[PathBuf],
        since_sha: Option<&str>,
        until_ref: &str,
    ) -> RailResult<Vec<CommitInfo>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Normalize all paths
        let relative_paths: Vec<String> = paths
            .iter()
            .map(|path| {
                self.normalize_path(path)
                    .map(|relative| utils::path_to_git_format(&relative))
            })
            .collect::<RailResult<_>>()?;

        let mut args = vec!["log", "--reverse", "--format=%H"];
        let range_arg;

        // Add range
        if let Some(since) = since_sha {
            range_arg = format!("{}..{}", since, until_ref);
            args.push(&range_arg);
        } else {
            args.push(until_ref);
        }

        // Add all path filters
        args.push("--");
        let path_refs: Vec<&str> = relative_paths.iter().map(|s| s.as_str()).collect();
        args.extend(path_refs);

        let output = self.run_git(&args)?;
        let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        self.get_commits_bulk(&shas)
    }

    /// Get path commits reachable from `until_ref` but not from any excluded commit.
    ///
    /// Exclusions are streamed through stdin so mapping history cannot exceed the
    /// platform's process argument limit. Git's reachability walk keeps this correct
    /// for merged histories where vector position is not an ancestry boundary.
    pub(crate) fn get_commits_touching_paths_excluding(
        &self,
        paths: &[PathBuf],
        excluded_commits: &[&str],
        until_ref: &str,
    ) -> RailResult<Vec<CommitInfo>> {
        use std::io::Write as _;
        use std::process::Stdio;

        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if excluded_commits.is_empty() {
            return self.get_commits_touching_paths(paths, None, until_ref);
        }

        let relative_paths = paths
            .iter()
            .map(|path| {
                self.normalize_path(path)
                    .map(|relative| utils::path_to_git_format(&relative))
            })
            .collect::<RailResult<Vec<_>>>()?;
        let mut command = self.git_cmd();
        command
            .args(["log", "--reverse", "--format=%H", "--stdin", until_ref, "--"])
            .args(&relative_paths)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("Failed to start mapped-ancestor Git history walk")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("mapped-ancestor Git history stdin was unavailable"))?;
        for commit in excluded_commits {
            stdin
                .write_all(b"^")
                .and_then(|()| stdin.write_all(commit.as_bytes()))
                .and_then(|()| stdin.write_all(b"\n"))
                .context("Failed to write mapped-ancestor Git history exclusions")?;
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .context("Failed to finish mapped-ancestor Git history walk")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git log --stdin <mapped-ancestor exclusions> -- <paths>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let shas = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.get_commits_bulk(&shas)
    }

    /// Get every commit reachable from `until_ref` but not from any excluded
    /// ancestry frontier. Callers can then apply stable path-policy selectors
    /// to each commit's exact add/delete set without reducing selectors to the
    /// files present in today's checkout.
    pub(crate) fn get_commits_excluding(
        &self,
        excluded_commits: &[&str],
        until_ref: &str,
    ) -> RailResult<Vec<CommitInfo>> {
        use std::io::Write as _;
        use std::process::Stdio;

        let mut command = self.git_cmd();
        command
            .args(["rev-list", "--reverse", "--stdin", until_ref])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("Failed to start stable-selector Git history walk")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("stable-selector Git history stdin was unavailable"))?;
        for commit in excluded_commits {
            stdin
                .write_all(b"^")
                .and_then(|()| stdin.write_all(commit.as_bytes()))
                .and_then(|()| stdin.write_all(b"\n"))
                .context("Failed to write stable-selector ancestry exclusion")?;
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .context("Failed to finish stable-selector Git history walk")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git rev-list --reverse --stdin".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let shas = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.get_commits_bulk(&shas)
    }

    /// One-pass commit log with per-commit changed files
    ///
    /// Returns entries newest-first for `from..to` (or the full history of
    /// `to` when `from` is `None`), skipping merge commits. One git subprocess
    /// regardless of how many crates consume the result — this is the
    /// changelog-attribution workhorse.
    pub fn log_with_files(&self, from: Option<&str>, to: &str) -> RailResult<Vec<LogEntry>> {
        let range;
        let range_arg = if let Some(from) = from {
            range = format!("{}..{}", from, to);
            &range
        } else {
            to
        };

        let output = self.run_git(&[
            "log",
            "--no-merges",
            "-z",
            "--name-only",
            "--format=%x01%H%x02%s%x02%b%x02",
            range_arg,
        ])?;

        parse_log_with_files(&output.stdout)
    }

    /// Get commit metadata for a single SHA
    ///
    /// Uses `git log -1 --format` for efficient single-commit lookup.
    pub fn get_commit(&self, sha: &str) -> RailResult<CommitInfo> {
        // Format: %H (hash) %an (author name) %ae (author email) %at (author time)
        //         %cn (committer name) %ce (committer email) %ct (committer time)
        //         %P (parent hashes) %B (body)
        let format = "%H%n%an%n%ae%n%at%n%ai%n%cn%n%ce%n%ct%n%ci%n%P%n%B";
        let format_arg = format!("--format={}", format);

        let output = self
            .run_git(&["log", "-1", &format_arg, sha])
            .map_err(|error| match error {
                RailError::Git(GitError::CommandFailed { .. }) => {
                    RailError::Git(GitError::CommitNotFound { sha: sha.to_string() })
                }
                other => other,
            })?;

        parse_commit_output(&output.stdout)
    }

    /// List all files at a specific commit under a path
    pub fn list_files_at_commit(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<PathBuf>> {
        let spec = if path.as_os_str().is_empty() {
            commit_sha.to_string()
        } else {
            let git_path = utils::path_to_git_format(path);
            format!("{}:{}", commit_sha, git_path)
        };

        // Use run_git_check since failure is not an error (empty result)
        if !self.run_git_check(&["ls-tree", "-r", "--name-only", &spec]) {
            return Ok(vec![]);
        }

        // If successful, get the output
        let output = self.run_git(&["ls-tree", "-r", "--name-only", &spec])?;
        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(PathBuf::from)
            .collect();

        Ok(files)
    }

    /// Collect all files from a tree recursively.
    pub fn collect_tree_files(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<(PathBuf, Vec<u8>)>> {
        let files = self.list_files_at_commit(commit_sha, path)?;

        if files.is_empty() {
            return Ok(vec![]);
        }

        // Build paths first (owned), then create references for bulk API
        let paths: Vec<PathBuf> = files.iter().map(|file| path.join(file)).collect();
        let items: Vec<(&str, &Path)> = paths.iter().map(|p| (commit_sha, p.as_path())).collect();

        // Read all files in one batch.
        let contents = self.read_files_bulk(&items)?;

        // Combine full paths (with crate prefix) with contents
        let results: Vec<(PathBuf, Vec<u8>)> = paths.into_iter().zip(contents).collect();

        Ok(results)
    }

    /// Read exact tree entries under `path` without materializing the worktree.
    pub(crate) fn collect_tree_entries(&self, commit_sha: &str, path: &Path) -> RailResult<Vec<GitTreeEntry>> {
        self.collect_tree_entries_for_paths(commit_sha, &[path.to_path_buf()])
    }

    /// Collect exact entries for many paths in one `ls-tree` subprocess.
    pub(crate) fn collect_tree_entries_for_paths(
        &self,
        commit_sha: &str,
        paths: &[PathBuf],
    ) -> RailResult<Vec<GitTreeEntry>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let normalized = paths
            .iter()
            .map(|path| self.normalize_path(path).map(|path| utils::path_to_git_format(&path)))
            .collect::<RailResult<Vec<_>>>()?;
        let mut command = self.git_cmd();
        command
            .env_remove("GIT_GLOB_PATHSPECS")
            .env_remove("GIT_NOGLOB_PATHSPECS")
            .env_remove("GIT_ICASE_PATHSPECS")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .args(["ls-tree", "-r", "-z", "--full-tree", commit_sha, "--"]);
        command.args(&normalized);
        let output = command.output().context("Failed to collect exact Git tree entries")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git ls-tree".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let mut entries = Vec::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| RailError::message("invalid git ls-tree record: missing path separator"))?;
            let metadata = String::from_utf8_lossy(&record[..tab]);
            let mut fields = metadata.split_whitespace();
            let mode = fields
                .next()
                .ok_or_else(|| RailError::message("invalid git ls-tree record: missing mode"))?;
            let _kind = fields
                .next()
                .ok_or_else(|| RailError::message("invalid git ls-tree record: missing object kind"))?;
            let object_id = fields
                .next()
                .ok_or_else(|| RailError::message("invalid git ls-tree record: missing object ID"))?;
            if fields.next().is_some() {
                return Err(RailError::message("invalid git ls-tree record: unexpected metadata"));
            }
            let path = std::str::from_utf8(&record[tab + 1..])
                .map_err(|_| RailError::message("git tree path is not valid UTF-8"))?;
            entries.push(GitTreeEntry {
                mode: mode.to_string(),
                object_id: object_id.to_string(),
                path: PathBuf::from(path),
            });
        }
        Ok(entries)
    }

    /// Read one exact path image from a commit tree.
    pub(crate) fn tree_entry(&self, commit: &str, path: &Path) -> RailResult<Option<GitTreeEntry>> {
        let normalized = RepositoryPath::new(path)?.as_path().to_path_buf();
        let mut entries = self.collect_tree_entries_for_paths(commit, std::slice::from_ref(&normalized))?;
        entries.retain(|entry| entry.path == normalized);
        match entries.len() {
            0 => Ok(None),
            1 => Ok(entries.pop()),
            _ => Err(RailError::message(format!(
                "Git tree contains repeated exact path '{}'",
                normalized.display()
            ))),
        }
    }

    /// Read one exact stage-zero index image, rejecting unmerged stages.
    pub(crate) fn index_entry(&self, path: &Path) -> RailResult<Option<GitTreeEntry>> {
        let normalized = RepositoryPath::new(path)?.as_path().to_path_buf();
        Ok(self
            .index_entries(std::slice::from_ref(&normalized))?
            .remove(&normalized)
            .flatten())
    }

    fn index_entries(
        &self,
        paths: &[PathBuf],
    ) -> RailResult<std::collections::BTreeMap<PathBuf, Option<GitTreeEntry>>> {
        let mut normalized = paths
            .iter()
            .map(|path| RepositoryPath::new(path).map(|path| path.as_path().to_path_buf()))
            .collect::<RailResult<Vec<_>>>()?;
        normalized.sort();
        if normalized.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RailError::message("exact Git index capture repeats a path"));
        }
        let mut results = normalized
            .iter()
            .cloned()
            .map(|path| (path, None))
            .collect::<std::collections::BTreeMap<_, _>>();
        if normalized.is_empty() {
            return Ok(results);
        }
        let mut command = self.git_cmd();
        command
            .env_remove("GIT_GLOB_PATHSPECS")
            .env_remove("GIT_NOGLOB_PATHSPECS")
            .env_remove("GIT_ICASE_PATHSPECS")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .args(["ls-files", "-v", "--stage", "-z", "--"])
            .args(&normalized);
        let output = command.output().context("Failed to inspect exact Git index path")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git ls-files --stage -- <exact-path>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| RailError::message("invalid exact Git index record"))?;
            let record_path = PathBuf::from(std::str::from_utf8(&record[tab + 1..])?);
            let result = results.get_mut(&record_path).ok_or_else(|| {
                RailError::message(format!(
                    "exact Git index capture returned unexpected path '{}'",
                    record_path.display()
                ))
            })?;
            let flag = *record
                .first()
                .ok_or_else(|| RailError::message("invalid exact Git index record: missing state flag"))?;
            if record.get(1) != Some(&b' ') {
                return Err(RailError::message(
                    "invalid exact Git index record: missing state-flag separator",
                ));
            }
            let header = std::str::from_utf8(&record[2..tab])?;
            let mut fields = header.split_whitespace();
            let mode = fields.next().unwrap_or_default();
            let object_id = fields.next().unwrap_or_default();
            let stage = fields.next().unwrap_or_default();
            if fields.next().is_some() || stage != "0" || result.is_some() {
                return Err(RailError::message(format!(
                    "exact Git index path '{}' is unmerged or repeated",
                    record_path.display()
                )));
            }
            if flag != b'H' {
                return Err(RailError::with_help(
                    format!(
                        "exact Git index path '{}' has unsupported state flag '{}'",
                        record_path.display(),
                        char::from(flag)
                    ),
                    "clear assume-unchanged and skip-worktree state before retrying; cargo-rail will not guess hidden index authority",
                ));
            }
            if mode == "160000" {
                return Err(RailError::with_help(
                    format!("exact Git index path '{}' is a submodule", record_path.display()),
                    "prepared Git path effects do not materialize submodule worktrees",
                ));
            }
            if !matches!(mode, "100644" | "100755" | "120000") {
                return Err(RailError::message(format!(
                    "exact Git index path '{}' has unsupported mode '{mode}'",
                    record_path.display()
                )));
            }
            *result = Some(GitTreeEntry {
                mode: mode.to_string(),
                object_id: object_id.to_string(),
                path: record_path,
            });
        }
        let intent_to_add = self.index_paths_with_intent_to_add(&normalized)?;
        for path in intent_to_add {
            if results.get(&path).is_some_and(Option::is_some) {
                return Err(RailError::with_help(
                    format!("exact Git index path '{}' has intent-to-add state", path.display()),
                    "stage exact contents or remove the intent-to-add entry before retrying; cargo-rail requires a complete stage-zero index image",
                ));
            }
        }
        Ok(results)
    }

    fn index_paths_with_intent_to_add(&self, paths: &[PathBuf]) -> RailResult<std::collections::BTreeSet<PathBuf>> {
        if paths.is_empty() {
            return Ok(std::collections::BTreeSet::new());
        }
        let mut command = self.git_cmd();
        command
            .env_remove("GIT_GLOB_PATHSPECS")
            .env_remove("GIT_NOGLOB_PATHSPECS")
            .env_remove("GIT_ICASE_PATHSPECS")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["diff-files", "--raw", "-z", "--no-abbrev", "--no-renames", "--"])
            .args(paths);
        let output = command.output().context("Failed to inspect exact Git index intent")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git diff-files --raw -- <exact-path>".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let records = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        if !records.len().is_multiple_of(2) {
            return Err(RailError::message("Git returned an invalid exact index-intent record"));
        }
        let expected = paths.iter().collect::<std::collections::BTreeSet<_>>();
        let mut intent_to_add = std::collections::BTreeSet::new();
        for pair in records.as_chunks::<2>().0 {
            let path = PathBuf::from(std::str::from_utf8(pair[1])?);
            if !expected.contains(&path) {
                return Err(RailError::message("Git returned an unexpected exact index-intent path"));
            }
            let header = std::str::from_utf8(pair[0])?;
            let mut fields = header.split_whitespace();
            let old_mode = fields.next().unwrap_or_default().strip_prefix(':').unwrap_or_default();
            let _new_mode = fields.next().unwrap_or_default();
            let old_object = fields.next().unwrap_or_default();
            let _new_object = fields.next().unwrap_or_default();
            let _status = fields.next().unwrap_or_default();
            if fields.next().is_some() {
                return Err(RailError::message("Git returned invalid exact index-intent metadata"));
            }
            if old_mode == "000000" && !old_object.is_empty() && old_object.bytes().all(|byte| byte == b'0') {
                intent_to_add.insert(path);
            }
        }
        Ok(intent_to_add)
    }

    /// Capture exact index and worktree images for a canonical path set with
    /// one index query, one intent query, and one file-mode query.
    pub(crate) fn exact_path_images(&self, paths: &[PathBuf]) -> RailResult<Vec<GitPathImages>> {
        let index = self.index_entries(paths)?;
        #[cfg(unix)]
        let trusts_file_mode = if self.exact_paths_include_regular_file(paths)? {
            self.trusts_file_mode()?
        } else {
            false
        };
        #[cfg(not(unix))]
        let trusts_file_mode = false;
        paths
            .iter()
            .map(|path| {
                let normalized = RepositoryPath::new(path)?.as_path().to_path_buf();
                let indexed = index.get(&normalized).cloned().flatten();
                let worktree = self.worktree_entry_from_index(&normalized, indexed.as_ref(), trusts_file_mode)?;
                Ok(GitPathImages {
                    index: indexed,
                    worktree,
                })
            })
            .collect()
    }

    #[cfg(unix)]
    fn exact_paths_include_regular_file(&self, paths: &[PathBuf]) -> RailResult<bool> {
        for path in paths {
            let normalized = RepositoryPath::new(path)?.as_path().to_path_buf();
            reject_symlink_or_reparse_ancestors(&self.worktree_root, &normalized)?;
            match std::fs::symlink_metadata(self.worktree_root.join(&normalized)) {
                Ok(metadata) if metadata.file_type().is_file() => return Ok(true),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    /// Hash one exact worktree path without trusting assume-unchanged,
    /// skip-worktree, or cached stat information.
    pub(crate) fn worktree_entry(&self, path: &Path) -> RailResult<Option<GitTreeEntry>> {
        let normalized = RepositoryPath::new(path)?.as_path().to_path_buf();
        let indexed = self.index_entry(&normalized)?;
        let trusts_file_mode = self.trusts_file_mode()?;
        self.worktree_entry_from_index(&normalized, indexed.as_ref(), trusts_file_mode)
    }

    fn worktree_entry_from_index(
        &self,
        normalized: &Path,
        indexed: Option<&GitTreeEntry>,
        _trusts_file_mode: bool,
    ) -> RailResult<Option<GitTreeEntry>> {
        reject_symlink_or_reparse_ancestors(&self.worktree_root, normalized)?;
        let absolute = self.worktree_root.join(normalized);
        let metadata = match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.is_dir() {
            return Ok(None);
        }
        let (mode, object_id) = if metadata.file_type().is_file() {
            let (bytes, _opened_metadata) = read_regular_file_nofollow(&self.worktree_root, normalized)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt as _;
                if _trusts_file_mode {
                    if _opened_metadata.permissions().mode() & 0o100 == 0 {
                        "100644"
                    } else {
                        "100755"
                    }
                } else {
                    indexed
                        .and_then(|entry| {
                            matches!(entry.mode.as_str(), "100644" | "100755").then_some(entry.mode.as_str())
                        })
                        .unwrap_or("100644")
                }
            };
            #[cfg(not(unix))]
            let mode = indexed
                .and_then(|entry| matches!(entry.mode.as_str(), "100644" | "100755").then_some(entry.mode.as_str()))
                .unwrap_or("100644");
            #[cfg(windows)]
            if indexed.is_some_and(|entry| entry.mode == "120000") {
                return Ok(Some(GitTreeEntry {
                    mode: "120000".to_string(),
                    object_id: self.hash_bytes(&bytes)?,
                    path: normalized.to_path_buf(),
                }));
            }
            let git_path = normalized.to_string_lossy().replace('\\', "/");
            (mode.to_string(), self.hash_path_bytes(&git_path, &bytes)?)
        } else if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&absolute)?;
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::ffi::OsStrExt as _;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().as_bytes().to_vec();
            ("120000".to_string(), self.hash_bytes(&bytes)?)
        } else {
            return Err(RailError::message(format!(
                "exact Git worktree path '{}' is not a regular file or symlink",
                normalized.display()
            )));
        };
        Ok(Some(GitTreeEntry {
            mode,
            object_id,
            path: normalized.to_path_buf(),
        }))
    }

    /// Reconcile the real index and worktree from one exact prepared tree
    /// transition after the branch ref has reached `result`.
    ///
    /// Every changed path must be in either its exact old or exact new image;
    /// a third state is preserved and rejected. Git's non-forcing two-tree
    /// checkout performs the final materialization and independently refuses
    /// a concurrent tracked or untracked overwrite.
    pub(crate) fn reconcile_prepared_commit_paths(
        &self,
        expected: Option<&str>,
        result: &str,
        changed_paths: &[PathBuf],
    ) -> RailResult<()> {
        let object_format = self.object_format()?;
        validate_object_format(&object_format)?;
        validate_exact_object_id(result, &object_format)?;
        if let Some(expected) = expected {
            validate_exact_object_id(expected, &object_format)?;
        }
        let mut paths = changed_paths
            .iter()
            .map(|path| RepositoryPath::new(path).map(|path| path.as_path().to_path_buf()))
            .collect::<RailResult<Vec<_>>>()?;
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RailError::message("prepared Git path reconciliation repeats a path"));
        }
        let old_entries = expected
            .map(|expected| self.collect_tree_entries_for_paths(expected, &paths))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let new_entries = self
            .collect_tree_entries_for_paths(result, &paths)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let actual_paths = if expected.is_some() {
            self.changed_paths_between(expected, result)?
        } else {
            new_entries.keys().cloned().collect()
        };
        if actual_paths != paths {
            return Err(RailError::message(
                "prepared Git path transition set does not match the exact old-to-new tree diff",
            ));
        }

        let allowed = paths.iter().cloned().collect::<std::collections::BTreeSet<_>>();
        let obstructing = self.obstructing_worktree_paths()?;
        if obstructing.iter().any(|path| {
            !allowed.contains(path)
                && allowed
                    .iter()
                    .any(|changed| path.starts_with(changed) || changed.starts_with(path))
        }) {
            return Err(RailError::with_help(
                "target gained an overlapping index, worktree, untracked, or ignored change during prepared recovery",
                "preserve the target bytes and resolve the overlapping path before retrying the exact prepared effect",
            ));
        }

        let mut all_new = true;
        let mut index_repairs = Vec::new();
        let current_images = self.exact_path_images(&paths)?;
        for (path, images) in paths.iter().zip(current_images) {
            let old = old_entries.get(path).cloned();
            let new = new_entries.get(path).cloned();
            if old == new {
                return Err(RailError::message(format!(
                    "prepared Git path '{}' has no exact tree transition",
                    path.display()
                )));
            }
            if old.as_ref().is_some_and(|entry| entry.mode == "160000")
                || new.as_ref().is_some_and(|entry| entry.mode == "160000")
            {
                return Err(RailError::message(format!(
                    "prepared Git path '{}' crosses an unsupported gitlink",
                    path.display()
                )));
            }
            let index_old = exact_entry_image_eq(images.index.as_ref(), old.as_ref());
            let index_new = exact_entry_image_eq(images.index.as_ref(), new.as_ref());
            let worktree_old = exact_entry_image_eq(images.worktree.as_ref(), old.as_ref());
            let worktree_new = exact_entry_image_eq(images.worktree.as_ref(), new.as_ref());
            if !(index_old || index_new) || !(worktree_old || worktree_new) {
                return Err(RailError::with_help(
                    format!(
                        "prepared Git path '{}' changed to a third index or worktree state",
                        path.display()
                    ),
                    "cargo-rail preserves the unexpected bytes; restore the exact old or prepared state before retrying",
                ));
            }
            if index_old && worktree_new {
                index_repairs.push((path.clone(), new.clone()));
            }
            all_new &= index_new && worktree_new;
        }
        if !index_repairs.is_empty() {
            self.update_index_images(&index_repairs, &object_format)?;
            all_new = self
                .exact_path_images(&paths)?
                .into_iter()
                .zip(&paths)
                .all(|(images, path)| {
                    let new = new_entries.get(path);
                    exact_entry_image_eq(images.index.as_ref(), new)
                        && exact_entry_image_eq(images.worktree.as_ref(), new)
                });
        }
        if all_new {
            return Ok(());
        }

        if let Some(expected) = expected {
            let mut tracked_old_paths = Vec::new();
            for path in &paths {
                if old_entries.contains_key(path) {
                    tracked_old_paths.push(path);
                }
            }
            if !tracked_old_paths.is_empty() {
                let mut refresh = self.git_cmd();
                refresh
                    .env_remove("GIT_GLOB_PATHSPECS")
                    .env_remove("GIT_NOGLOB_PATHSPECS")
                    .env_remove("GIT_ICASE_PATHSPECS")
                    .env("GIT_LITERAL_PATHSPECS", "1")
                    .env("GIT_NO_LAZY_FETCH", "1")
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
                    .args(["update-index", "--refresh", "--ignore-missing", "--"])
                    .args(&tracked_old_paths);
                let output = refresh.output().context("Failed to refresh prepared Git index paths")?;
                if !output.status.success() {
                    return Err(RailError::Git(GitError::CommandFailed {
                        command: "git update-index --refresh -- <exact-old-paths>".to_string(),
                        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
                    }));
                }
            }
            let output = self
                .git_cmd()
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .args(["read-tree", "-u", "-m", expected, result])
                .output()
                .context("Failed to materialize prepared Git tree transition")?;
            if !output.status.success() {
                return Err(RailError::Git(GitError::CommandFailed {
                    command: "git read-tree -u -m <old> <result>".to_string(),
                    stderr: git_command_diagnostics(&output.stdout, &output.stderr),
                }));
            }
        } else {
            // An unborn target has no old tree for a two-tree checkout. Seed
            // the index first, then materialize only still-absent files without
            // force. A concurrently created path is therefore preserved and
            // rejected by Git rather than overwritten by `--reset -u`.
            let indexed = self
                .git_cmd()
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .args(["read-tree", result])
                .output()
                .context("Failed to seed unborn prepared Git index")?;
            if !indexed.status.success() {
                return Err(RailError::Git(GitError::CommandFailed {
                    command: "git read-tree <result>".to_string(),
                    stderr: git_command_diagnostics(&indexed.stdout, &indexed.stderr),
                }));
            }
            let mut missing = Vec::new();
            for (path, images) in paths.iter().zip(self.exact_path_images(&paths)?) {
                let new = new_entries.get(path);
                if exact_entry_image_eq(images.worktree.as_ref(), new) {
                    continue;
                }
                if images.worktree.is_some() {
                    return Err(RailError::with_help(
                        format!(
                            "unborn prepared Git path '{}' appeared before materialization",
                            path.display()
                        ),
                        "cargo-rail preserved the path; remove or relocate it before retrying the exact prepared effect",
                    ));
                }
                missing.push(path.clone());
            }
            if !missing.is_empty() {
                let mut checkout = self.git_cmd();
                checkout
                    .env_remove("GIT_GLOB_PATHSPECS")
                    .env_remove("GIT_NOGLOB_PATHSPECS")
                    .env_remove("GIT_ICASE_PATHSPECS")
                    .env("GIT_LITERAL_PATHSPECS", "1")
                    .env("GIT_NO_LAZY_FETCH", "1")
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
                    .args(["checkout-index", "--"])
                    .args(&missing);
                let output = checkout
                    .output()
                    .context("Failed to materialize unborn prepared Git paths")?;
                if !output.status.success() {
                    return Err(RailError::Git(GitError::CommandFailed {
                        command: "git checkout-index -- <exact-paths>".to_string(),
                        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
                    }));
                }
            }
        }
        for (path, images) in paths.iter().zip(self.exact_path_images(&paths)?) {
            let new = new_entries.get(path);
            if !exact_entry_image_eq(images.index.as_ref(), new) || !exact_entry_image_eq(images.worktree.as_ref(), new)
            {
                return Err(RailError::message(format!(
                    "prepared Git path '{}' did not converge to its exact result image",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn update_index_images(&self, images: &[(PathBuf, Option<GitTreeEntry>)], object_format: &str) -> RailResult<()> {
        use std::io::Write as _;
        use std::process::Stdio;

        let zero = "0".repeat(object_id_hex_len(object_format)?);
        let mut input = Vec::new();
        for (path, entry) in images {
            let path = RepositoryPath::new(path)?;
            if let Some(entry) = entry {
                write!(input, "{} {}\t{}\0", entry.mode, entry.object_id, path.as_str())?;
            } else {
                write!(input, "0 {zero}\t{}\0", path.as_str())?;
            }
        }
        let mut command = self.git_cmd();
        command
            .env_remove("GIT_GLOB_PATHSPECS")
            .env_remove("GIT_NOGLOB_PATHSPECS")
            .env_remove("GIT_ICASE_PATHSPECS")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .args(["update-index", "-z", "--index-info"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to start exact prepared index repair")?;
        child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("exact prepared index repair stdin was unavailable"))?
            .write_all(&input)?;
        let output = child
            .wait_with_output()
            .context("Failed to finish exact prepared index repair")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git update-index -z --index-info".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        Ok(())
    }

    fn changed_paths_between(&self, expected: Option<&str>, result: &str) -> RailResult<Vec<PathBuf>> {
        let mut paths = if let Some(expected) = expected {
            let output = self
                .git_cmd()
                .env_remove("GIT_GLOB_PATHSPECS")
                .env_remove("GIT_NOGLOB_PATHSPECS")
                .env_remove("GIT_ICASE_PATHSPECS")
                .env("GIT_LITERAL_PATHSPECS", "1")
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .args([
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "--no-renames",
                    "-r",
                    "-z",
                    expected,
                    result,
                    "--",
                ])
                .output()
                .context("Failed to inspect prepared Git tree transition")?;
            if !output.status.success() {
                return Err(RailError::Git(GitError::CommandFailed {
                    command: "git diff-tree <old> <result>".to_string(),
                    stderr: git_command_diagnostics(&output.stdout, &output.stderr),
                }));
            }
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .map(|record| {
                    let path = std::str::from_utf8(record)?;
                    RepositoryPath::new(Path::new(path)).map(|path| path.as_path().to_path_buf())
                })
                .collect::<RailResult<Vec<_>>>()?
        } else {
            self.collect_tree_entries(result, Path::new("."))?
                .into_iter()
                .map(|entry| entry.path)
                .collect()
        };
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn trusts_file_mode(&self) -> RailResult<bool> {
        let output = self
            .git_cmd()
            .args(["config", "--bool", "--get", "core.filemode"])
            .output()
            .context("Failed to inspect Git file-mode policy")?;
        if output.status.success() {
            return match String::from_utf8(output.stdout)?.trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(RailError::message(format!(
                    "Git returned invalid core.filemode value '{other}'"
                ))),
            };
        }
        if output.status.code() == Some(1) {
            return Ok(cfg!(unix));
        }
        Err(RailError::Git(GitError::CommandFailed {
            command: "git config --bool --get core.filemode".to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        }))
    }

    /// Add a remote repository
    pub fn add_remote(&self, name: &str, url: &str) -> RailResult<()> {
        match self.run_git(&["remote", "add", name, url]) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Check if error is because remote already exists
                if let RailError::Git(GitError::CommandFailed { stderr, .. }) = &e
                    && stderr.contains("already exists")
                {
                    return Ok(());
                }
                Err(e)
            }
        }
    }

    /// List all remotes
    pub fn list_remotes(&self) -> RailResult<Vec<(String, String)>> {
        // Use run_git_check since failure returns empty list
        if !self.run_git_check(&["remote", "-v"]) {
            return Ok(vec![]);
        }

        let output = self.run_git(&["remote", "-v"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut remotes = Vec::new();

        for line in stdout.lines() {
            // Format: "origin  git@github.com:user/repo.git (fetch)"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && line.contains("(fetch)") {
                remotes.push((parts[0].to_string(), parts[1].to_string()));
            }
        }

        Ok(remotes)
    }

    /// Push to remote
    pub fn push_to_remote(&self, remote_name: &str, branch: &str) -> RailResult<()> {
        progress!("   Pushing to remote '{}'...", remote_name);

        if let Err(error) = self.run_git_observable(&["push", "-u", remote_name, branch]) {
            return match error {
                RailError::Git(GitError::CommandFailed { stderr, .. }) => Err(RailError::Git(GitError::PushFailed {
                    remote: remote_name.to_string(),
                    branch: branch.to_string(),
                    reason: stderr,
                })),
                other => Err(other),
            };
        }

        progress!("   ✅ Pushed to {}/{}", remote_name, branch);
        Ok(())
    }

    /// Publish one exact commit to an exact configured URL with an exact
    /// compare-and-swap expectation for the destination branch.
    ///
    /// An absent `expected_remote_head` means that the destination ref must
    /// still be absent. Passing the URL directly avoids trusting a mutable
    /// local remote name after the authority snapshot was captured.
    pub(crate) fn push_commit_to_url_with_lease(
        &self,
        remote_url: &str,
        destination_ref: &str,
        desired_commit: &str,
        expected_remote_head: Option<&str>,
    ) -> RailResult<()> {
        validate_branch_ref_name(destination_ref)?;
        let object_format = self.object_format()?;
        validate_object_format(&object_format)?;
        validate_exact_object_id(desired_commit, &object_format)?;
        if let Some(expected) = expected_remote_head {
            validate_exact_object_id(expected, &object_format)?;
        }
        self.require_exact_commit(desired_commit, &object_format)?;
        let lease = format!(
            "--force-with-lease={destination_ref}:{}",
            expected_remote_head.unwrap_or_default()
        );
        let refspec = format!("{desired_commit}:{destination_ref}");
        progress!("   Publishing the exact prepared target commit...");

        if let Err(error) = self.run_git_observable(&["push", &lease, remote_url, &refspec]) {
            return match error {
                RailError::Git(GitError::CommandFailed { stderr, .. }) => Err(RailError::Git(GitError::PushFailed {
                    remote: "configured remote URL".to_string(),
                    branch: destination_ref.to_string(),
                    reason: stderr,
                })),
                other => Err(other),
            };
        }

        progress!("   ✅ Published exact prepared target commit");
        Ok(())
    }

    /// Fetch from remote
    pub fn fetch_from_remote(&self, remote_name: &str) -> RailResult<()> {
        progress!("   Fetching from remote '{}'...", remote_name);

        self.run_git(&["fetch", remote_name])?;

        progress!("   ✅ Fetched from {}", remote_name);
        Ok(())
    }

    /// Check if remote exists
    pub fn has_remote(&self, name: &str) -> RailResult<bool> {
        let remotes = self.list_remotes()?;
        Ok(remotes.iter().any(|(n, _)| n == name))
    }

    /// Create a branch
    pub fn create_branch(&self, branch_name: &str) -> RailResult<()> {
        self.run_git(&["branch", branch_name])?;
        Ok(())
    }

    /// Checkout a branch
    pub fn checkout_branch(&self, branch_name: &str) -> RailResult<()> {
        self.run_git_observable(&["checkout", branch_name])?;
        Ok(())
    }

    /// Check if a local branch exists
    pub fn branch_exists(&self, branch_name: &str) -> RailResult<bool> {
        let ref_name = format!("refs/heads/{}", branch_name);
        Ok(self.run_git_check(&["show-ref", "--verify", "--quiet", &ref_name]))
    }

    /// Create and checkout a branch
    pub fn create_and_checkout_branch(&self, branch_name: &str) -> RailResult<()> {
        self.create_branch(branch_name)?;
        self.checkout_branch(branch_name)?;
        Ok(())
    }

    /// Read multiple files through one `git cat-file --batch` subprocess.
    ///
    /// Output order matches input order. Missing files produce empty byte vectors.
    pub fn read_files_bulk(&self, items: &[(&str, &Path)]) -> RailResult<Vec<Vec<u8>>> {
        if items.is_empty() {
            return Ok(vec![]);
        }

        let mut requests = Vec::with_capacity(items.len());
        for (commit_sha, path) in items {
            let relative_path = self.normalize_path(path)?;
            let git_path = utils::path_to_git_format(&relative_path);
            requests.push(format!("{}:{}", commit_sha, git_path));
        }
        self.read_batch_objects(&requests, true, b"blob")
    }

    /// Read exact blob object IDs in one `git cat-file --batch` subprocess.
    pub(crate) fn read_blobs_bulk(&self, object_ids: &[&str]) -> RailResult<Vec<Vec<u8>>> {
        let requests: Vec<String> = object_ids.iter().map(|object_id| (*object_id).to_string()).collect();
        self.read_batch_objects(&requests, false, b"blob")
    }

    fn read_batch_objects(
        &self,
        requests: &[String],
        missing_as_empty: bool,
        expected_type: &[u8],
    ) -> RailResult<Vec<Vec<u8>>> {
        use std::io::Write as _;
        use std::process::Stdio;

        if requests.is_empty() {
            return Ok(Vec::new());
        }
        crate::instrumentation::record_git_object_read_batch(requests.len());

        let mut command = self.git_cmd();
        command
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("Failed to spawn git cat-file")?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RailError::message("Failed to open stdin"))?;

        // `cat-file --batch` writes one response while it reads each request. Drain
        // stdout concurrently with request submission so neither pipe can fill and
        // deadlock on repositories whose tree exceeds the platform pipe capacity.
        let output = std::thread::scope(|scope| -> RailResult<_> {
            let writer = scope.spawn(move || -> std::io::Result<()> {
                for request in requests {
                    stdin.write_all(request.as_bytes())?;
                    stdin.write_all(b"\n")?;
                }
                Ok(())
            });
            let output = child.wait_with_output().context("Failed to read git cat-file output")?;
            writer
                .join()
                .map_err(|_| RailError::message("git cat-file request writer panicked"))?
                .context("Failed to write to git cat-file stdin")?;
            Ok(output)
        })?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git cat-file --batch".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }

        let mut results = Vec::with_capacity(requests.len());
        let stdout = &output.stdout[..];
        let mut pos = 0;
        for request in requests {
            let line_end = stdout[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .ok_or_else(|| RailError::message("Invalid cat-file output: missing newline"))?;
            let header = &stdout[pos..pos + line_end];
            pos += line_end + 1;

            if header.ends_with(b" missing") {
                if missing_as_empty {
                    results.push(Vec::new());
                    continue;
                }
                return Err(RailError::message(format!("Git object '{}' is missing", request)));
            }

            let mut parts = header.split(|&b| b == b' ');
            let _object_id = parts.next();
            let object_type = parts.next();
            let size = parts.next();
            if parts.next().is_some() || object_type != Some(expected_type) {
                return Err(RailError::message(format!(
                    "Invalid cat-file header: {}",
                    String::from_utf8_lossy(header)
                )));
            }
            let size = size.ok_or_else(|| RailError::message("Invalid cat-file header: missing object size"))?;
            let size_str = String::from_utf8_lossy(size);
            let size: usize = size_str
                .parse()
                .map_err(|_| RailError::message(format!("Invalid size in cat-file output: {}", size_str)))?;

            if pos + size > stdout.len() {
                return Err(RailError::message("Unexpected end of cat-file output"));
            }
            let content = stdout[pos..pos + size].to_vec();
            pos += size;
            if stdout.get(pos) != Some(&b'\n') {
                return Err(RailError::message(
                    "Invalid cat-file output: missing content terminator",
                ));
            }
            pos += 1;
            results.push(content);
        }
        Ok(results)
    }

    /// Get multiple commits through one bounded `cat-file --batch` stream.
    ///
    /// Used by history and path-filtered walks so commit count does not become
    /// subprocess count. Output order matches input SHA order.
    ///
    /// # Performance
    /// - One subprocess for any number of commits
    /// - Memory bounded by returned metadata plus Git's stream output
    pub fn get_commits_bulk(&self, shas: &[String]) -> RailResult<Vec<CommitInfo>> {
        let objects = self.read_batch_objects(shas, false, b"commit")?;
        shas.iter()
            .zip(objects)
            .map(|(sha, object)| parse_raw_commit(sha, &object))
            .collect()
    }

    /// Resolve a git reference (tag, branch) to a commit SHA
    pub fn resolve_reference(&self, ref_name: &str) -> RailResult<String> {
        self.run_git_stdout(&["rev-parse", ref_name])
    }
}

fn create_empty_shallow_file(directory: &Path) -> RailResult<PathBuf> {
    let path = directory.join("cargo-rail-empty-shallow");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .context("Failed to create complete Git object view")?;
    file.sync_all().context("Failed to persist complete Git object view")?;
    Ok(path)
}

fn deterministic_pack_command(mut command: std::process::Command) -> std::process::Command {
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "-c",
            "pack.threads=1",
            "-c",
            "pack.window=0",
            "-c",
            "pack.depth=1",
            "-c",
            "pack.compression=0",
            "-c",
            "core.compression=0",
            "-c",
            "pack.useSparse=false",
            "pack-objects",
            "--stdout",
            "--revs",
            "--window=0",
            "--depth=1",
            "--compression=0",
            "--no-reuse-object",
            "--no-reuse-delta",
        ]);
    command
}

fn deterministic_index_pack_command(
    mut command: std::process::Command,
    keep_message: Option<&str>,
) -> std::process::Command {
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "-c",
            "pack.threads=1",
            "-c",
            "pack.indexVersion=2",
            "-c",
            "pack.writeReverseIndex=false",
            "index-pack",
            "--stdin",
            "--strict",
            "--threads=1",
        ]);
    if let Some(message) = keep_message {
        command.arg(format!("--keep={message}"));
    }
    command
}

fn validate_object_format(object_format: &str) -> RailResult<()> {
    object_id_hex_len(object_format).map(drop)
}

fn object_id_hex_len(object_format: &str) -> RailResult<usize> {
    match object_format {
        "sha1" => Ok(40),
        "sha256" => Ok(64),
        other => Err(RailError::message(format!("unsupported Git object format '{other}'"))),
    }
}

fn object_format_for_id(object_id: &str) -> RailResult<&'static str> {
    match object_id.len() {
        40 => Ok("sha1"),
        64 => Ok("sha256"),
        _ => Err(RailError::message("Git returned an object ID with unsupported length")),
    }
}

fn validate_exact_object_id(object_id: &str, object_format: &str) -> RailResult<()> {
    let length = object_id_hex_len(object_format)?;
    if object_id.len() != length
        || !object_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(format!(
            "invalid exact {object_format} Git object ID '{object_id}'"
        )));
    }
    Ok(())
}

fn validate_sha256_digest(digest: &str) -> RailResult<()> {
    let Some(hex) = digest.strip_prefix("sha256-") else {
        return Err(RailError::message("prepared Git object bundle digest is not SHA-256"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(
            "prepared Git object bundle digest is not canonical SHA-256",
        ));
    }
    Ok(())
}

fn validate_branch_ref_name(ref_name: &str) -> RailResult<()> {
    if !ref_name.starts_with("refs/heads/") || ref_name.len() <= "refs/heads/".len() {
        return Err(RailError::message(format!(
            "prepared Git ref '{ref_name}' is not an exact local branch ref"
        )));
    }
    if ref_name.len() > 1_024 || ref_name.bytes().any(|byte| byte == 0 || byte == b'\n' || byte == b'\r') {
        return Err(RailError::message(
            "prepared Git branch ref is not a bounded single-line value",
        ));
    }
    Ok(())
}

fn validate_effect_keep_id(effect_id: &str) -> RailResult<()> {
    let valid = !effect_id.is_empty()
        && effect_id.len() <= 256
        && effect_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !valid {
        return Err(RailError::message(
            "prepared Git effect ID is not a bounded filesystem-safe token",
        ));
    }
    Ok(())
}

fn reject_symlink_or_reparse_ancestors(root: &Path, path: &Path) -> RailResult<()> {
    let mut current = root.to_path_buf();
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(RailError::message(format!(
                "prepared Git path '{}' is not repository-relative",
                path.display()
            )));
        };
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(RailError::message(format!(
                    "failed to inspect prepared Git path ancestor '{}': {error}",
                    current.display()
                )));
            }
        };
        if utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::message(format!(
                "prepared Git path ancestor '{}' is not a real directory",
                current.display()
            )));
        }
        if !metadata.is_dir() {
            return Ok(());
        }
    }
    Ok(())
}

fn exact_entry_image_eq(observed: Option<&GitTreeEntry>, expected: Option<&GitTreeEntry>) -> bool {
    match (observed, expected) {
        (None, None) => true,
        (Some(observed), Some(expected)) => observed.mode == expected.mode && observed.object_id == expected.object_id,
        _ => false,
    }
}

fn verify_prepared_commit_output(mut command: std::process::Command, expected: &GitCommitEffect) -> RailResult<()> {
    use std::fmt::Write as _;

    let mut headers = String::new();
    writeln!(&mut headers, "tree {}", expected.tree()).expect("writing to String cannot fail");
    for parent in expected.parents() {
        writeln!(&mut headers, "parent {parent}").expect("writing to String cannot fail");
    }
    let metadata = expected.metadata();
    writeln!(
        &mut headers,
        "author {} <{}> {} {}",
        metadata.author, metadata.author_email, metadata.author_timestamp, metadata.author_timezone
    )
    .expect("writing to String cannot fail");
    writeln!(
        &mut headers,
        "committer {} <{}> {} {}",
        metadata.committer, metadata.committer_email, metadata.committer_timestamp, metadata.committer_timezone
    )
    .expect("writing to String cannot fail");
    headers.push('\n');

    let actual = command.output().context("Failed to read prepared Git commit")?;
    if !actual.status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
            command: "git cat-file commit <prepared-result>".to_string(),
            stderr: git_command_diagnostics(&actual.stdout, &actual.stderr),
        }));
    }

    let mut reconstructed = headers.into_bytes();
    reconstructed.extend_from_slice(expected.message().as_bytes());
    if actual.stdout != reconstructed {
        if !expected.message().ends_with('\n') {
            reconstructed.push(b'\n');
        }
        if actual.stdout != reconstructed {
            return Err(RailError::message(format!(
                "prepared Git commit '{}' does not match its journaled tree, ordered parents, raw message, and metadata",
                expected.oid()
            )));
        }
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn reject_platform_tree_aliases(_entries: &[GitTreeEntry]) -> RailResult<()> {
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn reject_platform_tree_aliases(entries: &[GitTreeEntry]) -> RailResult<()> {
    let mut prefixes = std::collections::BTreeMap::<String, String>::new();
    for entry in entries {
        let path = RepositoryPath::new(&entry.path)?;
        let mut original_prefix = String::new();
        let mut alias_prefix = String::new();
        for component in path.as_str().split('/') {
            if !original_prefix.is_empty() {
                original_prefix.push('/');
                alias_prefix.push('/');
            }
            original_prefix.push_str(component);
            alias_prefix.push_str(&platform_alias_component(component));
            if let Some(existing) = prefixes.get(&alias_prefix) {
                if existing != &original_prefix {
                    return Err(RailError::with_help(
                        format!(
                            "prepared Git tree paths '{}' and '{}' alias on this platform",
                            existing, original_prefix
                        ),
                        "rename one path so every component has a distinct case- and Unicode-normalized filesystem name",
                    ));
                }
            } else {
                prefixes.insert(alias_prefix.clone(), original_prefix.clone());
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn platform_alias_component(component: &str) -> String {
    component.chars().flat_map(char::to_uppercase).collect()
}

#[cfg(target_os = "macos")]
fn platform_alias_component(component: &str) -> String {
    let normalizer = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    normalizer
        .normalize(component)
        .chars()
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(unix)]
fn read_regular_file_nofollow(root: &Path, path: &Path) -> RailResult<(Vec<u8>, std::fs::Metadata)> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let mut components = path.components().peekable();
    let mut directory = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)?;
    let mut final_name = None;
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            return Err(RailError::message("prepared Git worktree path is not canonical"));
        };
        if components.peek().is_none() {
            final_name = Some(component.to_os_string());
            break;
        }
        directory = rustix::fs::openat(
            &directory,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(std::io::Error::from)?;
    }
    let final_name = final_name.ok_or_else(|| RailError::message("prepared Git path did not identify a file"))?;
    let mut file = rustix::fs::openat(
        &directory,
        &final_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mode() != after.mode()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' changed while it was read",
            path.display()
        )));
    }
    Ok((bytes, before))
}

#[cfg(windows)]
fn read_regular_file_nofollow(root: &Path, path: &Path) -> RailResult<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read as _;

    let root_guard = crate::windows_fs::open_for_execution_guard(root)?;
    crate::windows_fs::observe_file(&root_guard)?;
    let opened_root = crate::windows_fs::opened_path(&root_guard)?;
    let mut ancestor_guards = vec![root_guard];
    let mut current = root.to_path_buf();
    if let Some(parent) = path.parent() {
        for component in parent.components() {
            let std::path::Component::Normal(component) = component else {
                return Err(RailError::message("prepared Git worktree path is not canonical"));
            };
            current.push(component);
            let guard = crate::windows_fs::open_for_execution_guard(&current)?;
            crate::windows_fs::observe_file(&guard)?;
            if !guard.metadata()?.is_dir() {
                return Err(RailError::message(format!(
                    "prepared Git path ancestor '{}' is not a real directory",
                    current.display()
                )));
            }
            ancestor_guards.push(guard);
        }
    }
    let absolute = root.join(path);
    let mut file = crate::windows_fs::open_for_execution_guard(&absolute)?;
    let before = crate::windows_fs::observe_file(&file)?;
    let opened = crate::windows_fs::opened_path(&file)?;
    let relative = utils::path_relative_to(&opened_root, &opened).map_err(|_| {
        RailError::message(format!(
            "prepared Git worktree path '{}' resolves outside its repository",
            path.display()
        ))
    })?;
    if relative != path {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' resolved with different path spelling '{}'",
            path.display(),
            relative.display()
        )));
    }
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let after = crate::windows_fs::observe_file(&file)?;
    if before != after {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' changed while it was read",
            path.display()
        )));
    }
    drop(ancestor_guards);
    Ok((bytes, metadata))
}

#[cfg(not(any(unix, windows)))]
fn read_regular_file_nofollow(root: &Path, path: &Path) -> RailResult<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read as _;

    let absolute = root.join(path);
    let metadata = std::fs::symlink_metadata(&absolute)?;
    if !metadata.is_file() || utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' is not a regular file",
            path.display()
        )));
    }
    let mut file = std::fs::File::open(&absolute)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if metadata.len() != after.len() || metadata.modified()? != after.modified()? {
        return Err(RailError::message(format!(
            "prepared Git worktree path '{}' changed while it was read",
            path.display()
        )));
    }
    Ok((bytes, metadata))
}

fn require_complete_object_closure(
    command: std::process::Command,
    object_id: &str,
    object_format: &str,
    context: &str,
) -> RailResult<()> {
    require_complete_object_closures(command, [object_id], object_format, context)
}

fn require_complete_object_closures<'a>(
    mut command: std::process::Command,
    object_ids: impl IntoIterator<Item = &'a str>,
    object_format: &str,
    context: &str,
) -> RailResult<()> {
    use std::io::{BufRead as _, Read as _, Seek as _};
    use std::process::Stdio;

    let object_ids = object_ids
        .into_iter()
        .map(|object_id| {
            validate_exact_object_id(object_id, object_format)?;
            Ok(object_id)
        })
        .collect::<RailResult<std::collections::BTreeSet<_>>>()?;
    if object_ids.is_empty() {
        return Ok(());
    }
    let mut stderr = tempfile::tempfile().context("Failed to allocate Git connectivity diagnostics")?;
    command
        .args(["rev-list", "--objects", "--missing=print", "--no-object-names"])
        .args(&object_ids)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr.try_clone()?));
    let mut child = command
        .spawn()
        .context("Failed to start exact Git connectivity check")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RailError::message("exact Git connectivity stdout was unavailable"))?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut record = Vec::with_capacity(object_id_hex_len(object_format)?.saturating_add(2));
    let mut missing = Vec::new();
    loop {
        record.clear();
        let read = reader.read_until(b'\n', &mut record)?;
        if read == 0 {
            break;
        }
        while record.last().is_some_and(|byte| matches!(byte, b'\n' | b'\r')) {
            record.pop();
        }
        if let Some(raw_missing) = record.strip_prefix(b"?") {
            let missing_id = std::str::from_utf8(raw_missing)
                .map_err(|_| RailError::message("Git reported a non-UTF-8 missing object ID"))?;
            validate_exact_object_id(missing_id, object_format)?;
            if missing.len() < 8 {
                missing.push(missing_id.to_string());
            }
        }
    }
    let status = child.wait().context("Failed to finish exact Git connectivity check")?;
    stderr.rewind()?;
    let mut diagnostics = Vec::new();
    stderr.take(64 * 1024).read_to_end(&mut diagnostics)?;
    if !status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
            command: "git rev-list --objects --missing=print <exact-object>".to_string(),
            stderr: git_command_diagnostics(&[], &diagnostics),
        }));
    }
    if !missing.is_empty() {
        return Err(RailError::message(format!(
            "{context} set has missing local object(s): {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn digest_opened_pack(file: &mut std::fs::File) -> RailResult<String> {
    use rscrypto::Sha256;
    use std::io::{Read as _, Seek as _};

    file.rewind()?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.rewind()?;
    let hex = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256-{hex}"))
}

fn parse_index_pack_result<'a>(output: &'a [u8], object_format: &str) -> RailResult<(&'a str, &'a str)> {
    let output = std::str::from_utf8(output)?.trim_end_matches(['\n', '\r']);
    let (disposition, pack_hash) = output
        .split_once('\t')
        .ok_or_else(|| RailError::message("git index-pack returned an invalid retained-pack record"))?;
    if !matches!(disposition, "keep" | "pack") {
        return Err(RailError::message(format!(
            "git index-pack returned unsupported disposition '{disposition}'"
        )));
    }
    validate_exact_object_id(pack_hash, object_format)?;
    Ok((disposition, pack_hash))
}

fn pack_keep_bytes(message: &str) -> Vec<u8> {
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes
}

fn inspect_pack_keep(path: &Path, message: &str) -> RailResult<bool> {
    use std::io::Read as _;

    let mut file = open_pack_keep_nofollow(path)?;
    let expected = pack_keep_bytes(message);
    let mut actual = Vec::new();
    (&mut file)
        .take(u64::try_from(expected.len()).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut actual)?;
    Ok(actual == expected)
}

fn remove_owned_pack_keep(path: &Path, message: &str) -> RailResult<()> {
    use std::io::Read as _;

    let expected = pack_keep_bytes(message);
    #[cfg(windows)]
    {
        let mut file = crate::windows_fs::open_for_stable_byte_observation_and_delete(path)?;
        crate::windows_fs::observe_file(&file)?;
        let mut actual = Vec::new();
        (&mut file)
            .take(u64::try_from(expected.len()).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut actual)?;
        if actual != expected {
            return Err(RailError::message(format!(
                "prepared Git pack keep '{}' changed before cleanup",
                path.display()
            )));
        }
        crate::windows_fs::delete_file_by_handle(file)?;
        Ok(())
    }
    #[cfg(unix)]
    {
        let mut file = open_pack_keep_nofollow(path)?;
        let mut actual = Vec::new();
        (&mut file)
            .take(u64::try_from(expected.len()).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut actual)?;
        if actual != expected {
            return Err(RailError::message(format!(
                "prepared Git pack keep '{}' changed before cleanup",
                path.display()
            )));
        }
        rustix::fs::unlinkat(rustix::fs::CWD, path, rustix::fs::AtFlags::empty()).map_err(std::io::Error::from)?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut file = open_pack_keep_nofollow(path)?;
        let mut actual = Vec::new();
        (&mut file)
            .take(u64::try_from(expected.len()).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut actual)?;
        if actual != expected {
            return Err(RailError::message(format!(
                "prepared Git pack keep '{}' changed before cleanup",
                path.display()
            )));
        }
        std::fs::remove_file(path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn open_pack_keep_nofollow(path: &Path) -> RailResult<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;

    let file = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(RailError::message(format!(
            "prepared Git pack keep '{}' is not a private regular file",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_pack_keep_nofollow(path: &Path) -> RailResult<std::fs::File> {
    let file = crate::windows_fs::open_for_stable_byte_observation(path)?;
    crate::windows_fs::observe_file(&file)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_pack_keep_nofollow(path: &Path) -> RailResult<std::fs::File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "prepared Git pack keep '{}' is not a regular file",
            path.display()
        )));
    }
    Ok(std::fs::File::open(path)?)
}

fn materialize_historical_symlink(destination: &Path, path: &Path, bytes: &[u8]) -> RailResult<()> {
    let target = std::str::from_utf8(bytes).map_err(|_| {
        RailError::message(format!(
            "historical symbolic-link target for '{}' is not valid UTF-8",
            path.display()
        ))
    })?;
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return Err(RailError::with_help(
            format!(
                "historical symbolic link '{}' points outside the isolated tree",
                path.display()
            ),
            "select a revision whose Cargo workspace does not depend on external symbolic links",
        ));
    }
    let parent = path
        .parent()
        .and_then(|parent| parent.strip_prefix(destination).ok())
        .ok_or_else(|| RailError::message("historical symbolic link has no contained parent"))?;
    let mut depth = parent.components().count();
    for component in target_path.components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if depth > 0 => depth -= 1,
            std::path::Component::ParentDir | std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(RailError::with_help(
                    format!(
                        "historical symbolic link '{}' escapes the isolated tree",
                        path.display()
                    ),
                    "select a revision whose Cargo workspace does not depend on external symbolic links",
                ));
            }
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path).map_err(|error| {
            RailError::message(format!(
                "failed to materialize historical symbolic link '{}': {error}",
                path.display()
            ))
        })?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, path).map_err(|error| {
            RailError::with_help(
                format!(
                    "failed to materialize historical symbolic link '{}': {error}",
                    path.display()
                ),
                "enable symbolic-link creation or select a historical Cargo workspace without symbolic links",
            )
        })?;
    }
    Ok(())
}

/// Parse `git diff --name-status -z` output into (path, change_type) pairs.
///
/// Handles:
/// - Simple changes: `M\0path\0` (Modified), `A\0path\0` (Added), `D\0path\0` (Deleted)
/// - Renames: `R100\0old_path\0new_path\0` - emits both (old_path, 'D') and (new_path, 'A')
/// - Copies: `C100\0src_path\0dest_path\0` - emits both (src_path, 'M') and (dest_path, 'A')
/// - Paths with spaces/newlines/tabs: handled safely via NUL separation
///
/// The status codes from git are:
/// - A: Added
/// - C: Copied (followed by similarity percentage)
/// - D: Deleted
/// - M: Modified
/// - R: Renamed (followed by similarity percentage)
/// - T: Type changed
/// - U: Unmerged
/// - X: Unknown
///
/// One commit from [`SystemGit::log_with_files`]
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Full commit SHA
    pub sha: String,
    /// Commit subject line
    pub subject: String,
    /// Commit body (trimmed), if any
    pub body: Option<String>,
    /// Paths changed by this commit, relative to the repository root
    pub files: Vec<PathBuf>,
}

/// Parse `git log -z --name-only --format=%x01%H%x02%s%x02%b%x02` output
///
/// Record layout: `\x01` + sha + `\x02` + subject + `\x02` + body + `\x02` +
/// NUL (+ `\n` when files follow) + NUL-terminated file paths. Bodies may
/// contain newlines; the trailing `\x02` before the NUL delimits them.
fn parse_log_with_files(output: &[u8]) -> RailResult<Vec<LogEntry>> {
    let text = String::from_utf8_lossy(output);
    let mut entries = Vec::new();

    for record in text.split('\x01') {
        if record.is_empty() {
            continue;
        }

        let Some((sha, rest)) = record.split_once('\x02') else {
            continue;
        };
        let Some((subject, rest)) = rest.split_once('\x02') else {
            continue;
        };
        // The body may contain any text except \x02; the last \x02 in the
        // record closes it and the file list follows.
        let Some((body_raw, files_raw)) = rest.rsplit_once('\x02') else {
            continue;
        };

        let body = body_raw.trim();
        let files = files_raw
            .trim_start_matches(['\0', '\n'])
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(PathBuf::from)
            .collect();

        entries.push(LogEntry {
            sha: sha.trim().to_string(),
            subject: subject.trim().to_string(),
            body: (!body.is_empty()).then(|| body.to_string()),
            files,
        });
    }

    Ok(entries)
}

fn parse_name_status_output_z(output: &[u8]) -> RailResult<Vec<(PathBuf, char)>> {
    let mut files = Vec::new();

    let mut parts = output.split(|&b| b == 0);
    while let Some(status_bytes) = parts.next() {
        if status_bytes.is_empty() {
            continue;
        }

        let status = String::from_utf8_lossy(status_bytes);
        let change_type = status.chars().next().unwrap_or('M');

        let mut next_path = || parts.next().filter(|p| !p.is_empty());

        match change_type {
            'R' => {
                // Rename: R100\0old_path\0new_path\0
                // Treat as: delete from old location, add at new location
                let Some(old_path) = next_path() else {
                    continue;
                };
                let Some(new_path) = next_path() else {
                    // Fallback: if only one path, treat as modified
                    files.push((PathBuf::from(String::from_utf8_lossy(old_path).into_owned()), 'M'));
                    continue;
                };

                files.push((PathBuf::from(String::from_utf8_lossy(old_path).into_owned()), 'D'));
                files.push((PathBuf::from(String::from_utf8_lossy(new_path).into_owned()), 'A'));
            }
            'C' => {
                // Copy: C100\0src_path\0dest_path\0
                // Source still exists (mark as touched), dest is new
                let Some(src_path) = next_path() else {
                    continue;
                };
                let Some(dest_path) = next_path() else {
                    files.push((PathBuf::from(String::from_utf8_lossy(src_path).into_owned()), 'A'));
                    continue;
                };

                files.push((PathBuf::from(String::from_utf8_lossy(src_path).into_owned()), 'M'));
                files.push((PathBuf::from(String::from_utf8_lossy(dest_path).into_owned()), 'A'));
            }
            'A' | 'D' | 'M' | 'T' | 'U' => {
                let Some(path) = next_path() else {
                    continue;
                };
                files.push((PathBuf::from(String::from_utf8_lossy(path).into_owned()), change_type));
            }
            _ => {
                // Unknown status - treat as modified if we have a path
                let Some(path) = next_path() else {
                    continue;
                };
                files.push((PathBuf::from(String::from_utf8_lossy(path).into_owned()), 'M'));
            }
        }
    }

    Ok(files)
}

fn parse_name_status_records(records: &[&[u8]]) -> RailResult<Vec<(PathBuf, char)>> {
    if !records.len().is_multiple_of(2) {
        return Err(RailError::message(
            "git diff-tree returned an incomplete status/path pair",
        ));
    }
    records
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let status_record = pair[0].strip_prefix(b"\n").unwrap_or(pair[0]);
            let status = status_record
                .first()
                .copied()
                .map(char::from)
                .ok_or_else(|| RailError::message("git diff-tree returned an empty change status"))?;
            let change_type = match status {
                'A' | 'D' | 'M' | 'T' | 'U' => status,
                _ => 'M',
            };
            Ok((
                PathBuf::from(String::from_utf8_lossy(pair[1]).into_owned()),
                change_type,
            ))
        })
        .collect()
}

/// Parse git log output into CommitInfo
///
/// Format is %H%n%an%n%ae%n%at%n%ai%n%cn%n%ce%n%ct%n%ci%n%P%n%B
/// Which gives us: hash, author name, author email, author time,
///                 committer name, committer email, committer time,
///                 parent hashes, body
fn parse_commit_output(data: &[u8]) -> RailResult<CommitInfo> {
    let output = String::from_utf8_lossy(data);
    let mut lines = output.lines();

    let sha = lines
        .next()
        .ok_or_else(|| RailError::message("Missing commit SHA"))?
        .to_string();
    let author = lines
        .next()
        .ok_or_else(|| RailError::message("Missing author name"))?
        .to_string();
    let author_email = lines
        .next()
        .ok_or_else(|| RailError::message("Missing author email"))?
        .to_string();
    let timestamp = lines
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| RailError::message("Missing/invalid author timestamp"))?;
    let author_timezone =
        parse_formatted_timezone(lines.next().ok_or_else(|| RailError::message("Missing author date"))?)?;
    let committer = lines
        .next()
        .ok_or_else(|| RailError::message("Missing committer name"))?
        .to_string();
    let committer_email = lines
        .next()
        .ok_or_else(|| RailError::message("Missing committer email"))?
        .to_string();
    let committer_timestamp = lines
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| RailError::message("Missing/invalid committer timestamp"))?;
    let committer_timezone = parse_formatted_timezone(
        lines
            .next()
            .ok_or_else(|| RailError::message("Missing committer date"))?,
    )?;
    let parents_line = lines.next().unwrap_or("");
    let parent_shas = if parents_line.is_empty() {
        vec![]
    } else {
        parents_line.split_whitespace().map(|s| s.to_string()).collect()
    };

    // Rest is commit message
    let message: Vec<String> = lines.map(|s| s.to_string()).collect();
    let message = message.join("\n").trim().to_string();

    Ok(CommitInfo {
        sha,
        author,
        author_email,
        committer,
        committer_email,
        message,
        timestamp,
        author_timezone,
        committer_timestamp,
        committer_timezone,
        parent_shas,
    })
}

fn parse_raw_commit(sha: &str, data: &[u8]) -> RailResult<CommitInfo> {
    let separator = data
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or_else(|| RailError::message(format!("Git commit '{}' has no message separator", sha)))?;
    let headers = std::str::from_utf8(&data[..separator])
        .map_err(|_| RailError::message(format!("Git commit '{}' has non-UTF-8 headers", sha)))?;
    let mut author = None;
    let mut committer = None;
    let mut parent_shas = Vec::new();
    for line in headers.lines() {
        if let Some(parent) = line.strip_prefix("parent ") {
            validate_batch_object_id(parent)?;
            parent_shas.push(parent.to_string());
        } else if let Some(signature) = line.strip_prefix("author ") {
            author = Some(parse_raw_signature(signature, "author")?);
        } else if let Some(signature) = line.strip_prefix("committer ") {
            committer = Some(parse_raw_signature(signature, "committer")?);
        }
    }
    let (author, author_email, timestamp, author_timezone) =
        author.ok_or_else(|| RailError::message(format!("Git commit '{}' has no author", sha)))?;
    let (committer, committer_email, committer_timestamp, committer_timezone) =
        committer.ok_or_else(|| RailError::message(format!("Git commit '{}' has no committer", sha)))?;
    let raw_message = String::from_utf8_lossy(&data[separator + 2..]);
    let message = raw_message.strip_suffix('\n').unwrap_or(&raw_message).to_string();

    Ok(CommitInfo {
        sha: sha.to_string(),
        author,
        author_email,
        committer,
        committer_email,
        message,
        timestamp,
        author_timezone,
        committer_timestamp,
        committer_timezone,
        parent_shas,
    })
}

fn parse_raw_signature(signature: &str, field: &str) -> RailResult<(String, String, i64, String)> {
    let mut suffix = signature.rsplitn(3, ' ');
    let timezone = suffix
        .next()
        .ok_or_else(|| RailError::message(format!("Git {} has no time-zone offset", field)))?;
    validate_timezone(timezone)?;
    let timestamp = suffix
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| RailError::message(format!("Git {} has an invalid timestamp", field)))?;
    let identity = suffix
        .next()
        .ok_or_else(|| RailError::message(format!("Git {} has no identity", field)))?;
    let email_end = identity
        .strip_suffix('>')
        .ok_or_else(|| RailError::message(format!("Git {} email is malformed", field)))?;
    let (name, email) = email_end
        .rsplit_once(" <")
        .ok_or_else(|| RailError::message(format!("Git {} identity is malformed", field)))?;
    Ok((name.to_string(), email.to_string(), timestamp, timezone.to_string()))
}

fn validate_batch_object_id(value: &str) -> RailResult<()> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(RailError::message(format!(
            "Git commit has invalid parent object ID '{}'",
            value
        )))
    }
}

fn parse_formatted_timezone(date: &str) -> RailResult<String> {
    let timezone = date
        .split_whitespace()
        .next_back()
        .ok_or_else(|| RailError::message("Git date has no time-zone offset"))?;
    validate_timezone(timezone)?;
    Ok(timezone.to_string())
}

fn validate_timezone(timezone: &str) -> RailResult<()> {
    if timezone.len() == 5
        && matches!(timezone.as_bytes().first(), Some(b'+' | b'-'))
        && timezone.as_bytes()[1..].iter().all(u8::is_ascii_digit)
    {
        return Ok(());
    }
    Err(RailError::message(format!(
        "Git date has invalid time-zone offset '{}'",
        timezone
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn init_test_repo() -> (tempfile::TempDir, SystemGit) {
        let directory = tempfile::tempdir().unwrap();
        crate::git::init_repo(directory.path(), "main").unwrap();
        let git = SystemGit::open(directory.path()).unwrap();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        git.set_config("commit.gpgsign", "false").unwrap();
        git.set_config("core.autocrlf", "false").unwrap();
        (directory, git)
    }

    fn commit_test_file(git: &SystemGit, root: &Path, path: &str, contents: &[u8]) -> String {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
        git.stage_all().unwrap();
        git.commit("test commit").unwrap();
        git.head_commit().unwrap()
    }

    fn write_typed_object(git: &SystemGit, kind: &str, contents: &[u8]) -> String {
        use std::io::Write as _;
        use std::process::Stdio;

        let mut command = git.git_cmd();
        command
            .args(["hash-object", "-t", kind, "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(contents).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit_effect(git: &SystemGit, oid: &str) -> GitCommitEffect {
        let commit = git.get_commit(oid).unwrap();
        let metadata = commit.metadata();
        GitCommitEffect::new(
            oid.to_string(),
            git.run_git_stdout(&["rev-parse", &format!("{oid}^{{tree}}")]).unwrap(),
            commit.parent_shas,
            commit.message,
            crate::mutation::git_effect::GitEffectCommitMetadata::from(&metadata),
        )
    }

    #[cfg(target_os = "macos")]
    fn tree_entry(path: &str, digit: char) -> GitTreeEntry {
        GitTreeEntry {
            mode: "100644".to_string(),
            object_id: std::iter::repeat_n(digit, 40).collect(),
            path: PathBuf::from(path),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_tree_rejects_macos_case_and_unicode_aliases() {
        let case_aliases = [tree_entry("Dir/a", '1'), tree_entry("dir/b", '2')];
        let error = reject_platform_tree_aliases(&case_aliases).unwrap_err();
        assert!(error.to_string().contains("alias on this platform"), "{error}");

        let unicode_aliases = [tree_entry("caf\u{00e9}/a", '1'), tree_entry("cafe\u{0301}/b", '2')];
        let error = reject_platform_tree_aliases(&unicode_aliases).unwrap_err();
        assert!(error.to_string().contains("alias on this platform"), "{error}");

        let distinct = [tree_entry("cafe/a", '1'), tree_entry("cafeteria/b", '2')];
        reject_platform_tree_aliases(&distinct).unwrap();
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ObjectStoreEntry {
        path: PathBuf,
        is_directory: bool,
        length: u64,
        modified: std::time::SystemTime,
    }

    fn object_store_snapshot(git: &SystemGit) -> Vec<ObjectStoreEntry> {
        fn visit(root: &Path, path: &Path, entries: &mut Vec<ObjectStoreEntry>) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            entries.push(ObjectStoreEntry {
                path: path.strip_prefix(root).unwrap().to_path_buf(),
                is_directory: metadata.is_dir(),
                length: metadata.len(),
                modified: metadata.modified().unwrap(),
            });
            if metadata.is_dir() {
                let mut children = std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect::<Vec<_>>();
                children.sort();
                for child in children {
                    visit(root, &child, entries);
                }
            }
        }

        let root = git.object_directory().unwrap();
        let mut entries = Vec::new();
        visit(&root, &root, &mut entries);
        entries
    }

    fn assert_no_pack_keeps(git: &SystemGit) {
        let pack = git.object_directory().unwrap().join("pack");
        if !pack.exists() {
            return;
        }
        let keeps = std::fs::read_dir(pack)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "keep"))
            .collect::<Vec<_>>();
        assert!(keeps.is_empty(), "prepared pack keep leaked: {keeps:?}");
    }

    /// Helper to get the git repository root
    fn find_git_root() -> PathBuf {
        env::current_dir().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn historical_materialization_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let link = nested.join("link");

        let error = materialize_historical_symlink(root.path(), &link, b"../../outside")
            .expect_err("historical symlink escaped the isolated tree");
        assert!(error.to_string().contains("escapes the isolated tree"), "{error}");
        assert!(!link.exists());
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_duplicate_write_does_not_freshen_real_object() {
        use rustix::fs::{Timespec, Timestamps};
        use std::os::unix::fs::MetadataExt as _;

        let (directory, git) = init_test_repo();
        let head = commit_test_file(&git, directory.path(), "file.txt", b"same contents\n");
        let blob = git.tree_entry(&head, Path::new("file.txt")).unwrap().unwrap().object_id;
        let objects = git.object_directory().unwrap();
        let (directory_name, file_name) = blob.split_at(2);
        let loose = objects.join(directory_name).join(file_name);
        let loose_file = std::fs::File::open(&loose).unwrap();
        let old = Timespec {
            tv_sec: 1_600_000_000,
            tv_nsec: 123_456_789,
        };
        rustix::fs::futimens(
            &loose_file,
            &Timestamps {
                last_access: old,
                last_modification: old,
            },
        )
        .unwrap();
        let before = loose_file.metadata().unwrap();

        let quarantine = git.object_quarantine().unwrap();
        quarantine.import_object_closure(&git, &[&head]).unwrap();
        assert_eq!(quarantine.write_blob(b"same contents\n").unwrap(), blob);
        quarantine.require_complete_closure(&head).unwrap();

        let after = loose_file.metadata().unwrap();
        assert_eq!(
            (before.mtime(), before.mtime_nsec()),
            (after.mtime(), after.mtime_nsec())
        );
    }

    #[test]
    fn quarantine_blob_batch_matches_individual_git_object_identity() {
        let (_directory, git) = init_test_repo();
        let quarantine = git.object_quarantine().unwrap();
        let contents = vec![b"alpha\n".to_vec(), vec![0, 1, 2, 0xff], b"alpha\n".to_vec()];

        let batch = quarantine.write_blobs(&contents).unwrap();
        let individual = contents
            .iter()
            .map(|content| quarantine.write_blob(content))
            .collect::<RailResult<Vec<_>>>()
            .unwrap();

        assert_eq!(batch, individual);
        assert_eq!(
            batch[0], batch[2],
            "duplicate bytes must retain one Git object identity"
        );
        assert!(quarantine.write_blobs(&[]).unwrap().is_empty());
    }

    #[test]
    fn quarantine_import_rejects_missing_closure_without_mutating_target_objects() {
        let (source_directory, source) = init_test_repo();
        let head = commit_test_file(&source, source_directory.path(), "file.txt", b"complete\n");
        let commit = source.run_git(&["cat-file", "commit", &head]).unwrap().stdout;
        let (target_directory, target) = init_test_repo();
        let copied = write_typed_object(&target, "commit", &commit);
        assert_eq!(copied, head);
        target.run_git(&["update-ref", "refs/heads/main", &head]).unwrap();
        let before = object_store_snapshot(&target);

        let quarantine = target.object_quarantine().unwrap();
        let error = quarantine
            .import_object_closure(&target, &[&head])
            .expect_err("a commit without its tree must not be accepted as a complete input");

        assert!(
            error.to_string().contains("missing")
                || error.to_string().contains("unable to read")
                || error.to_string().contains("bad tree object"),
            "{error}"
        );
        assert_eq!(object_store_snapshot(&target), before);
        assert!(
            !quarantine
                .git_cmd()
                .args(["cat-file", "-e", &head])
                .status()
                .unwrap()
                .success()
        );
        drop(target_directory);
    }

    #[test]
    fn quarantine_import_ignores_repository_shallow_boundary() {
        let (directory, git) = init_test_repo();
        let parent = commit_test_file(&git, directory.path(), "file.txt", b"parent\n");
        let child = commit_test_file(&git, directory.path(), "file.txt", b"child\n");
        let shallow_path = git.run_git_stdout(&["rev-parse", "--git-path", "shallow"]).unwrap();
        let shallow_path = PathBuf::from(shallow_path);
        let shallow_path = if shallow_path.is_absolute() {
            shallow_path
        } else {
            directory.path().join(shallow_path)
        };
        std::fs::write(shallow_path, format!("{child}\n")).unwrap();

        let quarantine = git.object_quarantine().unwrap();
        quarantine.import_object_closure(&git, &[&child]).unwrap();

        assert!(
            quarantine
                .git_cmd()
                .args(["cat-file", "-e", &format!("{parent}^{{commit}}")])
                .status()
                .unwrap()
                .success(),
            "the private closure omitted the parent hidden by the repository's shallow boundary"
        );
        quarantine.require_complete_closure(&child).unwrap();
    }

    #[test]
    fn exact_path_images_treat_pathspec_metacharacters_literally() {
        let (directory, git) = init_test_repo();
        std::fs::write(directory.path().join("literal[ab].txt"), b"brackets\n").unwrap();
        std::fs::write(directory.path().join("literala.txt"), b"expanded\n").unwrap();
        git.stage_all().unwrap();
        git.commit("literal paths").unwrap();
        let head = git.head_commit().unwrap();

        let tree = git.tree_entry(&head, Path::new("literal[ab].txt")).unwrap().unwrap();
        let index = git.index_entry(Path::new("literal[ab].txt")).unwrap().unwrap();

        assert_eq!(tree.path, Path::new("literal[ab].txt"));
        assert_eq!(index.path, Path::new("literal[ab].txt"));
        assert_eq!(tree.object_id, index.object_id);
    }

    #[test]
    fn exact_index_image_rejects_hidden_index_state() {
        let (directory, git) = init_test_repo();
        commit_test_file(&git, directory.path(), "file.txt", b"tracked\n");

        git.run_git(&["update-index", "--assume-unchanged", "--", "file.txt"])
            .unwrap();
        let assume_unchanged = git
            .index_entry(Path::new("file.txt"))
            .expect_err("assume-unchanged must not become prepared index authority");
        assert!(assume_unchanged.to_string().contains("unsupported state flag"));

        git.run_git(&["update-index", "--no-assume-unchanged", "--", "file.txt"])
            .unwrap();
        git.run_git(&["update-index", "--skip-worktree", "--", "file.txt"])
            .unwrap();
        let skip_worktree = git
            .index_entry(Path::new("file.txt"))
            .expect_err("skip-worktree must not become prepared index authority");
        assert!(skip_worktree.to_string().contains("unsupported state flag"));

        git.run_git(&["update-index", "--no-skip-worktree", "--", "file.txt"])
            .unwrap();
        std::fs::write(directory.path().join("intent.txt"), b"intent\n").unwrap();
        git.run_git(&["add", "--intent-to-add", "--", "intent.txt"]).unwrap();
        let intent_to_add = git
            .index_entry(Path::new("intent.txt"))
            .expect_err("intent-to-add must not become prepared index authority");
        assert!(intent_to_add.to_string().contains("intent-to-add"));
    }

    #[test]
    fn prepared_paths_group_file_to_directory_and_retry() {
        let (directory, git) = init_test_repo();
        let root = directory.path();
        let old = commit_test_file(&git, root, "node", b"old file\n");
        std::fs::remove_file(root.join("node")).unwrap();
        std::fs::create_dir(root.join("node")).unwrap();
        std::fs::write(root.join("node/child"), b"new child\n").unwrap();
        git.stage_all().unwrap();
        git.commit("file to directory").unwrap();
        let result = git.head_commit().unwrap();
        git.run_git(&["reset", "--hard", &old]).unwrap();
        git.run_git(&["update-ref", "refs/heads/main", &result, &old]).unwrap();
        let paths = vec![PathBuf::from("node"), PathBuf::from("node/child")];

        git.reconcile_prepared_commit_paths(Some(&old), &result, &paths)
            .unwrap();
        assert_eq!(std::fs::read(root.join("node/child")).unwrap(), b"new child\n");
        git.reconcile_prepared_commit_paths(Some(&old), &result, &paths)
            .unwrap();
    }

    #[test]
    fn prepared_paths_group_directory_to_file_and_retry() {
        let (directory, git) = init_test_repo();
        let root = directory.path();
        std::fs::create_dir(root.join("node")).unwrap();
        std::fs::write(root.join("node/child"), b"old child\n").unwrap();
        git.stage_all().unwrap();
        git.commit("directory state").unwrap();
        let old = git.head_commit().unwrap();
        std::fs::remove_file(root.join("node/child")).unwrap();
        std::fs::remove_dir(root.join("node")).unwrap();
        std::fs::write(root.join("node"), b"new file\n").unwrap();
        git.stage_all().unwrap();
        git.commit("directory to file").unwrap();
        let result = git.head_commit().unwrap();
        git.run_git(&["reset", "--hard", &old]).unwrap();
        git.run_git(&["update-ref", "refs/heads/main", &result, &old]).unwrap();
        let paths = vec![PathBuf::from("node"), PathBuf::from("node/child")];

        git.reconcile_prepared_commit_paths(Some(&old), &result, &paths)
            .unwrap();
        assert_eq!(std::fs::read(root.join("node")).unwrap(), b"new file\n");
        git.reconcile_prepared_commit_paths(Some(&old), &result, &paths)
            .unwrap();
    }

    #[test]
    fn prepared_shape_transition_preserves_overlapping_untracked_sentinel() {
        let (directory, git) = init_test_repo();
        let root = directory.path();
        let old = commit_test_file(&git, root, "node", b"old file\n");
        std::fs::remove_file(root.join("node")).unwrap();
        std::fs::create_dir(root.join("node")).unwrap();
        std::fs::write(root.join("node/child"), b"new child\n").unwrap();
        git.stage_all().unwrap();
        git.commit("file to directory").unwrap();
        let result = git.head_commit().unwrap();
        git.run_git(&["reset", "--hard", &old]).unwrap();
        git.run_git(&["update-ref", "refs/heads/main", &result, &old]).unwrap();
        std::fs::remove_file(root.join("node")).unwrap();
        std::fs::create_dir(root.join("node")).unwrap();
        std::fs::write(root.join("node/outside"), b"preserve me\n").unwrap();
        let paths = vec![PathBuf::from("node"), PathBuf::from("node/child")];

        let error = git
            .reconcile_prepared_commit_paths(Some(&old), &result, &paths)
            .expect_err("an overlapping untracked descendant must stop grouped materialization");

        assert!(error.to_string().contains("overlapping"), "{error}");
        assert_eq!(std::fs::read(root.join("node/outside")).unwrap(), b"preserve me\n");
        assert!(!root.join("node/child").exists());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_mode_uses_owner_execute_and_respects_core_filemode() {
        use std::os::unix::fs::PermissionsExt as _;

        let (directory, git) = init_test_repo();
        let path = directory.path().join("mode.txt");
        std::fs::write(&path, b"mode\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        git.stage_all().unwrap();
        git.commit("executable").unwrap();

        git.set_config("core.filemode", "true").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o645)).unwrap();
        assert_eq!(
            git.worktree_entry(Path::new("mode.txt")).unwrap().unwrap().mode,
            "100644"
        );

        git.set_config("core.filemode", "false").unwrap();
        assert_eq!(
            git.worktree_entry(Path::new("mode.txt")).unwrap().unwrap().mode,
            "100755"
        );
    }

    #[test]
    fn prepared_pack_install_updates_then_adopts_and_removes_its_keep() {
        let (source_directory, source) = init_test_repo();
        let result = commit_test_file(&source, source_directory.path(), "file.txt", b"result\n");
        let (_target_directory, target) = init_test_repo();
        let quarantine = target.object_quarantine().unwrap();
        quarantine.import_object_closure(&source, &[&result]).unwrap();
        let mut bundle = tempfile::NamedTempFile::new().unwrap();
        let digest = quarantine.write_pack(&result, None, bundle.as_file_mut()).unwrap();
        let effect_id = "git-effect-v1-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let expected_commit = commit_effect(&source, &result);

        let first = target
            .install_prepared_object_pack_and_update_ref(
                bundle.reopen().unwrap(),
                bundle.path(),
                &digest,
                &expected_commit,
                "refs/heads/main",
                None,
                effect_id,
            )
            .unwrap();
        assert_eq!(first, GitRefInstallOutcome::Updated);
        let object_format = target.run_git_stdout(&["rev-parse", "--show-object-format"]).unwrap();
        assert_eq!(
            target
                .exact_ref_oid("refs/heads/main", Some(&object_format))
                .unwrap()
                .as_deref(),
            Some(result.as_str())
        );
        target.run_git(&["fsck", "--full", "--strict"]).unwrap();
        assert_no_pack_keeps(&target);

        let repeated = target
            .install_prepared_object_pack_and_update_ref(
                bundle.reopen().unwrap(),
                bundle.path(),
                &digest,
                &expected_commit,
                "refs/heads/main",
                None,
                effect_id,
            )
            .unwrap();
        assert_eq!(repeated, GitRefInstallOutcome::Adopted);
        assert_no_pack_keeps(&target);
    }

    #[test]
    fn prepared_pack_third_ref_state_leaves_target_object_store_unchanged() {
        let (source_directory, source) = init_test_repo();
        let result = commit_test_file(&source, source_directory.path(), "result.txt", b"result\n");
        let (target_directory, target) = init_test_repo();
        commit_test_file(&target, target_directory.path(), "current.txt", b"current\n");
        let quarantine = target.object_quarantine().unwrap();
        quarantine.import_object_closure(&source, &[&result]).unwrap();
        let mut bundle = tempfile::NamedTempFile::new().unwrap();
        let digest = quarantine.write_pack(&result, None, bundle.as_file_mut()).unwrap();
        let before = object_store_snapshot(&target);
        let expected_commit = commit_effect(&source, &result);

        let error = target
            .install_prepared_object_pack_and_update_ref(
                bundle.reopen().unwrap(),
                bundle.path(),
                &digest,
                &expected_commit,
                "refs/heads/main",
                None,
                "git-effect-v1-third-state",
            )
            .expect_err("an unrelated ref must fail before object installation");

        assert!(
            error.to_string().contains("changed before object installation"),
            "{error}"
        );
        assert_eq!(object_store_snapshot(&target), before);
    }

    #[test]
    fn prepared_pack_rejects_every_tampered_commit_field_before_repository_mutation() {
        let (directory, git) = init_test_repo();
        let parent = commit_test_file(&git, directory.path(), "file.txt", b"parent\n");
        let result = commit_test_file(&git, directory.path(), "file.txt", b"result\n");
        let quarantine = git.object_quarantine().unwrap();
        quarantine.import_object_closure(&git, &[&result]).unwrap();
        let mut bundle = tempfile::NamedTempFile::new().unwrap();
        let digest = quarantine
            .write_pack(&result, Some(&parent), bundle.as_file_mut())
            .unwrap();
        let exact = commit_effect(&git, &result);
        git.run_git(&["reset", "--hard", &parent]).unwrap();
        let parent_tree = git
            .run_git_stdout(&["rev-parse", &format!("{parent}^{{tree}}")])
            .unwrap();

        let mut variants = Vec::new();
        let mut tampered = exact.clone();
        tampered.tree = parent_tree;
        variants.push(("tree", tampered));
        let mut tampered = exact.clone();
        tampered.parents.clear();
        variants.push(("ordered parents", tampered));
        let mut tampered = exact.clone();
        tampered.message.push_str("substituted");
        variants.push(("raw message", tampered));
        type MetadataMutation = (&'static str, fn(&mut GitCommitEffect));
        let metadata_mutations: [MetadataMutation; 8] = [
            ("author", |effect| effect.metadata.author.push_str(" changed")),
            ("author email", |effect| {
                effect.metadata.author_email.push_str(".changed")
            }),
            ("author timestamp", |effect| effect.metadata.author_timestamp += 1),
            ("author timezone", |effect| {
                effect.metadata.author_timezone = if effect.metadata.author_timezone == "+0000" {
                    "+0100"
                } else {
                    "+0000"
                }
                .to_string()
            }),
            ("committer", |effect| effect.metadata.committer.push_str(" changed")),
            ("committer email", |effect| {
                effect.metadata.committer_email.push_str(".changed")
            }),
            ("committer timestamp", |effect| effect.metadata.committer_timestamp += 1),
            ("committer timezone", |effect| {
                effect.metadata.committer_timezone = if effect.metadata.committer_timezone == "+0000" {
                    "+0100"
                } else {
                    "+0000"
                }
                .to_string()
            }),
        ];
        for (name, mutate) in metadata_mutations {
            let mut tampered = exact.clone();
            mutate(&mut tampered);
            variants.push((name, tampered));
        }

        let ref_before = git.head_commit().unwrap();
        let index_before = git.run_git(&["ls-files", "--stage", "-z"]).unwrap().stdout;
        let worktree_before = std::fs::read(directory.path().join("file.txt")).unwrap();
        let objects_before = object_store_snapshot(&git);
        for (field, tampered) in variants {
            assert_ne!(tampered, exact, "{field} mutation must change commit authority");
            let error = git
                .install_prepared_object_pack_and_update_ref(
                    bundle.reopen().unwrap(),
                    bundle.path(),
                    &digest,
                    &tampered,
                    "refs/heads/main",
                    Some(&parent),
                    "git-effect-v1-tampered-commit",
                )
                .expect_err(field);
            assert!(error.to_string().contains("does not match"), "{field}: {error}");
            assert_eq!(git.head_commit().unwrap(), ref_before, "{field}");
            assert_eq!(
                git.run_git(&["ls-files", "--stage", "-z"]).unwrap().stdout,
                index_before,
                "{field}"
            );
            assert_eq!(
                std::fs::read(directory.path().join("file.txt")).unwrap(),
                worktree_before,
                "{field}"
            );
            assert_eq!(object_store_snapshot(&git), objects_before, "{field}");
            assert_no_pack_keeps(&git);
        }
    }

    #[test]
    fn parse_log_with_files_round_trips_records() {
        // Two commits: one with a body and two files, one bodyless with one file.
        let raw = b"\x01aaaa1111\x02feat: add planner\x02Longer body\nwith newlines\x02\x00\nsrc/lib.rs\x00src/main.rs\x00\x01bbbb2222\x02fix: typo\x02\x02\x00\nREADME.md\x00";
        let entries = parse_log_with_files(raw).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sha, "aaaa1111");
        assert_eq!(entries[0].subject, "feat: add planner");
        assert_eq!(entries[0].body.as_deref(), Some("Longer body\nwith newlines"));
        assert_eq!(
            entries[0].files,
            vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]
        );
        assert_eq!(entries[1].sha, "bbbb2222");
        assert_eq!(entries[1].body, None);
        assert_eq!(entries[1].files, vec![PathBuf::from("README.md")]);
    }

    #[test]
    fn parse_log_with_files_handles_empty_file_lists() {
        // A commit with no files (e.g. empty commit) ends after the format NUL.
        let raw = b"\x01cccc3333\x02chore: empty\x02\x02\x00";
        let entries = parse_log_with_files(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].files.is_empty());
    }

    #[test]
    fn log_with_files_reads_real_history() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let entries = git.log_with_files(None, "HEAD").unwrap();
        assert!(!entries.is_empty());
        let first = &entries[0];
        assert_eq!(first.sha.len(), 40);
        assert!(!first.subject.is_empty());
    }

    #[test]
    fn test_commit_history() {
        let git = SystemGit::open(&find_git_root()).unwrap();

        // Test with limit
        let commits = git.commit_history(Some(5)).unwrap();
        assert!(!commits.is_empty());
        assert!(commits.len() <= 5);

        // Check first commit has required fields
        let first = &commits[0];
        assert!(!first.sha.is_empty());
        assert_eq!(first.sha.len(), 40); // Full SHA
        assert!(!first.author.is_empty());
        assert!(first.timestamp > 0);
        assert!(!first.message.is_empty());
    }

    #[test]
    fn test_get_changed_files() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let head = git.head_commit().unwrap();

        // Get changed files for HEAD
        let changed = git.get_changed_files(&head).unwrap();

        // HEAD should have at least one changed file (unless it's the initial commit)
        // Just verify the call succeeds and returns valid data
        for (path, change_type) in changed {
            assert!(!path.as_os_str().is_empty());
            assert!(['A', 'M', 'D', 'R', 'C'].contains(&change_type));
        }
    }

    #[test]
    fn test_get_commits_bulk() {
        let git = SystemGit::open(&find_git_root()).unwrap();

        // Get some commit SHAs
        let history = git.commit_history(Some(5)).unwrap();
        let shas: Vec<String> = history.iter().map(|c| c.sha.clone()).collect();

        // Fetch them in bulk
        let commits = git.get_commits_bulk(&shas).unwrap();

        assert_eq!(commits.len(), shas.len());

        // Verify all commits match
        for (i, commit) in commits.iter().enumerate() {
            assert_eq!(commit.sha, shas[i]);
        }
    }

    #[test]
    fn test_read_files_bulk() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let head = git.head_commit().unwrap();

        // Prepare items to read
        let paths = [
            PathBuf::from("Cargo.toml"),
            PathBuf::from("README.md"),
            PathBuf::from("this-does-not-exist.txt"),
        ];
        let items: Vec<(&str, &Path)> = paths.iter().map(|p| (head.as_str(), p.as_path())).collect();

        let results = git.read_files_bulk(&items).unwrap();

        assert_eq!(results.len(), 3);

        // First two should have content (Cargo.toml and README.md exist)
        assert!(!results[0].is_empty(), "Cargo.toml should exist");
        assert!(!results[1].is_empty(), "README.md should exist");

        // Third should be empty (file doesn't exist)
        assert!(results[2].is_empty(), "Non-existent file should be empty");

        // Verify Cargo.toml content
        let cargo_toml = String::from_utf8_lossy(&results[0]);
        assert!(cargo_toml.contains("package") || cargo_toml.contains("dependencies"));
    }

    #[test]
    fn test_read_files_bulk_empty() {
        let git = SystemGit::open(&find_git_root()).unwrap();

        // Empty input should return empty output
        let results = git.read_files_bulk(&[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn read_blobs_bulk_exceeding_pipe_capacity_completes() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let head = git.head_commit().unwrap();
        let entry = git.tree_entry(&head, Path::new("Cargo.toml")).unwrap().unwrap();
        let payload = git
            .read_files_bulk(&[(&head, Path::new("Cargo.toml"))])
            .unwrap()
            .pop()
            .unwrap();
        let object_id = entry.object_id;
        let object_ids = vec![object_id.as_str(); 4096];

        let results = git.read_blobs_bulk(&object_ids).unwrap();

        assert_eq!(results.len(), object_ids.len());
        assert!(results.iter().all(|result| result == &payload));
    }

    #[test]
    fn test_get_commits_touching_path() {
        let git = SystemGit::open(&find_git_root()).unwrap();

        // Get commits that touched Cargo.toml
        let commits = git
            .get_commits_touching_path(Path::new("Cargo.toml"), None, "HEAD")
            .unwrap();

        // Cargo.toml should have been modified at least once
        assert!(!commits.is_empty(), "Cargo.toml should have commits");

        // Verify chronological order (oldest first)
        if commits.len() >= 2 {
            assert!(
                commits[0].timestamp <= commits[1].timestamp,
                "Commits should be in chronological order"
            );
        }
    }

    #[test]
    fn test_collect_tree_files_with_bulk() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let head = git.head_commit().unwrap();

        // Collect files from src/ directory at HEAD
        let files = git.collect_tree_files(&head, Path::new("src")).unwrap();

        // Should have at least main.rs and some core files
        assert!(!files.is_empty(), "src/ should contain files");

        // Verify all files have valid paths
        for (path, _content) in &files {
            assert!(!path.as_os_str().is_empty(), "Path should not be empty");
            // Most source files should have content (some may be empty but most won't be)
        }

        // Verify at least one Rust file exists with actual content
        let has_rust_with_content = files
            .iter()
            .any(|(path, content)| path.extension().and_then(|s| s.to_str()) == Some("rs") && !content.is_empty());
        assert!(has_rust_with_content, "Should have at least one .rs file with content");

        // Test with empty path (root directory) - should get all files
        let all_files = git.collect_tree_files(&head, Path::new("")).unwrap();
        assert!(
            all_files.len() >= files.len(),
            "Root should have at least as many files as src/"
        );
    }

    #[test]
    fn test_collect_tree_files_nonexistent() {
        let git = SystemGit::open(&find_git_root()).unwrap();
        let head = git.head_commit().unwrap();

        // Try to collect from non-existent directory
        let files = git
            .collect_tree_files(&head, Path::new("this-directory-does-not-exist-12345"))
            .unwrap();

        // Should return empty list for non-existent directory
        assert!(files.is_empty(), "Non-existent directory should return empty list");
    }

    #[test]
    fn test_parse_name_status_simple() {
        // Simple modifications
        let output = b"M\0src/main.rs\0A\0src/new.rs\0D\0src/old.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        assert_eq!(result.len(), 3);
        assert_eq!(result[0], (PathBuf::from("src/main.rs"), 'M'));
        assert_eq!(result[1], (PathBuf::from("src/new.rs"), 'A'));
        assert_eq!(result[2], (PathBuf::from("src/old.rs"), 'D'));
    }

    #[test]
    fn test_parse_name_status_rename() {
        // Rename: R100 (100% similarity) old_path -> new_path
        let output = b"R100\0src/old_name.rs\0src/new_name.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        // Should emit both: delete from old, add at new
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (PathBuf::from("src/old_name.rs"), 'D'));
        assert_eq!(result[1], (PathBuf::from("src/new_name.rs"), 'A'));
    }

    #[test]
    fn test_parse_name_status_copy() {
        // Copy: C100 (100% similarity) src_path -> dest_path
        let output = b"C095\0src/original.rs\0src/copied.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        // Should emit: source touched, dest added
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (PathBuf::from("src/original.rs"), 'M'));
        assert_eq!(result[1], (PathBuf::from("src/copied.rs"), 'A'));
    }

    #[test]
    fn test_parse_name_status_paths_with_spaces() {
        // Paths with spaces are handled by NUL separation
        let output = b"M\0path with spaces/file name.rs\0A\0another path/new file.txt\0";
        let result = parse_name_status_output_z(output).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (PathBuf::from("path with spaces/file name.rs"), 'M'));
        assert_eq!(result[1], (PathBuf::from("another path/new file.txt"), 'A'));
    }

    #[test]
    fn test_parse_name_status_rename_with_spaces() {
        // Rename with spaces in paths
        let output = b"R100\0old path/old file.rs\0new path/new file.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], (PathBuf::from("old path/old file.rs"), 'D'));
        assert_eq!(result[1], (PathBuf::from("new path/new file.rs"), 'A'));
    }

    #[test]
    fn test_parse_name_status_mixed() {
        // Mixed changes in one diff
        let output = b"M\0src/lib.rs\0R100\0src/old.rs\0src/renamed.rs\0A\0src/new.rs\0D\0src/deleted.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        assert_eq!(result.len(), 5);
        assert_eq!(result[0], (PathBuf::from("src/lib.rs"), 'M'));
        assert_eq!(result[1], (PathBuf::from("src/old.rs"), 'D')); // Rename source
        assert_eq!(result[2], (PathBuf::from("src/renamed.rs"), 'A')); // Rename dest
        assert_eq!(result[3], (PathBuf::from("src/new.rs"), 'A'));
        assert_eq!(result[4], (PathBuf::from("src/deleted.rs"), 'D'));
    }

    #[test]
    fn test_parse_name_status_empty() {
        let result = parse_name_status_output_z(b"").unwrap();
        assert!(result.is_empty());

        let result = parse_name_status_output_z(b"\0\0").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_name_status_type_change() {
        // Type change (e.g., file to symlink)
        let output = b"T\0src/link.rs\0";
        let result = parse_name_status_output_z(output).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (PathBuf::from("src/link.rs"), 'T'));
    }
}
