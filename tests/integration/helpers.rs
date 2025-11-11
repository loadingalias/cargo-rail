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
    let root = TempDir::new()?;
    let path = root.path().to_path_buf();

    // Initialize git repo
    git(&path, &["init"])?;
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
  pub fn file_exists(&self, path: &str) -> bool {
    self.path.join(path).exists()
  }

  /// Read a file
  pub fn read_file(&self, path: &str) -> Result<String> {
    Ok(std::fs::read_to_string(self.path.join(path))?)
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

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::bail!(
      "cargo-rail command failed: cargo rail {}\nstdout: {}\nstderr: {}",
      args.join(" "),
      stdout,
      stderr
    );
  }

  Ok(output)
}

/// A Node.js test workspace
pub struct NodeWorkspace {
  _root: TempDir,
  pub path: PathBuf,
}

impl NodeWorkspace {
  /// Create a new Node.js pnpm workspace with git
  pub fn new_pnpm() -> Result<Self> {
    let root = TempDir::new()?;
    let path = root.path().to_path_buf();

    // Initialize git repo
    git(&path, &["init"])?;
    git(&path, &["config", "user.name", "Test User"])?;
    git(&path, &["config", "user.email", "test@example.com"])?;

    // Create root package.json
    let package_json = serde_json::json!({
      "name": "test-workspace",
      "version": "1.0.0",
      "private": true,
      "description": "Test Node workspace"
    });
    std::fs::write(path.join("package.json"), serde_json::to_string_pretty(&package_json)?)?;

    // Create pnpm-workspace.yaml
    std::fs::write(path.join("pnpm-workspace.yaml"), "packages:\n  - 'packages/*'\n")?;

    // Create pnpm-lock.yaml
    std::fs::write(path.join("pnpm-lock.yaml"), "lockfileVersion: '6.0'\n")?;

    // Create packages directory
    std::fs::create_dir_all(path.join("packages"))?;

    git(&path, &["add", "."])?;
    git(&path, &["commit", "-m", "Initial workspace setup"])?;

    Ok(Self { _root: root, path })
  }

  /// Create a new Node.js npm workspace with git
  pub fn new_npm() -> Result<Self> {
    let root = TempDir::new()?;
    let path = root.path().to_path_buf();

    // Initialize git repo
    git(&path, &["init"])?;
    git(&path, &["config", "user.name", "Test User"])?;
    git(&path, &["config", "user.email", "test@example.com"])?;

    // Create root package.json with workspaces
    let package_json = serde_json::json!({
      "name": "test-workspace",
      "version": "1.0.0",
      "private": true,
      "workspaces": ["packages/*"]
    });
    std::fs::write(path.join("package.json"), serde_json::to_string_pretty(&package_json)?)?;

    // Create package-lock.json
    let package_lock = serde_json::json!({
      "name": "test-workspace",
      "version": "1.0.0",
      "lockfileVersion": 3,
      "requires": true,
      "packages": {}
    });
    std::fs::write(
      path.join("package-lock.json"),
      serde_json::to_string_pretty(&package_lock)?,
    )?;

    // Create packages directory
    std::fs::create_dir_all(path.join("packages"))?;

    git(&path, &["add", "."])?;
    git(&path, &["commit", "-m", "Initial workspace setup"])?;

    Ok(Self { _root: root, path })
  }

  /// Add a package to the workspace
  pub fn add_package(&self, name: &str, version: &str, deps: &[(&str, &str)]) -> Result<PathBuf> {
    // For scoped packages like "@test/pkg-a", use just "pkg-a" as the directory name
    let dir_name = if name.contains('/') {
      name.split('/').next_back().unwrap()
    } else {
      name
    };

    let pkg_path = self.path.join("packages").join(dir_name);
    std::fs::create_dir_all(&pkg_path)?;
    std::fs::create_dir_all(pkg_path.join("src"))?;

    // Create package.json
    let mut package_json = serde_json::json!({
      "name": name,
      "version": version,
      "description": format!("{} package", name),
      "main": "src/index.js",
      "dependencies": {}
    });

    // Add dependencies
    if !deps.is_empty() {
      let mut deps_obj = serde_json::Map::new();
      for (dep_name, dep_version) in deps {
        deps_obj.insert(
          (*dep_name).to_string(),
          serde_json::Value::String((*dep_version).to_string()),
        );
      }
      package_json["dependencies"] = serde_json::Value::Object(deps_obj);
    }

    std::fs::write(
      pkg_path.join("package.json"),
      serde_json::to_string_pretty(&package_json)?,
    )?;

    // Create basic index.js
    std::fs::write(
      pkg_path.join("src/index.js"),
      format!("// {} package\nexport const hello = 'Hello from {}';\n", name, name),
    )?;

    // Create README
    std::fs::write(pkg_path.join("README.md"), format!("# {}\n\nA test package.\n", name))?;

    Ok(pkg_path)
  }

  /// Commit current changes
  pub fn commit(&self, message: &str) -> Result<String> {
    git(&self.path, &["add", "."])?;
    git(&self.path, &["commit", "-m", message])?;

    // Get the commit SHA
    let output = git(&self.path, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  /// Modify a file in a package
  pub fn modify_file(&self, package_name: &str, file: &str, content: &str) -> Result<()> {
    // Handle scoped package names (extract the last part after /)
    let dir_name = if package_name.contains('/') {
      package_name.split('/').next_back().unwrap()
    } else {
      package_name
    };

    let file_path = self.path.join("packages").join(dir_name).join(file);
    std::fs::write(file_path, content)?;
    Ok(())
  }

  /// Read a file
  pub fn read_file(&self, path: &str) -> Result<String> {
    Ok(std::fs::read_to_string(self.path.join(path))?)
  }

  /// Get git log
  pub fn git_log(&self, n: usize) -> Result<Vec<String>> {
    let output = git(&self.path, &["log", &format!("-{}", n), "--oneline"])?;
    Ok(
      String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .collect(),
    )
  }
}
