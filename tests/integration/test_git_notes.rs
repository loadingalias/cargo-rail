use anyhow::Result;
use cargo_rail::git::mappings::{HistorySide, MappingStore, OriginContext, repository_identity};

use crate::helpers::git;

fn commit(repo: &std::path::Path, message: &str) -> Result<String> {
    git(repo, &["commit", "--allow-empty", "-m", message])?;
    let output = git(repo, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[test]
fn ordinary_clone_recovers_mapping_without_notes_ref() {
    let result: Result<()> = (|| {
        let source = tempfile::TempDir::new()?;
        git(source.path(), &["init", "-b", "main"])?;
        git(source.path(), &["config", "user.name", "Test User"])?;
        git(source.path(), &["config", "user.email", "test@example.com"])?;
        let source_commit = commit(source.path(), "source")?;
        let source_identity = repository_identity(source.path())?;

        let target = tempfile::TempDir::new()?;
        git(target.path(), &["init", "-b", "main"])?;
        git(target.path(), &["config", "user.name", "Test User"])?;
        git(target.path(), &["config", "user.email", "test@example.com"])?;
        let origin = OriginContext::new(source_identity.clone(), "demo", "v1-sha256-test")?;
        let target_commit = commit(target.path(), &format!("split\n\n{}", origin.trailer(&source_commit)?))?;

        let clone_parent = tempfile::TempDir::new()?;
        let clone = clone_parent.path().join("clone");
        git(
            clone_parent.path(),
            &["clone", target.path().to_str().unwrap(), "clone"],
        )?;
        let notes = git(&clone, &["for-each-ref", "--format=%(refname)", "refs/notes/rail"])?;
        assert!(notes.stdout.is_empty(), "ordinary clone must not need mapping notes");

        let mut mappings = MappingStore::new("demo".to_string());
        mappings.load_history(&clone, HistorySide::Target, &source_identity)?;
        assert_eq!(mappings.get_mapping(&source_commit), Some(target_commit));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn legacy_note_and_history_divergence_is_rejected() {
    let result: Result<()> = (|| {
        let repo = tempfile::TempDir::new()?;
        git(repo.path(), &["init", "-b", "main"])?;
        git(repo.path(), &["config", "user.name", "Test User"])?;
        git(repo.path(), &["config", "user.email", "test@example.com"])?;
        let source_commit = commit(repo.path(), "source object")?;
        let source_identity = repository_identity(repo.path())?;
        let origin = OriginContext::new(source_identity.clone(), "demo", "v1-sha256-test")?;
        let history_target = commit(repo.path(), &format!("split\n\n{}", origin.trailer(&source_commit)?))?;
        let note_target = commit(repo.path(), "different target")?;
        git(
            repo.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/demo",
                "add",
                "-m",
                &note_target,
                &source_commit,
            ],
        )?;

        let mut mappings = MappingStore::new("demo".to_string());
        mappings.load_history(repo.path(), HistorySide::Target, &source_identity)?;
        let error = mappings.load_legacy_notes(repo.path()).unwrap_err();
        assert!(error.to_string().contains("maps to both"));
        assert_ne!(history_target, note_target);
        Ok(())
    })();
    super::helpers::finish_test(result);
}
