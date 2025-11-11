//! Integration tests for Node.js/TypeScript workspace support

use crate::helpers::*;
use anyhow::Result;

#[test]
fn test_node_pnpm_workspace_detection() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Add packages
  workspace.add_package("@test/pkg-a", "0.1.0", &[])?;
  workspace.add_package("@test/pkg-b", "0.2.0", &[("@test/pkg-a", "workspace:*")])?;
  workspace.commit("Add packages")?;

  // Initialize cargo-rail config
  let output = run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Verify it detected the Node workspace
  assert!(
    stdout.contains("Found workspace"),
    "Should detect workspace:\n{}",
    stdout
  );
  assert!(stdout.contains("@test/pkg-a"), "Should find pkg-a");
  assert!(stdout.contains("@test/pkg-b"), "Should find pkg-b");

  // Verify config was created
  assert!(workspace.path.join("rail.toml").exists());

  // Read and verify config contents
  let config = workspace.read_file("rail.toml")?;
  assert!(config.contains("@test/pkg-a"));
  assert!(config.contains("@test/pkg-b"));
  assert!(config.contains("package.json")); // Should include package.json in patterns

  Ok(())
}

#[test]
fn test_node_npm_workspace_detection() -> Result<()> {
  let workspace = NodeWorkspace::new_npm()?;

  // Add packages
  workspace.add_package("my-lib", "1.0.0", &[])?;
  workspace.add_package("my-app", "1.0.0", &[("my-lib", "workspace:*")])?;
  workspace.commit("Add packages")?;

  // Initialize cargo-rail config
  let output = run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Verify it detected the Node workspace
  assert!(stdout.contains("Found workspace"));
  assert!(stdout.contains("my-lib"));
  assert!(stdout.contains("my-app"));

  // Verify config was created
  assert!(workspace.path.join("rail.toml").exists());

  Ok(())
}

#[test]
fn test_node_workspace_with_history() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create package with history
  workspace.add_package("@test/my-package", "0.1.0", &[])?;
  workspace.commit("Initial package")?;

  workspace.modify_file("my-package", "src/index.js", "export const version = '0.1.1';")?;
  workspace.commit("Update version")?;

  workspace.modify_file("my-package", "README.md", "# My Package v0.1.1")?;
  workspace.commit("Update README")?;

  // Verify git history
  let log = workspace.git_log(4)?;
  assert_eq!(log.len(), 4); // 3 commits + initial workspace setup
  assert!(log[0].contains("Update README"));
  assert!(log[1].contains("Update version"));
  assert!(log[2].contains("Initial package"));

  Ok(())
}

#[test]
fn test_node_workspace_protocol_transformation() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create interdependent packages
  workspace.add_package("@test/lib", "0.1.0", &[])?;
  workspace.add_package("@test/app", "0.2.0", &[("@test/lib", "workspace:*")])?;
  workspace.commit("Add packages with workspace dependencies")?;

  // Verify workspace: protocol is in package.json
  let app_pkg_json = workspace.read_file("packages/app/package.json")?;
  assert!(app_pkg_json.contains("workspace:*"), "Should have workspace: protocol");
  assert!(app_pkg_json.contains("@test/lib"));

  // Initialize rail
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;

  // Verify rail.toml was created
  assert!(workspace.path.join("rail.toml").exists());

  Ok(())
}

#[test]
fn test_node_auxiliary_files() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create auxiliary files
  std::fs::write(workspace.path.join(".nvmrc"), "18.17.0")?;
  std::fs::write(
    workspace.path.join("tsconfig.json"),
    r#"{"compilerOptions": {"target": "ES2020"}}"#,
  )?;
  std::fs::write(workspace.path.join(".eslintrc"), r#"{"extends": "standard"}"#)?;

  workspace.add_package("@test/pkg", "1.0.0", &[])?;
  workspace.commit("Add package with aux files")?;

  // Initialize rail
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;

  // Verify files exist
  assert!(workspace.path.join(".nvmrc").exists());
  assert!(workspace.path.join("tsconfig.json").exists());
  assert!(workspace.path.join(".eslintrc").exists());

  Ok(())
}

