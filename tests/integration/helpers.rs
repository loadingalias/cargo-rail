//! Test helpers for integration tests

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const NEXTEST_CONFIG: &str = r#"[profile.default]
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
"#;

/// Finish a fallible integration-test body without discarding its error chain.
#[track_caller]
pub fn finish_test(result: Result<()>) {
    assert_eq!(result.map_err(|error| format!("{error:#}")), Ok(()));
}

fn isolated_git_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/isolated.gitconfig")
}

/// Build a cargo-rail command isolated from the developer's Cargo and Git configuration.
pub fn cargo_rail_command(cwd: &Path) -> Result<Command> {
    let git_config = isolated_git_config();
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
    command
        .current_dir(cwd)
        .env("CARGO_RAIL_CACHE_DIR", cwd.join("target/cargo-rail-test-cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", git_config)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
        .env("GIT_CONFIG_VALUE_1", "false");
    Ok(command)
}

/// Isolate cache-management commands from a real cargo-rail installation receipt.
pub fn isolated_cargo_rail_command(cwd: &Path) -> Result<Command> {
    let cargo_home = cwd.join("target/cargo-rail-test-cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
    let mut command = cargo_rail_command(cwd)?;
    command.env("CARGO_HOME", cargo_home);
    Ok(command)
}

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
        std::fs::write(path.join(".gitignore"), "target/\n")?;

        // Create .config/rail.toml for tests
        std::fs::create_dir_all(path.join(".config"))?;
        std::fs::write(path.join(".config/rail.toml"), "")?;

        // Create .config/nextest.toml with commit profile for CI compatibility.
        std::fs::write(path.join(".config/nextest.toml"), NEXTEST_CONFIG)?;

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
        std::fs::write(path.join(".gitignore"), "target/\n")?;

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
        std::fs::write(path.join(".config/rail.toml"), "")?;
        std::fs::write(path.join(".config/nextest.toml"), NEXTEST_CONFIG)?;

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
        let remotes = git(&self.path, &["remote"])?;
        if String::from_utf8_lossy(&remotes.stdout)
            .lines()
            .any(|remote| remote == "origin")
        {
            git(&self.path, &["remote", "remove", "origin"])?;
        }
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
            String::new()
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

/// Create a workspace nested inside a git repo (git root != workspace root)
///
/// Structure:
/// ```text
/// git_root/
/// ├── .git/
/// ├── docs/
/// │   └── README.md
/// └── rust/                  <- Cargo workspace root
///     ├── Cargo.toml
///     ├── .config/rail.toml
///     └── crates/
///         └── example/
/// ```
pub struct NestedWorkspace {
    _root: TempDir,
    /// Git repository root
    pub git_root: PathBuf,
    /// Cargo workspace root (subdirectory of git_root)
    pub workspace_root: PathBuf,
}

impl NestedWorkspace {
    /// Create a nested workspace where cargo workspace is in a subdirectory
    pub fn new(subdir: &str) -> Result<Self> {
        let root = TempDir::new_in(std::env::temp_dir()).context("Failed to create temp dir")?;
        let git_root = root.path().to_path_buf();
        let workspace_root = git_root.join(subdir);

        // Initialize git repo at root level
        git(&git_root, &["init", "--initial-branch=main"])?;
        git(&git_root, &["config", "user.name", "Test User"])?;
        git(&git_root, &["config", "user.email", "test@example.com"])?;

        // Create some non-workspace files at git root
        std::fs::create_dir_all(git_root.join("docs"))?;
        std::fs::write(git_root.join("docs/README.md"), "# Project Docs\n")?;
        std::fs::write(git_root.join("README.md"), "# Root README\n")?;
        std::fs::write(git_root.join(".gitignore"), "target/\n")?;

        // Create workspace in subdirectory
        std::fs::create_dir_all(&workspace_root)?;
        std::fs::create_dir_all(workspace_root.join("crates"))?;

        // Create workspace Cargo.toml
        std::fs::write(
            workspace_root.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
edition = "2021"
"#,
        )?;

        // Create .config/rail.toml
        std::fs::create_dir_all(workspace_root.join(".config"))?;
        std::fs::write(workspace_root.join(".config/rail.toml"), "")?;

        // Initial commit from git root
        git(&git_root, &["add", "."])?;
        git(&git_root, &["commit", "-m", "Initial nested workspace setup"])?;

        Ok(Self {
            _root: root,
            git_root,
            workspace_root,
        })
    }

    /// Add a crate to the nested workspace
    pub fn add_crate(&self, name: &str, version: &str) -> Result<PathBuf> {
        let crate_path = self.workspace_root.join("crates").join(name);
        std::fs::create_dir_all(&crate_path)?;
        std::fs::create_dir_all(crate_path.join("src"))?;

        std::fs::write(
            crate_path.join("Cargo.toml"),
            format!(
                r#"[package]
name = "{}"
version = "{}"
edition.workspace = true
"#,
                name, version
            ),
        )?;

        std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}\n")?;

        Ok(crate_path)
    }

    /// Commit from git root
    pub fn commit(&self, message: &str) -> Result<String> {
        git(&self.git_root, &["add", "."])?;
        git(&self.git_root, &["commit", "-m", message])?;
        let output = git(&self.git_root, &["rev-parse", "HEAD"])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Modify a file in a crate
    pub fn modify_file(&self, crate_name: &str, file: &str, content: &str) -> Result<()> {
        let file_path = self.workspace_root.join("crates").join(crate_name).join(file);
        std::fs::write(file_path, content)?;
        Ok(())
    }
}

/// Run git command in a directory
pub fn git(cwd: &Path, args: &[&str]) -> Result<Output> {
    let git_config = isolated_git_config();
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", git_config)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
        .env("GIT_CONFIG_VALUE_1", "false")
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
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
    run_cargo_rail_with_env(cwd, args, &[])
}

/// Run cargo-rail with explicit environment overrides.
pub fn run_cargo_rail_with_env(cwd: &Path, args: &[&str], environment: &[(&str, &str)]) -> Result<Output> {
    let mut command = if matches!(args.get(1), Some(&"cache" | &"clean")) {
        isolated_cargo_rail_command(cwd)?
    } else {
        cargo_rail_command(cwd)?
    };
    command.args(args);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().context("Failed to run cargo-rail")?;

    Ok(output)
}

/// Reconstruct compiler-evidence entries from the configured local CAS.
pub fn compiler_evidence_cache(workspace_root: &Path) -> Result<serde_json::Value> {
    compiler_evidence_cache_at(&workspace_root.join("target/cargo-rail-test-cache"))
}

/// Reconstruct compiler-evidence entries from an explicitly configured cache base.
pub fn compiler_evidence_cache_at(cache_base: &Path) -> Result<serde_json::Value> {
    let root = cache_base.join("cargo-rail/local-cas-v2");
    let mut pin_created = std::collections::HashMap::new();
    for pin in std::fs::read_dir(root.join("pins"))? {
        let pin: serde_json::Value = serde_json::from_slice(&std::fs::read(pin?.path())?)?;
        if let (Some(action_key), Ok(created)) = (
            pin["action_key"].as_str(),
            pin["created_unix_nanos"].to_string().parse::<u128>(),
        ) {
            pin_created.insert(action_key.to_string(), created);
        }
    }
    let mut result_directories = std::fs::read_dir(root.join("results"))?.collect::<Result<Vec<_>, _>>()?;
    result_directories.sort_by_key(|entry| entry.file_name());
    let mut latest = std::collections::BTreeMap::<String, (u128, serde_json::Value)>::new();
    for result in result_directories {
        let validation_directory = result.path().join("validations");
        let evidence_directory = result.path().join("evidence");
        if !validation_directory.is_dir() || !evidence_directory.is_dir() {
            continue;
        }
        let validation_path = std::fs::read_dir(&validation_directory)?
            .next()
            .transpose()?
            .context("compiler-evidence validation object")?
            .path();
        let evidence_path = std::fs::read_dir(&evidence_directory)?
            .next()
            .transpose()?
            .context("compiler-evidence payload object")?
            .path();
        let validation: serde_json::Value = serde_json::from_slice(&std::fs::read(validation_path)?)?;
        let evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(evidence_path)?)?;
        let Some(action_key) = validation["action_key"].as_str() else {
            continue;
        };
        let key = serde_json::to_string(&validation["key"])?;
        let created = pin_created.get(action_key).copied().unwrap_or_default();
        let value = serde_json::json!({
          "key": validation["key"],
          "evidence": evidence["evidence"],
          "collector_version": validation["collector_version"],
          "observations": validation["observations"],
          "created_unix_nanos": created,
        });
        if latest.get(&key).is_none_or(|(current, _)| *current < created) {
            latest.insert(key, (created, value));
        }
    }
    let entries = latest
        .into_iter()
        .map(|(key, (_, value))| (key, value))
        .collect::<serde_json::Map<_, _>>();
    Ok(serde_json::json!({ "entries": entries }))
}

/// Load RailConfig from a workspace
pub fn load_rail_config(workspace_root: &Path) -> Result<cargo_rail::config::RailConfig> {
    cargo_rail::config::RailConfig::load(workspace_root).context("Failed to load rail.toml configuration")
}
