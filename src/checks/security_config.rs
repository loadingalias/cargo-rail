//! Security configuration validation checks

use super::trait_def::{Check, CheckContext, CheckResult};
use crate::core::config::RailConfig;
use crate::core::error::RailResult;
use std::process::Command;

/// Check that validates security configuration (branch protection, signing, etc.)
pub struct SecurityConfigCheck;

impl Check for SecurityConfigCheck {
  fn name(&self) -> &str {
    "security-config"
  }

  fn description(&self) -> &str {
    "Validates security configuration (branch protection, commit signing)"
  }

  fn run(&self, ctx: &CheckContext) -> RailResult<CheckResult> {
    // Detect actual config file location
    let config_path = RailConfig::find_config_path(&ctx.workspace_root);

    // Try to load configuration
    let config = match RailConfig::load(&ctx.workspace_root) {
      Ok(c) => c,
      Err(_) => {
        return Ok(CheckResult::warning(
          self.name(),
          "No rail.toml found - security settings not configured",
          Some("Run 'cargo rail init' to create configuration"),
        ));
      }
    };

    let mut warnings = Vec::new();
    let mut info = Vec::new();

    // Check branch protection
    if config.security.protected_branches.is_empty() {
      warnings.push("No protected branches configured - any branch can be directly committed to".to_string());
    } else {
      info.push(format!(
        "Protected branches: {}",
        config.security.protected_branches.join(", ")
      ));
    }

    // Check commit signing configuration
    if config.security.require_signed_commits {
      info.push("Commit signing: REQUIRED".to_string());

      // Check if git is configured for signing
      if ctx.thorough
        && let Ok(signing_configured) = check_git_signing_configured()
      {
        if !signing_configured {
          warnings.push(
            "Commit signing required but git not configured for signing. \
             Run: git config --global commit.gpgsign true"
              .to_string(),
          );
        } else {
          info.push("Git signing configured: YES".to_string());
        }
      }

      // Check signing key path if specified
      if let Some(ref key_path) = config.security.signing_key_path {
        if !key_path.exists() {
          warnings.push(format!(
            "Signing key path specified but not found: {}",
            key_path.display()
          ));
        } else {
          info.push(format!("Signing key: {}", key_path.display()));
        }
      }
    } else {
      warnings.push("Commit signing: DISABLED - commits will not be verified".to_string());
    }

    // Check SSH key path if specified
    if let Some(ref ssh_key_path) = config.security.ssh_key_path {
      if !ssh_key_path.exists() {
        warnings.push(format!(
          "SSH key path specified but not found: {}",
          ssh_key_path.display()
        ));
      } else {
        info.push(format!("SSH key: {}", ssh_key_path.display()));
      }
    }

    // Build result message
    let message = if info.is_empty() && warnings.is_empty() {
      "No security configuration issues found".to_string()
    } else {
      let mut msg = String::new();
      if !info.is_empty() {
        msg.push_str(&info.join("\n"));
      }
      if !warnings.is_empty() {
        if !msg.is_empty() {
          msg.push_str("\n\nWarnings:\n");
        }
        msg.push_str(&warnings.join("\n"));
      }
      msg
    };

    if warnings.is_empty() {
      Ok(CheckResult::pass(self.name(), message))
    } else {
      // Use actual config path in suggestion
      let suggestion = if let Some(path) = config_path {
        let relative_path = path.strip_prefix(&ctx.workspace_root).unwrap_or(&path);
        format!("Review security settings in {}", relative_path.display())
      } else {
        "Review security settings in rail.toml".to_string()
      };

      Ok(CheckResult::warning(self.name(), message, Some(&suggestion)))
    }
  }

  fn is_expensive(&self) -> bool {
    false
  }

  fn requires_crate(&self) -> bool {
    false
  }
}

/// Check if git is configured for commit signing
fn check_git_signing_configured() -> RailResult<bool> {
  let output = Command::new("git")
    .args(["config", "--get", "commit.gpgsign"])
    .output()?;

  if output.status.success() {
    let value = String::from_utf8_lossy(&output.stdout);
    Ok(value.trim() == "true")
  } else {
    Ok(false)
  }
}
