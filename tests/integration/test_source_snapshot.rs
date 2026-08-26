use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Result;
use cargo_rail::git::SystemGit;
#[cfg(unix)]
use cargo_rail::source::SourceEntryKind;
use cargo_rail::source::{ChangeLayer, ChangeRelation, SourceChange, SourceChangeKind, SourceSnapshot};
use cargo_rail::workspace::WorkspaceContext;
use serde_json::Value;

use crate::helpers::{TestWorkspace, git, run_cargo_rail};

fn assert_actionable_capture_error(output: &Output, message: &str, help: &str) -> Result<()> {
    anyhow::ensure!(
        output.status.code() == Some(2),
        "expected exit code 2, got {:?}",
        output.status
    );
    anyhow::ensure!(output.stderr.is_empty(), "JSON errors must keep stderr empty");
    let error: Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(error["error"] == true, "JSON error marker is missing: {error}");
    anyhow::ensure!(error["code"] == 2, "JSON error code is not 2: {error}");
    anyhow::ensure!(
        error["message"].as_str().is_some_and(|actual| actual.contains(message)),
        "unexpected error message: {}",
        error["message"]
    );
    anyhow::ensure!(error["help"] == help, "unexpected recovery help: {}", error["help"]);
    Ok(())
}

fn change<'a>(changes: &'a [SourceChange], path: &str) -> &'a SourceChange {
    let matches = changes
        .iter()
        .filter(|change| change.path.as_str() == path)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one change for {path:?}: {changes:#?}"
    );
    matches[0]
}