#[test]
fn test_node_multiple_packages() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create multiple packages
  workspace.add_package("@test/utils", "1.0.0", &[])?;
  workspace.add_package("@test/core", "2.0.0", &[("@test/utils", "workspace:*")])?;
  workspace.add_package("@test/cli", "1.5.0", &[("@test/core", "workspace:*")])?;
  workspace.commit("Add multiple packages")?;

  // Initialize rail
  let output = run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Verify all packages detected
  assert!(stdout.contains("Found 3 packages"));
  assert!(stdout.contains("@test/utils"));
  assert!(stdout.contains("@test/core"));
  assert!(stdout.contains("@test/cli"));

  // Verify config
  let config = workspace.read_file("rail.toml")?;
  assert!(config.contains("@test/utils"));
  assert!(config.contains("@test/core"));
  assert!(config.contains("@test/cli"));

  Ok(())
}

#[test]
fn test_node_split_creates_repo() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  workspace.add_package("@test/my-package", "0.1.0", &[])?;
  workspace.modify_file("my-package", "src/index.js", "export const hello = 'world';")?;
  workspace.commit("Add my-package")?;

  // Initialize cargo-rail config
  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;

  // Update config to set local remote path
  let split_dir = workspace.path.join("split-repos/my-package-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  // Run split
  run_cargo_rail(&workspace.path, &["rail", "split", "@test/my-package", "--apply"])?;

  // Verify split repo exists
  assert!(split_dir.exists());
  assert!(split_dir.join(".git").exists());
  assert!(split_dir.join("package.json").exists());
  assert!(split_dir.join("src/index.js").exists());

  // Verify history was preserved
  let log = git(&split_dir, &["log", "--oneline"])?;
  let log_str = String::from_utf8_lossy(&log.stdout);
  assert!(log_str.contains("Add my-package"));

  Ok(())
}

#[test]
fn test_node_sync_mono_to_remote() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create and split a package
  workspace.add_package("@test/my-package", "0.1.0", &[])?;
  workspace.commit("Add my-package")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-package-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "@test/my-package", "--apply"])?;

  // Make changes in monorepo
  workspace.modify_file(
    "my-package",
    "src/index.js",
    "// Monorepo change\nexport const updated = true;",
  )?;
  workspace.commit("Update in monorepo")?;

  // Sync to remote
  run_cargo_rail(
    &workspace.path,
    &["rail", "sync", "@test/my-package", "--to-remote", "--apply"],
  )?;

  // Verify changes appear in split repo
  let split_index = std::fs::read_to_string(split_dir.join("src/index.js"))?;
  assert!(split_index.contains("Monorepo change"));
  assert!(split_index.contains("updated = true"));

  Ok(())
}

#[test]
fn test_node_sync_remote_to_mono() -> Result<()> {
  let workspace = NodeWorkspace::new_pnpm()?;

  // Create and split a package
  workspace.add_package("@test/my-package", "0.1.0", &[])?;
  workspace.commit("Add my-package")?;

  run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
  let split_dir = workspace.path.join("split-repos/my-package-split");
  let config = workspace.read_file("rail.toml")?;
  let updated_config = config.replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()));
  std::fs::write(workspace.path.join("rail.toml"), updated_config)?;

  run_cargo_rail(&workspace.path, &["rail", "split", "@test/my-package", "--apply"])?;

  // Make changes in split repo
  std::fs::write(
    split_dir.join("src/index.js"),
    "// Split repo change\nexport const fromSplit = true;",
  )?;
  git(&split_dir, &["add", "."])?;
  git(&split_dir, &["commit", "-m", "Update in split repo"])?;

  // Sync from remote
  run_cargo_rail(
    &workspace.path,
    &["rail", "sync", "@test/my-package", "--from-remote", "--apply"],
  )?;

  // Verify changes appear in monorepo
  let mono_index = workspace.read_file("packages/my-package/src/index.js")?;
  assert!(mono_index.contains("Split repo change"));
  assert!(mono_index.contains("fromSplit = true"));

  Ok(())
}
