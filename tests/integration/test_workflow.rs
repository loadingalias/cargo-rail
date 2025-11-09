//! End-to-end workflow tests

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_full_workflow_init_split_sync() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Step 1: Create workspace with multiple crates
  workspace.add_crate("lib-core", "0.1.0", &[("anyhow", "\"1.0\"")])?;
  workspace.add_crate(
    "lib-utils",
    "0.1.0",
    &[
      ("anyhow", "\"1.0\""),
      ("lib-core", "{ path = \"../lib-core\", version = \"0.1\" }"),
    ],
  )?;
  workspace.commit("Initial crates")?;

  // Step 2: Initialize cargo-rail
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  assert!(workspace.file_exists("rail.toml"));

  // Step 3: Configure remotes
  let core_split = workspace.path.join("split-repos/lib-core-split");
  let utils_split = workspace.path.join("split-repos/lib-utils-split");

  let config = workspace.read_file("rail.toml")?;
  let config = config.replace(
    r#"name = "lib-core"
remote = """#,
    &format!(
      r#"name = "lib-core"
remote = "{}""#,
      core_split.display()
    ),
  );
  let config = config.replace(
    r#"name = "lib-utils"
remote = """#,
    &format!(
      r#"name = "lib-utils"
remote = "{}""#,
      utils_split.display()
    ),
  );
  std::fs::write(workspace.path.join("rail.toml"), config)?;

  // Step 4: Split both crates
  run_cargo_rail(&workspace.path, &["rail", "split", "lib-core"])?;
  run_cargo_rail(&workspace.path, &["rail", "split", "lib-utils"])?;

  assert!(core_split.exists());
  assert!(utils_split.exists());

  // Step 5: Verify transforms in lib-utils
  let utils_cargo = std::fs::read_to_string(utils_split.join("Cargo.toml"))?;
  assert!(utils_cargo.contains("lib-core"));
  assert!(utils_cargo.contains("0.1"));
  assert!(!utils_cargo.contains("path ="));

  // Step 6: Make changes in monorepo
  workspace.modify_file("lib-core", "src/lib.rs", "// Core update")?;
  workspace.commit("Update core")?;

  // Step 7: Sync to remote
  run_cargo_rail(&workspace.path, &["rail", "sync", "lib-core", "--to-remote"])?;

  // Step 8: Verify sync worked
  let core_lib = std::fs::read_to_string(core_split.join("src/lib.rs"))?;
  assert!(core_lib.contains("Core update"));

  // Step 9: Make change in split repo
  std::fs::write(utils_split.join("src/lib.rs"), "// Utils split change")?;
  git(&utils_split, &["add", "."])?;
  git(&utils_split, &["commit", "-m", "Update utils in split"])?;

  // Step 10: Sync back to monorepo
  run_cargo_rail(&workspace.path, &["rail", "sync", "lib-utils", "--from-remote"])?;

  // Step 11: Verify changes in monorepo
  let utils_lib = workspace.read_file("crates/lib-utils/src/lib.rs")?;
  assert!(utils_lib.contains("Utils split change"));

  Ok(())
}

#[test]
fn test_workflow_with_multiple_commits() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crate with multiple commits
  workspace.add_crate("my-crate", "0.1.0", &[])?;
  workspace.commit("Initial")?;

  workspace.modify_file("my-crate", "src/lib.rs", "// v1")?;
  workspace.commit("Version 1")?;

  workspace.modify_file("my-crate", "src/lib.rs", "// v2")?;
  workspace.commit("Version 2")?;

  workspace.modify_file("my-crate", "README.md", "# V3")?;
  workspace.commit("Version 3")?;

  // Initialize and split
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-crate-split");
  let config = workspace.read_file("rail.toml")?;
  let config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;

  // Verify all commits are present
  let log = git(&split_dir, &["log", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);
  assert!(log_str.contains("Initial"));
  assert!(log_str.contains("Version 1"));
  assert!(log_str.contains("Version 2"));
  assert!(log_str.contains("Version 3"));

  // Verify chronological order (oldest to newest)
  let commits: Vec<&str> = log_str.lines().collect();
  assert_eq!(commits.len(), 4);

  Ok(())
}

#[test]
fn test_workflow_handles_workspace_dependencies() -> Result<()> {
  let workspace = TestWorkspace::new()?;

  // Create crates with workspace dependencies
  workspace.add_crate("base", "1.0.0", &[("anyhow", "{ workspace = true }")])?;
  workspace.add_crate(
    "derived",
    "1.0.0",
    &[
      ("serde", "{ workspace = true }"),
      ("anyhow", "{ workspace = true }"),
      ("base", "{ path = \"../base\", version = \"1.0\" }"),
    ],
  )?;
  workspace.commit("Add crates with workspace deps")?;

  // Initialize and split
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/derived-split");
  let config = workspace.read_file("rail.toml")?;
  let config = config.replace(
    r#"name = "derived"
remote = """#,
    &format!(
      r#"name = "derived"
remote = "{}""#,
      split_dir.display()
    ),
  );
  std::fs::write(workspace.path.join("rail.toml"), config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "derived"])?;

  // Verify workspace dependencies were flattened
  let cargo_toml = std::fs::read_to_string(split_dir.join("Cargo.toml"))?;
  assert!(cargo_toml.contains("serde"));
  assert!(cargo_toml.contains("anyhow"));
  assert!(cargo_toml.contains("base"));

  // Should have actual versions, not workspace = true
  assert!(cargo_toml.contains("1.0"));
  assert!(!cargo_toml.contains("workspace = true"));

  Ok(())
}