#[test]
fn filesystem_capture_stays_within_its_declared_root_and_uses_only_explicit_ignores() {
    let result: Result<()> = (|| {
        let root = tempfile::TempDir::new()?;
        let workspace = root.path().join("workspace");
        for (path, content) in [
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/demo\"]\n"),
            (".gitignore", "ambient-ignored/\ntarget/\n"),
            ("ambient-ignored/source.txt", "still source without Git\n"),
            (
                "crates/demo/Cargo.toml",
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
            ),
            ("crates/demo/src/lib.rs", "pub fn value() -> u8 { 1 }\n"),
            ("docs/target/example.txt", "intentional source\n"),
            ("target/debug/generated.rlib", "Cargo output\n"),
            ("custom-output/generated.txt", "declared output\n"),
        ] {
            let path = workspace.join(path);
            std::fs::create_dir_all(path.parent().expect("fixture file must have a parent"))?;
            std::fs::write(path, content)?;
        }
        std::fs::write(root.path().join("outside-secret.txt"), "outside the declared root\n")?;

        let snapshot = SourceSnapshot::capture_filesystem(
            &workspace,
            &[PathBuf::from("target"), workspace.join("custom-output")],
        )?;
        assert!(matches!(snapshot, SourceSnapshot::FilesystemBacked(_)));
        assert_eq!(
            snapshot
                .tree()
                .entries()
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            [
                ".gitignore",
                "Cargo.toml",
                "ambient-ignored/source.txt",
                "crates/demo/Cargo.toml",
                "crates/demo/src/lib.rs",
                "docs/target/example.txt",
            ]
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn filesystem_capture_records_directory_symlinks_without_following_them() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let root = tempfile::TempDir::new()?;
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(&outside)?;
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n")?;
        std::fs::write(outside.join("secret.txt"), "must not be read\n")?;
        symlink(&outside, workspace.join("external"))?;

        let snapshot = SourceSnapshot::capture_filesystem(&workspace, &[])?;
        let external = snapshot
            .tree()
            .entries()
            .iter()
            .find(|entry| entry.path.as_str() == "external")
            .expect("directory symlink must be retained as a source entry");
        assert_eq!(
            external.kind,
            SourceEntryKind::Symlink {
                target: outside.to_string_lossy().into_owned()
            }
        );
        assert!(
            snapshot
                .tree()
                .entries()
                .iter()
                .all(|entry| entry.path.as_str() != "external/secret.txt")
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cargo_aware_filesystem_capture_rejects_a_live_git_workspace() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("filesystem-capture-with-git")?;
        workspace.add_crate("demo", "0.1.0", &[])?;
        let context = WorkspaceContext::build(&workspace.path)?;

        let error = context.capture_filesystem_source().unwrap_err();
        assert_eq!(
            error.to_string(),
            "filesystem-backed source capture is only available without Git"
        );
        assert_eq!(
            error.help_message().as_deref(),
            Some("use the Git-backed worktree capture for repositories")
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn git_worktree_capture_builds_one_final_tree_and_provenance_rich_changes() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-worktree-capture")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        for (name, content) in [
            ("committed.txt", "committed-base\n"),
            ("staged.txt", "staged-base\n"),
            ("unstaged.txt", "unstaged-base\n"),
            ("deleted.txt", "delete-me\n"),
            ("old.txt", "rename-me-uniquely\n"),
            ("copy-source.txt", "copy-me-uniquely\n"),
            ("type.txt", "regular-file\n"),
            ("tool.sh", "#!/bin/sh\nexit 0\n"),
        ] {
            std::fs::write(crate_root.join(name), content)?;
        }
        std::fs::write(ws.path.join(".gitignore"), "ignored.txt\n")?;
        let base = ws.commit("Add source capture fixture")?;

        std::fs::write(crate_root.join("committed.txt"), "committed-final\n")?;
        ws.commit("Commit one source layer")?;

        std::fs::write(crate_root.join("staged.txt"), "staged-final\n")?;
        git(&ws.path, &["add", "crates/demo/staged.txt"])?;
        std::fs::write(crate_root.join("unstaged.txt"), "unstaged-final\n")?;
        std::fs::remove_file(crate_root.join("deleted.txt"))?;
        git(&ws.path, &["mv", "crates/demo/old.txt", "crates/demo/new.txt"])?;
        std::fs::copy(crate_root.join("copy-source.txt"), crate_root.join("copied.txt"))?;
        git(&ws.path, &["add", "crates/demo/copied.txt"])?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt as _, symlink};

            std::fs::remove_file(crate_root.join("type.txt"))?;
            symlink("copy-source.txt", crate_root.join("type.txt"))?;
            git(&ws.path, &["add", "crates/demo/type.txt"])?;

            let mut permissions = std::fs::metadata(crate_root.join("tool.sh"))?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(crate_root.join("tool.sh"), permissions)?;
        }
        std::fs::write(crate_root.join("untracked.txt"), "untracked\n")?;
        std::fs::create_dir_all(ws.path.join("docs/target"))?;
        std::fs::write(ws.path.join("docs/target/example.txt"), "intentional source\n")?;
        std::fs::write(ws.path.join("ignored.txt"), "ignored\n")?;

        let git = SystemGit::open(&ws.path)?;
        let (snapshot, changes) = SourceSnapshot::capture_git_worktree(&git, &base)?;
        let paths: Vec<_> = snapshot
            .tree()
            .entries()
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]), "tree is not sorted");
        assert!(paths.contains(&"crates/demo/new.txt"));
        assert!(paths.contains(&"crates/demo/copied.txt"));
        assert!(paths.contains(&"crates/demo/untracked.txt"));
        assert!(paths.contains(&"docs/target/example.txt"));
        assert!(!paths.contains(&"crates/demo/old.txt"));
        assert!(!paths.contains(&"crates/demo/deleted.txt"));
        assert!(!paths.contains(&"ignored.txt"));

        #[cfg(unix)]
        {
            let executable = snapshot
                .tree()
                .entries()
                .iter()
                .find(|entry| entry.path.as_str() == "crates/demo/tool.sh")
                .expect("missing executable");
            assert!(matches!(
                executable.kind,
                SourceEntryKind::RegularFile { executable: true, .. }
            ));
            let symlink = snapshot
                .tree()
                .entries()
                .iter()
                .find(|entry| entry.path.as_str() == "crates/demo/type.txt")
                .expect("missing symlink");
            assert_eq!(
                symlink.kind,
                SourceEntryKind::Symlink {
                    target: "copy-source.txt".to_string()
                }
            );
        }

        let changes = changes.entries();
        assert_eq!(
            change(changes, "crates/demo/committed.txt").kind,
            SourceChangeKind::Modified
        );
        assert!(
            change(changes, "crates/demo/committed.txt")
                .provenance
                .contains(ChangeLayer::Committed)
        );
        assert!(
            change(changes, "crates/demo/staged.txt")
                .provenance
                .contains(ChangeLayer::Staged)
        );
        assert!(
            change(changes, "crates/demo/unstaged.txt")
                .provenance
                .contains(ChangeLayer::Unstaged)
        );
        assert_eq!(
            change(changes, "crates/demo/deleted.txt").kind,
            SourceChangeKind::Deleted
        );
        assert_eq!(
            change(changes, "crates/demo/old.txt").relation,
            Some(ChangeRelation::RenamedTo(cargo_rail::source::RepositoryPath::new(
                Path::new("crates/demo/new.txt")
            )?))
        );
        assert_eq!(
            change(changes, "crates/demo/new.txt").relation,
            Some(ChangeRelation::RenamedFrom(cargo_rail::source::RepositoryPath::new(
                Path::new("crates/demo/old.txt")
            )?))
        );
        assert_eq!(
            change(changes, "crates/demo/copied.txt").relation,
            Some(ChangeRelation::CopiedFrom(cargo_rail::source::RepositoryPath::new(
                Path::new("crates/demo/copy-source.txt")
            )?))
        );
        #[cfg(unix)]
        assert_eq!(
            change(changes, "crates/demo/type.txt").kind,
            SourceChangeKind::TypeChanged
        );
        assert!(
            change(changes, "crates/demo/untracked.txt")
                .provenance
                .contains(ChangeLayer::Untracked)
        );
        assert!(
            change(changes, "docs/target/example.txt")
                .provenance
                .contains(ChangeLayer::Untracked)
        );
        #[cfg(unix)]
        {
            assert_eq!(change(changes, "crates/demo/tool.sh").kind, SourceChangeKind::Modified);
            assert!(
                change(changes, "crates/demo/tool.sh")
                    .provenance
                    .contains(ChangeLayer::Unstaged)
            );
        }
        assert!(
            changes
                .windows(2)
                .all(|pair| pair[0].path.as_str() < pair[1].path.as_str()),
            "change set is not sorted"
        );
        assert_eq!(
            changes
                .iter()
                .filter(|change| change.path.as_str() == "crates/demo/new.txt")
                .count(),
            1
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn plan_excludes_ignored_and_default_generated_state_without_hiding_named_source_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-generated-state-default")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        ws.commit("Add generated-state fixture")?;

        for (path, content) in [
            ("target/debug/generated.rlib", "cargo output\n"),
            ("target/cargo-rail/metadata.json", "cargo-rail output\n"),
            ("ignored-state/generated.txt", "ignored output\n"),
            ("docs/target/example.txt", "intentional source\n"),
        ] {
            let path = ws.path.join(path);
            std::fs::create_dir_all(path.parent().expect("fixture path must have a parent"))?;
            std::fs::write(path, content)?;
        }

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(
            output.status.success(),
            "plan failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: Value = serde_json::from_slice(&output.stdout)?;
        let paths = plan["changes"]["files"]
            .as_array()
            .expect("files should be an array")
            .iter()
            .map(|file| file["path"].as_str().expect("planned path should be a string"))
            .collect::<Vec<_>>();
        assert_eq!(paths, ["docs/target/example.txt"]);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn plan_excludes_exact_custom_cargo_and_cargo_rail_roots_without_blanket_target_filtering() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-generated-state-custom")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        std::fs::create_dir_all(ws.path.join(".cargo"))?;
        std::fs::write(
            ws.path.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"cargo-output\"\nbuild-dir = \"cargo-build\"\n",
        )?;
        ws.commit("Configure a custom Cargo target directory")?;

        for (path, content) in [
            ("cargo-output/debug/generated.rlib", "custom Cargo output\n"),
            ("cargo-build/debug/intermediate.rmeta", "custom Cargo build state\n"),
            ("target/cargo-rail/receipts/generated.json", "cargo-rail output\n"),
            ("target/intentional.txt", "intentional source\n"),
        ] {
            let path = ws.path.join(path);
            std::fs::create_dir_all(path.parent().expect("fixture path must have a parent"))?;
            std::fs::write(path, content)?;
        }

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(
            output.status.success(),
            "plan failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: Value = serde_json::from_slice(&output.stdout)?;
        let paths = plan["changes"]["files"]
            .as_array()
            .expect("files should be an array")
            .iter()
            .map(|file| file["path"].as_str().expect("planned path should be a string"))
            .collect::<Vec<_>>();
        assert_eq!(paths, ["target/intentional.txt"]);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn historical_plan_excludes_resolved_generated_roots_without_reading_worktree_state() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-generated-state-history")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        let base = ws.commit("Add historical generated-state fixture")?;

        for (path, content) in [
            ("target/debug/generated.rlib", "committed Cargo output\n"),
            ("target/cargo-rail/metadata.json", "committed cargo-rail output\n"),
            ("docs/target/example.txt", "committed intentional source\n"),
        ] {
            let path = ws.path.join(path);
            std::fs::create_dir_all(path.parent().expect("fixture path must have a parent"))?;
            std::fs::write(path, content)?;
        }
        let head = ws.commit("Commit historical generated and source paths")?;
        std::fs::write(ws.path.join("untracked.txt"), "must not enter object comparison\n")?;

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--from", &base, "--to", &head, "--json"])?;
        assert!(output.status.success());
        let plan: Value = serde_json::from_slice(&output.stdout)?;
        let paths = plan["changes"]["files"]
            .as_array()
            .expect("files should be an array")
            .iter()
            .map(|file| file["path"].as_str().expect("planned path should be a string"))
            .collect::<Vec<_>>();
        assert_eq!(paths, ["docs/target/example.txt"]);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn generated_root_filter_drops_cross_boundary_rename_relations() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-generated-state-rename")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(ws.path.join(".gitignore"), "ignored-state/\n")?;
        std::fs::write(crate_root.join("moved.txt"), "move into generated state\n")?;
        let base = ws.commit("Add generated-boundary rename fixture")?;

        std::fs::create_dir_all(ws.path.join("target/debug"))?;
        git(&ws.path, &["mv", "crates/demo/moved.txt", "target/debug/moved.txt"])?;

        let ctx = WorkspaceContext::build_with_source_capture(&ws.path, true)?;
        let capture = ctx.source_capture().expect("source capture should be present");
        let changes = capture.changes_from(ctx.git()?.git(), &base)?;
        assert_eq!(changes.entries().len(), 1);
        assert_eq!(changes.entries()[0].path.as_str(), "crates/demo/moved.txt");
        assert_eq!(changes.entries()[0].kind, SourceChangeKind::Deleted);
        assert_eq!(changes.entries()[0].relation, None);
        assert!(
            !capture
                .snapshot()
                .tree()
                .entries()
                .iter()
                .any(|entry| entry.path.as_str().starts_with("target/"))
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn git_worktree_capture_rejects_symlinked_parent_escape() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let ws = TestWorkspace::new_named("source-worktree-parent-symlink")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        let guarded = crate_root.join("guarded");
        std::fs::create_dir_all(&guarded)?;
        std::fs::write(guarded.join("secret.txt"), "inside\n")?;
        ws.commit("Add guarded source")?;

        std::fs::remove_dir_all(&guarded)?;
        let outside = tempfile::tempdir()?;
        std::fs::write(outside.path().join("secret.txt"), "outside\n")?;
        symlink(outside.path(), &guarded)?;

        let git = SystemGit::open(&ws.path)?;
        let error = SourceSnapshot::capture_git_worktree(&git, "HEAD").expect_err("symlinked parent must be rejected");
        assert!(error.to_string().contains("through symbolic-link ancestor"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_set_does_not_double_count_a_recreated_untracked_path() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-worktree-recreated-path")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        let source = crate_root.join("recreated.txt");
        std::fs::write(&source, "base\n")?;
        ws.commit("Add recreated-path fixture")?;

        git(&ws.path, &["rm", "--cached", "crates/demo/recreated.txt"])?;
        let git_backend = SystemGit::open(&ws.path)?;
        let (_, unchanged) = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")?;
        assert!(
            unchanged.entries().is_empty(),
            "an identical final path is not a change"
        );

        std::fs::write(&source, "modified while untracked\n")?;
        let (snapshot, modified) = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")?;
        assert_eq!(
            snapshot
                .tree()
                .entries()
                .iter()
                .filter(|entry| entry.path.as_str() == "crates/demo/recreated.txt")
                .count(),
            1
        );
        let modified = change(modified.entries(), "crates/demo/recreated.txt");
        assert_eq!(modified.kind, SourceChangeKind::Modified);
        assert!(modified.provenance.contains(ChangeLayer::Staged));
        assert!(modified.provenance.contains(ChangeLayer::Untracked));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn worktree_capture_rejects_assume_unchanged_index_entries() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-worktree-assume-unchanged")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        let source = crate_root.join("hidden.txt");
        std::fs::write(&source, "base\n")?;
        ws.commit("Add assume-unchanged fixture")?;

        git(
            &ws.path,
            &["update-index", "--assume-unchanged", "crates/demo/hidden.txt"],
        )?;
        std::fs::write(&source, "hidden worktree change\n")?;

        let git_backend = SystemGit::open(&ws.path)?;
        let error = SourceSnapshot::capture_git_worktree(&git_backend, "HEAD")
            .expect_err("assume-unchanged paths make capture ambiguous");
        assert!(error.to_string().contains("assume-unchanged index entry"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn plan_rejects_an_unmerged_index_with_actionable_diagnostics() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("source-worktree-unmerged-index")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        let conflict = crate_root.join("conflict.txt");
        std::fs::write(&conflict, "base\n")?;
        ws.commit("Add conflict fixture")?;

        git(&ws.path, &["checkout", "-b", "conflict-side"])?;
        std::fs::write(&conflict, "side\n")?;
        ws.commit("Change conflict fixture on side")?;

        git(&ws.path, &["checkout", "main"])?;
        std::fs::write(&conflict, "main\n")?;
        ws.commit("Change conflict fixture on main")?;

        let merge = Command::new("git")
            .current_dir(&ws.path)
            .args(["merge", "--no-edit", "conflict-side"])
            .output()?;
        assert_eq!(
            merge.status.code(),
            Some(1),
            "fixture merge must conflict: stdout={} stderr={}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
        let unmerged = git(&ws.path, &["ls-files", "--unmerged", "--", "crates/demo/conflict.txt"])?;
        assert!(!unmerged.stdout.is_empty(), "fixture did not create an unmerged index");

        let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert_actionable_capture_error(
            &output,
            "cannot capture unmerged index entry 'crates/demo/conflict.txt'",
            "resolve the index conflict and stage the resolution before retrying",
        )
    })();
    super::helpers::finish_test(result);
}

#[test]
fn plan_rejects_an_unsupported_filesystem_object_with_actionable_diagnostics() -> Result<()> {
    let ws = TestWorkspace::new_named("source-worktree-unsupported-object")?;
    let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
    let unsupported = crate_root.join("tracked-object");
    std::fs::write(&unsupported, "regular file\n")?;
    ws.commit("Add filesystem-object fixture")?;

    std::fs::remove_file(&unsupported)?;
    std::fs::create_dir(&unsupported)?;

    let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
    assert_actionable_capture_error(
        &output,
        "cannot capture unsupported filesystem object 'crates/demo/tracked-object'",
        "replace it with a regular file or symbolic link before retrying",
    )
}

#[cfg(unix)]
#[test]
fn plan_rejects_a_unix_socket_without_reading_it() -> Result<()> {
    use std::os::unix::net::UnixListener;

    let ws = TestWorkspace::new_named("source-worktree-unsupported-socket")?;
    let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
    let unsupported = crate_root.join("tracked-socket");
    std::fs::write(&unsupported, "regular file\n")?;
    ws.commit("Add socket fixture")?;

    std::fs::remove_file(&unsupported)?;
    let _listener = UnixListener::bind(&unsupported)?;

    let output = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
    assert_actionable_capture_error(
        &output,
        "cannot capture unsupported filesystem object 'crates/demo/tracked-socket'",
        "replace it with a regular file or symbolic link before retrying",
    )
}

#[cfg(unix)]
#[test]
fn plan_rejects_exact_file_drift_during_capture() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt as _;

        let ws = TestWorkspace::new_named("source-capture-drift")?;
        let crate_root = ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(crate_root.join("a-victim.txt"), "before!!\n")?;
        ws.commit("Add deterministic capture-drift fixture")?;
        std::fs::write(crate_root.join("a-victim.txt"), "during!!\n")?;

        let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
        assert!(real_git.status.success(), "failed to resolve the real Git executable");
        let real_git = String::from_utf8(real_git.stdout)?.trim().to_string();

        let wrapper_dir = ws.path.join(".git/rail-test-bin");
        std::fs::create_dir_all(&wrapper_dir)?;
        let wrapper = wrapper_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
for argument in "$@"; do
  if [ "$argument" = "status" ]; then
    count=0
    if [ -f "$CARGO_RAIL_TEST_DRIFT_COUNTER" ]; then
      IFS= read -r count < "$CARGO_RAIL_TEST_DRIFT_COUNTER"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$CARGO_RAIL_TEST_DRIFT_COUNTER"
    if [ "$count" -eq 2 ]; then
      output="$CARGO_RAIL_TEST_DRIFT_COUNTER-output"
      "$CARGO_RAIL_TEST_REAL_GIT" "$@" > "$output"
      result=$?
      printf 'after!!!\n' > "$CARGO_RAIL_TEST_DRIFT_FILE"
      command cat "$output"
      command rm "$output"
      exit "$result"
    fi
    break
  fi
done
exec "$CARGO_RAIL_TEST_REAL_GIT" "$@"
"#,
        )?;
        let mut permissions = std::fs::metadata(&wrapper)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper, permissions)?;

        let path = std::env::join_paths(
            std::iter::once(wrapper_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )?;
        let counter = ws.path.join(".git/rail-drift-count");

        let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .current_dir(&ws.path)
            .env("PATH", path)
            .env("CARGO_RAIL_TEST_REAL_GIT", real_git)
            .env("CARGO_RAIL_TEST_DRIFT_COUNTER", counter)
            .env("CARGO_RAIL_TEST_DRIFT_FILE", crate_root.join("a-victim.txt"))
            .args(["rail", "plan", "--since", "HEAD", "--json"])
            .output()?;
        assert_actionable_capture_error(
            &output,
            "source file 'crates/demo/a-victim.txt' changed during capture",
            "retry after concurrent filesystem and Git operations have finished",
        )
    })();
    super::helpers::finish_test(result);
}
