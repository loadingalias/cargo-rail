//! Test helpers for integration tests

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// A test workspace with git history
pub struct TestWorkspace {
  _root: TempDir,
  pub path: PathBuf,
}

impl TestWorkspace {
  /// Create a new test workspace with basic structure
  pub fn new() -> Result<Self> {
    Self::new_named("test-workspace")
  }

  /// Create a new test workspace with specific name
  pub fn new_named(name: &str) -> Result<Self> {
    let root = TempDir::new_in(std::env::temp_dir())
      .with_context(|| format!("Failed to create temp dir for test workspace '{}'", name))?;
    let path = root.path().to_path_buf();

    // Initialize git repo with main as default branch
    git(&path, &["init", "--initial-branch=main"])?;
    git(&path, &["config", "user.name", "Test User"])?;
    git(&path, &["config", "user.email", "test@example.com"])?;

    // Create workspace Cargo.toml
    std::fs::write(
      path.join("Cargo.toml"),
      r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
authors = ["Test Author"]

[workspace.dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
"#,
    )?;

    // Create .config/rail.toml to disable sync_on_unify for tests
    std::fs::create_dir_all(path.join(".config"))?;
    std::fs::write(
      path.join(".config/rail.toml"),
      r#"[workspace]
root = "."

[toolchain]
channel = "stable"

[unify]
sync_on_unify = false
use_all_features = true
"#,
    )?;

    git(&path, &["add", "."])?;
    git(&path, &["commit", "-m", "Initial workspace setup"])?;

    Ok(Self { _root: root, path })
  }

  /// Add a crate to the workspace
  pub fn add_crate(&self, name: &str, version: &str, deps: &[(&str, &str)]) -> Result<PathBuf> {
    let crate_path = self.path.join("crates").join(name);
    std::fs::create_dir_all(&crate_path)?;
    std::fs::create_dir_all(crate_path.join("src"))?;

    // Create Cargo.toml
    let mut cargo_toml = format!(
      r#"[package]
name = "{}"
version = "{}"
edition.workspace = true
license.workspace = true
authors.workspace = true

[dependencies]
"#,
      name, version
    );

    for (dep_name, dep_spec) in deps {
      cargo_toml.push_str(&format!("{} = {}\n", dep_name, dep_spec));
    }

    std::fs::write(crate_path.join("Cargo.toml"), cargo_toml)?;

    // Create basic lib.rs
    std::fs::write(
      crate_path.join("src/lib.rs"),
      format!(
        r#"//! {} crate

pub fn hello() -> &'static str {{
    "Hello from {}"
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn test_hello() {{
        assert_eq!(hello(), "Hello from {}");
    }}
}}
"#,
        name, name, name
      ),
    )?;

    // Create README
    std::fs::write(crate_path.join("README.md"), format!("# {}\n\nA test crate.\n", name))?;

    Ok(crate_path)
  }

  /// Commit current changes
  pub fn commit(&self, message: &str) -> Result<String> {
    git(&self.path, &["add", "."])?;
    git(&self.path, &["commit", "-m", message])?;

    // Get the commit SHA
    let output = git(&self.path, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Modify a file in a crate
  pub fn modify_file(&self, crate_name: &str, file: &str, content: &str) -> Result<()> {
    let file_path = self.path.join("crates").join(crate_name).join(file);
    std::fs::write(file_path, content)?;
    Ok(())
  }

  /// Get git log
  #[cfg(test)]
  #[allow(dead_code)]
  pub fn git_log(&self, n: usize) -> Result<Vec<String>> {
    let output = git(&self.path, &["log", &format!("-{}", n), "--oneline"])?;
    Ok(
      String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .collect(),
    )
  }

  /// Check if a file exists
  #[cfg(test)]
  #[allow(dead_code)]
  pub fn file_exists(&self, path: &str) -> bool {
    self.path.join(path).exists()
  }

  /// Read a file
  #[cfg(test)]
  #[allow(dead_code)]
  pub fn read_file(&self, path: &str) -> Result<String> {
    Ok(std::fs::read_to_string(self.path.join(path))?)
  }

  /// Remove the rail.toml config file (useful for init tests)
  pub fn remove_config(&self) -> Result<()> {
    let config_path = self.path.join(".config/rail.toml");
    if config_path.exists() {
      std::fs::remove_file(config_path)?;
    }
    Ok(())
  }
}

/// Run git command in a directory
pub fn git(cwd: &Path, args: &[&str]) -> Result<Output> {
  let output = Command::new("git")
    .current_dir(cwd)
    .args(args)
    .output()
    .context("Failed to run git command")?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("Git command failed: git {}\n{}", args.join(" "), stderr);
  }

  Ok(output)
}

/// Run cargo-rail CLI command
pub fn run_cargo_rail(cwd: &Path, args: &[&str]) -> Result<Output> {
  let cargo_rail_bin = env!("CARGO_BIN_EXE_cargo-rail");

  let output = Command::new(cargo_rail_bin)
    .current_dir(cwd)
    .args(args)
    .output()
    .context("Failed to run cargo-rail")?;

  Ok(output)
}

/// Load RailConfig from a workspace
pub fn load_rail_config(workspace_root: &Path) -> Result<cargo_rail::config::RailConfig> {
  cargo_rail::config::RailConfig::load(workspace_root).context("Failed to load rail.toml configuration")
}
