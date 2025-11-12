//! SSH key validation checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::error::RailResult;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

/// Check that validates SSH keys and connectivity
pub struct SshKeyCheck;

impl Check for SshKeyCheck {
  fn name(&self) -> &str {
    "ssh-keys"
  }

  fn description(&self) -> &str {
    "Validates SSH key existence and permissions"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    let home = match std::env::var("HOME") {
      Ok(h) => PathBuf::from(h),
      Err(_) => {
        return Ok(CheckResult::error(
          self.name(),
          "HOME environment variable not set",
          Some("Ensure your shell environment is configured correctly"),
        ));
      }
    };

    let ssh_dir = home.join(".ssh");

    // Check if .ssh directory exists
    if !ssh_dir.exists() {
      return Ok(CheckResult::error(
        self.name(),
        format!("SSH directory not found: {}", ssh_dir.display()),
        Some("Create SSH keys with: ssh-keygen -t ed25519 -C \"your_email@example.com\""),
      ));
    }

    // Check common SSH key locations
    let key_types = vec![
      ("id_ed25519", "Ed25519 (recommended)"),
      ("id_rsa", "RSA"),
      ("id_ecdsa", "ECDSA"),
    ];

    let mut found_keys = Vec::new();
    let mut permission_issues: Vec<String> = Vec::new();

    for (key_name, key_desc) in key_types {
      let key_path = ssh_dir.join(key_name);
      if key_path.exists() {
        found_keys.push(format!("{} ({})", key_name, key_desc));

        // Check permissions (should be 600 or 400) - Unix only
        #[cfg(unix)]
        {
          if let Ok(metadata) = fs::metadata(&key_path) {
            let permissions = metadata.permissions();
            let mode = permissions.mode() & 0o777;
            if mode != 0o600 && mode != 0o400 {
              permission_issues.push(format!("{}: has mode {:o} (should be 600 or 400)", key_name, mode));
            }
          }
        }
      }
    }

    if found_keys.is_empty() {
      return Ok(CheckResult::error(
        self.name(),
        "No SSH keys found",
        Some("Create SSH keys with: ssh-keygen -t ed25519 -C \"your_email@example.com\""),
      ));
    }

    if !permission_issues.is_empty() {
      return Ok(CheckResult::warning(
        self.name(),
        format!("SSH key permission issues:\n{}", permission_issues.join("\n")),
        Some("Fix with: chmod 600 ~/.ssh/id_*"),
      ));
    }

    // If thorough mode, test SSH connectivity to github.com
    if ctx.thorough {
      match test_ssh_connectivity("github.com") {
        Ok(true) => Ok(CheckResult::pass(
          self.name(),
          format!(
            "SSH keys valid (found: {}), GitHub connectivity OK",
            found_keys.join(", ")
          ),
        )),
        Ok(false) => Ok(CheckResult::warning(
          self.name(),
          format!(
            "SSH keys found ({}), but cannot connect to GitHub",
            found_keys.join(", ")
          ),
          Some("Ensure your SSH key is added to GitHub: https://github.com/settings/keys"),
        )),
        Err(err) => Ok(CheckResult::warning(
          self.name(),
          format!(
            "SSH keys found ({}), but connectivity test failed: {}",
            found_keys.join(", "),
            err
          ),
          None::<String>,
        )),
      }
    } else {
      Ok(CheckResult::pass(
        self.name(),
        format!("SSH keys found: {}", found_keys.join(", ")),
      ))
    }
  }

  fn is_expensive(&self) -> bool {
    false // Basic check is fast, thorough mode tests connectivity
  }

  fn requires_crate(&self) -> bool {
    false
  }
}

/// Test SSH connectivity to a host
fn test_ssh_connectivity(host: &str) -> RailResult<bool> {
  let output = Command::new("ssh")
    .arg("-T")
    .arg("-o")
    .arg("StrictHostKeyChecking=no")
    .arg("-o")
    .arg("ConnectTimeout=5")
    .arg(format!("git@{}", host))
    .output()?;

  // GitHub returns exit code 1 with "successfully authenticated" message
  // This is expected behavior
  if output.status.code() == Some(1) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(stderr.contains("successfully authenticated"))
  } else {
    Ok(output.status.success())
  }
}
