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
