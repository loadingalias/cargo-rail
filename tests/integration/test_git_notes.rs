use crate::helpers::{TestWorkspace, git};
use anyhow::Result;
use cargo_rail::git::mappings::MappingStore;

#[test]
fn test_git_notes_basic_flow() -> Result<()> {
  let workspace = TestWorkspace::new()?;
  let repo_path = &workspace.path;

  // Create some real commits to attach notes to
  workspace.add_crate("test-crate", "0.1.0", &[])?;
  workspace.modify_file("test-crate", "file1.txt", "content1")?;
  let sha1 = workspace.commit("Commit 1")?;

  workspace.modify_file("test-crate", "file2.txt", "content2")?;
  let sha2 = workspace.commit("Commit 2")?;

  // Create a mapping store
  let mut store = MappingStore::new("test-crate".to_string());

  // Record some mappings
  store.record_mapping(&sha1, "remote_sha_1")?;
  store.record_mapping(&sha2, "remote_sha_2")?;

  // Save to git notes
  store.save(repo_path)?;

  // Verify notes were created
  let output = git(repo_path, &["notes", "--ref=refs/notes/rail/test-crate", "list"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains(&sha1) || stdout.contains(&sha2));

  // Load from a fresh store
  let mut loaded_store = MappingStore::new("test-crate".to_string());
  loaded_store.load(repo_path)?;

  assert_eq!(loaded_store.get_mapping(&sha1)?, Some("remote_sha_1".to_string()));
  assert_eq!(loaded_store.get_mapping(&sha2)?, Some("remote_sha_2".to_string()));

  Ok(())
}

#[test]
fn test_git_notes_conflict_resolution() -> Result<()> {
  // 1. Setup "remote" repo
  let remote_root = tempfile::TempDir::new()?;
  let remote_path = remote_root.path();
  git(remote_path, &["init", "--initial-branch=main"])?;
  git(remote_path, &["config", "user.name", "Remote User"])?;
  git(remote_path, &["config", "user.email", "remote@example.com"])?;

  // Create initial commit so we have a valid ref
  std::fs::write(remote_path.join("README.md"), "Remote")?;
  git(remote_path, &["add", "."])?;
  git(remote_path, &["commit", "-m", "Initial commit"])?;

  // Create a commit to attach notes to
  std::fs::write(remote_path.join("file.txt"), "content")?;
  git(remote_path, &["add", "."])?;
  git(remote_path, &["commit", "-m", "Target commit"])?;
  let output = git(remote_path, &["rev-parse", "HEAD"])?;
  let target_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

  // 2. Setup "local" repo as a clone
  let local_root = tempfile::TempDir::new()?;
  let local_path = local_root.path();
  git(local_path, &["clone", remote_path.to_str().unwrap(), "."])?;
  git(local_path, &["config", "user.name", "Local User"])?;
  git(local_path, &["config", "user.email", "local@example.com"])?;

  // 3. Create divergent notes

  // Remote side: Add mapping for target_sha -> remote_val
  {
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping(&target_sha, "remote_val")?;
    store.save(remote_path)?;
    // We need to push these notes to the remote's refs/notes/rail/test-crate
    // But here we are acting ON the remote repo directly, so save() wrote to local refs.
    // Since remote_path IS the remote, we are good.
  }

  // Local side: Add mapping for target_sha -> local_val (CONFLICT!)
  {
    let mut store = MappingStore::new("test-crate".to_string());
    store.record_mapping(&target_sha, "local_val")?;
    store.save(local_path)?;
  }

  // 4. Try to fetch/merge notes in local repo
  let store = MappingStore::new("test-crate".to_string());

  // This should trigger the conflict resolution logic (union merge)
  store.fetch_notes(local_path, "origin")?;

  // 5. Verify both mappings exist (union merge keeps both lines in the note)
  // Note: MappingStore::load reads the note. If it has multiple lines, it might pick one or fail parsing?
  // Let's check MappingStore::load implementation.
  // It reads `git notes show`. If union merge happened, the note content will be the concatenation.
  // "remote_val\nlocal_val" or similar.
  // MappingStore::load does `let target_sha = note_content.trim();`.
  // If there are multiple lines, `trim()` might keep them or if it expects a single SHA it might be weird.
  // But for the purpose of robustness, we just want to ensure `fetch_notes` didn't crash and merged successfully.

  // Let's check the actual note content
  let output = git(
    local_path,
    &["notes", "--ref=refs/notes/rail/test-crate", "show", &target_sha],
  )?;
  let content = String::from_utf8_lossy(&output.stdout);

  println!("Merged note content:\n{}", content);
  assert!(content.contains("remote_val"));
  assert!(content.contains("local_val"));

  Ok(())
}
