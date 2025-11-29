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
use_all_features = true
"#,
    )?;

    // Create .config/nextest.toml with commit profile for CI compatibility
    std::fs::write(
      path.join(".config/nextest.toml"),
      r#"[profile.default]
status-level = "pass"
success-output = "never"
failure-output = "immediate"
fail-fast = false

[profile.commit]
status-level = "fail"
success-output = "never"
failure-output = "immediate-final"
fail-fast = false
retries = { backoff = "exponential", count = 2, delay = "1s", jitter = true }
"#,
    )?;

    git(&path, &["add", "."])?;
    git(&path, &["commit", "-m", "Initial workspace setup"])?;

    Ok(Self { _root: root, path })
  }

  /// Create a single-crate repo (non-workspace, like a split repo)
  ///
  /// This creates a standalone crate without a [workspace] section,
  /// simulating what a split repository looks like.
  pub fn new_single_crate(crate_name: &str, version: &str) -> Result<Self> {
    let root = TempDir::new_in(std::env::temp_dir())
      .with_context(|| format!("Failed to create temp dir for single crate '{}'", crate_name))?;
    let path = root.path().to_path_buf();

    // Initialize git repo with main as default branch
    git(&path, &["init", "--initial-branch=main"])?;
    git(&path, &["config", "user.name", "Test User"])?;
    git(&path, &["config", "user.email", "test@example.com"])?;

    // Create package Cargo.toml (NO [workspace] section)
    std::fs::write(
      path.join("Cargo.toml"),
      format!(
        r#"[package]
name = "{}"
version = "{}"
edition = "2021"
license = "MIT"
authors = ["Test Author"]

[dependencies]
"#,
        crate_name, version
      ),
    )?;

    // Create src/lib.rs
    std::fs::create_dir_all(path.join("src"))?;
    std::fs::write(
      path.join("src/lib.rs"),
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
        crate_name, crate_name, crate_name
      ),
    )?;

    // Create README
    std::fs::write(path.join("README.md"), format!("# {}\n\nA test crate.\n", crate_name))?;

    // Create .config/rail.toml
    std::fs::create_dir_all(path.join(".config"))?;
    std::fs::write(
      path.join(".config/rail.toml"),
      r#"[workspace]
root = "."

[toolchain]
channel = "stable"
"#,
    )?;

    git(&path, &["add", "."])?;
    git(&path, &["commit", "-m", "Initial single-crate setup"])?;

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

  /// Set origin remote (useful for link generation)
  pub fn set_remote(&self, url: &str) -> Result<()> {
    git(&self.path, &["remote", "remove", "origin"]).ok();
    git(&self.path, &["remote", "add", "origin", url])?;
    Ok(())
  }

  /// Create an annotated tag
  pub fn tag(&self, name: &str, message: &str) -> Result<()> {
    git(&self.path, &["tag", "-a", name, "-m", message])?;
    Ok(())
  }

  /// Overwrite or create the release config block in .config/rail.toml
  pub fn write_release_config(&self, content: &str) -> Result<()> {
    let config_path = self.path.join(".config/rail.toml");

    // Create directory if it doesn't exist
    if let Some(parent) = config_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    // Read existing config or create default
    let mut existing = if config_path.exists() {
      std::fs::read_to_string(&config_path)?
    } else {
      r#"[workspace]
root = "."

[toolchain]
channel = "stable"
"#
      .to_string()
    };

    if let Some(idx) = existing.find("[release]") {
      existing.truncate(idx);
    }

    existing.push_str("\n[release]\n");
    existing.push_str(content);

    std::fs::write(&config_path, existing)?;
    Ok(())
  }

  /// Modify a file in a crate
  pub fn modify_file(&self, crate_name: &str, file: &str, content: &str) -> Result<()> {
    let file_path = self.path.join("crates").join(crate_name).join(file);
    std::fs::write(file_path, content)?;
    Ok(())
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
