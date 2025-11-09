use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::config::SecurityConfig;

/// Security validator for SSH keys and signing
pub struct SecurityValidator {
  config: SecurityConfig,
}

impl SecurityValidator {
  pub fn new(config: SecurityConfig) -> Self {
    Self { config }
  }

  /// Find and validate SSH key for git operations
  pub fn validate_ssh_key(&self) -> Result<PathBuf> {
    let ssh_key = if let Some(ref path) = self.config.ssh_key_path {
      // Use configured path
      path.clone()
    } else {
      // Auto-detect common SSH key locations
      self.find_default_ssh_key()?
    };

    // Check if key file exists
    if !ssh_key.exists() {
      anyhow::bail!(
        "SSH key not found: {}\n\
         \n\
         To fix this:\n\
         1. Generate an SSH key: ssh-keygen -t ed25519 -C \"your_email@example.com\"\n\
         2. Add the key to your SSH agent: ssh-add {}\n\
         3. Add the public key to your git remote (GitHub/GitLab/etc.)\n\
         \n\
         Or specify a custom key path in rail.toml:\n\
         [security]\n\
         ssh_key_path = \"/path/to/your/key\"",
        ssh_key.display(),
        ssh_key.display()
      );
    }

    // Check if key is readable
    if !ssh_key.metadata()?.permissions().readonly() && cfg!(unix) {
      // On Unix, check permissions (should be 600 or 400)
      use std::os::unix::fs::PermissionsExt;
      let perms = ssh_key.metadata()?.permissions();
      let mode = perms.mode();
      if mode & 0o077 != 0 {
        eprintln!(
          "⚠️  Warning: SSH key has insecure permissions: {:o}\n\
           → Recommended: chmod 600 {}",
          mode & 0o777,
          ssh_key.display()
        );
      }
    }

    println!("✅ SSH key validated: {}", ssh_key.display());

    Ok(ssh_key)
  }

  /// Find default SSH key in standard locations
  fn find_default_ssh_key(&self) -> Result<PathBuf> {
    let home = std::env::var("HOME")
      .or_else(|_| std::env::var("USERPROFILE"))
      .context(
        "Could not determine home directory. \
       Set $HOME or configure ssh_key_path in rail.toml",
      )?;

    let ssh_dir = PathBuf::from(&home).join(".ssh");

    // Check for keys in priority order
    let candidates = vec![
      ssh_dir.join("id_ed25519"),
      ssh_dir.join("id_rsa"),
      ssh_dir.join("id_ecdsa"),
    ];

    for key in candidates {
      if key.exists() {
        return Ok(key);
      }
    }

    anyhow::bail!(
      "No SSH key found in standard locations:\n\
       → ~/.ssh/id_ed25519\n\
       → ~/.ssh/id_rsa\n\
       → ~/.ssh/id_ecdsa\n\
       \n\
       Generate a key with: ssh-keygen -t ed25519 -C \"your_email@example.com\"\n\
       Or specify a custom path in rail.toml:\n\
       [security]\n\
       ssh_key_path = \"/path/to/your/key\""
    )
  }

  /// Validate signing key (if required)
  pub fn validate_signing_key(&self) -> Result<Option<PathBuf>> {
    if !self.config.require_signed_commits {
      return Ok(None);
    }

    let signing_key = if let Some(ref path) = self.config.signing_key_path {
      path.clone()
    } else {
      // Default to same as SSH key
      self.validate_ssh_key()?
    };

    if !signing_key.exists() {
      anyhow::bail!(
        "Signing key not found: {}\n\
         \n\
         Signing is required by your rail.toml configuration.\n\
         \n\
         To fix this:\n\
         1. Generate a signing key: ssh-keygen -t ed25519 -f ~/.ssh/id_signing\n\
         2. Configure git to use it: git config --global gpg.format ssh\n\
         3. git config --global user.signingkey {}\n\
         \n\
         Or disable signing in rail.toml:\n\
         [security]\n\
         require_signed_commits = false",
        signing_key.display(),
        signing_key.display()
      );
    }

    println!("✅ Signing key validated: {}", signing_key.display());

    Ok(Some(signing_key))
  }

  /// Check if current branch is protected
  #[allow(dead_code)]
  pub fn check_branch_protected(&self, branch_name: &str) -> Result<()> {
    if self.config.protected_branches.contains(&branch_name.to_string()) {
      anyhow::bail!(
        "Cannot commit directly to protected branch: {}\n\
         \n\
         Protected branches: {:?}\n\
         \n\
         For remote→mono syncs, cargo-rail will automatically create a PR branch.\n\
         Direct commits to protected branches are blocked to prevent accidental changes.",
        branch_name,
        self.config.protected_branches
      );
    }

    Ok(())
  }

  /// Generate PR branch name from pattern
  pub fn generate_pr_branch(&self, crate_name: &str) -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

    self
      .config
      .pr_branch_pattern
      .replace("{crate}", crate_name)
      .replace("{timestamp}", &timestamp.to_string())
  }

  /// Verify a commit is signed (if required)
  #[allow(dead_code)]
  pub fn verify_commit_signature(&self, repo_path: &Path, commit_sha: &str) -> Result<bool> {
    if !self.config.require_signed_commits {
      return Ok(true); // Not required, so pass
    }

    // Use git to verify signature
    let output = Command::new("git")
      .args(["verify-commit", commit_sha])
      .current_dir(repo_path)
      .output()
      .context("Failed to verify commit signature")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      eprintln!(
        "⚠️  Commit {} failed signature verification:\n{}",
        &commit_sha[..7],
        stderr
      );
      return Ok(false);
    }

    Ok(true)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_pr_branch_generation() {
    let config = SecurityConfig::default();
    let validator = SecurityValidator::new(config);

    let branch = validator.generate_pr_branch("test-crate");
    assert!(branch.starts_with("rail/sync/test-crate/"));
    assert!(branch.contains("202")); // Should have a year in timestamp
  }

  #[test]
  fn test_protected_branch_check() {
    let config = SecurityConfig::default();
    let validator = SecurityValidator::new(config);

    // Should fail for main
    assert!(validator.check_branch_protected("main").is_err());

    // Should fail for master
    assert!(validator.check_branch_protected("master").is_err());

    // Should pass for feature branch
    assert!(validator.check_branch_protected("feature/test").is_ok());
  }
}
