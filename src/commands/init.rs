//! `cargo rail init` - Initialize cargo-rail configuration
//!
//! Auto-detects workspace structure, toolchain settings, and generates
//! a sensible .config/rail.toml with smart defaults.

use crate::config::{PolicyConfig, RailConfig, SecurityConfig, ToolchainConfig, UnifyConfig, WorkspaceConfig};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Run the init command to bootstrap rail.toml configuration
///
/// # Arguments
/// * `ctx` - Workspace context (already loaded, contains cargo metadata)
/// * `output_path` - Where to write rail.toml (relative to workspace root)
/// * `force` - Overwrite existing config file
/// * `non_interactive` - Skip all prompts, use defaults
/// * `dry_run` - Print config to stdout instead of writing
pub fn run_init(
  ctx: &WorkspaceContext,
  output_path: &str,
  force: bool,
  non_interactive: bool,
  dry_run: bool,
) -> RailResult<()> {
  let workspace_root = ctx.workspace_root();
  let config_path = workspace_root.join(output_path);

  // 1. Check for existing config
  if let Some(existing) = check_existing_config(workspace_root) {
    if !force {
      return Err(RailError::with_help(
        format!(
          "Configuration already exists at: {}\nUse --force to overwrite or --output to specify a different location",
          existing.display()
        ),
        "Example: cargo rail init --force",
      ));
    }
    if !dry_run {
      println!("⚠️  Overwriting existing config at: {}", existing.display());
    }
  }

  // 2. Detection phase
  println!("🔍 Detecting workspace configuration...\n");

  let workspace_patterns = detect_workspace_patterns(ctx);
  let toolchain = detect_toolchain_config(workspace_root)?;
  let policy = detect_policy_config(workspace_root)?;
  let unify = default_unify_config();
  let security = default_security_config();

  // 3. Display summary
  println!("Workspace Analysis:");
  println!("  Root: {}", workspace_root.display());
  println!("{}", workspace_patterns.format_summary());

  if toolchain.path.is_some() || toolchain.channel != "stable" {
    println!("\nToolchain Detection:");
    if let Some(ref path) = toolchain.path {
      println!("  Path: {}", path);
    } else {
      println!("  Channel: {}", toolchain.channel);
    }
    println!("  Profile: {}", toolchain.profile);
    if !toolchain.components.is_empty() {
      println!("  Components: {}", toolchain.components.join(", "));
    }
    if !toolchain.targets.is_empty() {
      println!("  Targets: {}", toolchain.targets.join(", "));
    }
  }

  if policy.edition.is_some() || policy.resolver.is_some() || policy.msrv.is_some() {
    println!("\nPolicy Detection:");
    if let Some(ref edition) = policy.edition {
      println!("  Edition: {} (from workspace Cargo.toml)", edition);
    }
    if let Some(ref resolver) = policy.resolver {
      println!("  Resolver: {} (from workspace Cargo.toml)", resolver);
    }
    if let Some(ref msrv) = policy.msrv {
      println!("  MSRV: {} (from rust-version in Cargo.toml)", msrv);
    }
  }

  // 4. Interactive confirmation (unless non-interactive or dry-run)
  if !non_interactive && !dry_run {
    print!("\nGenerate rail.toml with these settings? [Y/n]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "n" || input == "no" {
      println!("Cancelled.");
      return Ok(());
    }
  }

  // 5. Build config
  println!("\n✅ Generated configuration with smart defaults\n");

  let config = build_rail_config(workspace_root.to_path_buf(), toolchain, policy, unify, security);

  // 6. Serialize with comments
  let toml_content = serialize_config_with_comments(&config)?;

  // 7. Write or print
  if dry_run {
    println!("--- .config/rail.toml ---");
    println!("{}", toml_content);
    println!("--- End of config ---");
  } else {
    println!("📝 Writing to {}...", config_path.display());
    ensure_output_dir(&config_path)?;
    write_config_file(&config_path, &toml_content)?;

    println!("\n✅ Configuration initialized successfully!\n");
    println!("Next steps:");
    println!("  1. Review {} and adjust settings", output_path);
    println!("  2. Run 'cargo rail config sync' to sync rust-toolchain.toml");
    println!("  3. Run 'cargo rail unify analyze' to check dependency unification");
  }

  Ok(())
}

/// Information about detected workspace organization patterns
#[derive(Debug)]
pub struct WorkspacePatternInfo {
  /// Total number of workspace members
  pub member_count: usize,

  /// Common subdirectory patterns detected
  pub subdirectories: Vec<String>,

  /// Whether workspace is single-crate (at root)
  pub is_single_crate: bool,

  /// Human-readable summary for display
  pub summary: String,
}

impl WorkspacePatternInfo {
  /// Format for display to user
  pub fn format_summary(&self) -> String {
    format!("  Members: {} crate(s)\n  Pattern: {}", self.member_count, self.summary)
  }
}

/// Detect workspace organization patterns
fn detect_workspace_patterns(ctx: &WorkspaceContext) -> WorkspacePatternInfo {
  let workspace_root = ctx.workspace_root();
  let members = ctx.cargo.metadata().list_crates();

  let member_count = members.len();
  let is_single_crate = member_count == 1;

  // Detect common subdirectory patterns
  let mut subdirs = std::collections::HashSet::new();
  for pkg in &members {
    if let Ok(rel_path) = pkg.manifest_path.strip_prefix(workspace_root)
      && let Some(first_component) = rel_path.components().next()
        && let Some(dir_name) = first_component.as_os_str().to_str()
          && dir_name != "Cargo.toml" && !dir_name.starts_with('.') {
            subdirs.insert(dir_name.to_string());
          }
  }

  let subdirectories: Vec<_> = subdirs.into_iter().collect();

  let summary = if is_single_crate {
    "Single crate at workspace root".to_string()
  } else if subdirectories.is_empty() {
    "Multiple crates at workspace root".to_string()
  } else if subdirectories.len() == 1 {
    format!("Organized in {}/ subdirectory", subdirectories[0])
  } else {
    format!("Organized in multiple subdirectories ({})", subdirectories.join(", "))
  };

  WorkspacePatternInfo {
    member_count,
    subdirectories,
    is_single_crate,
    summary,
  }
}

/// Detect toolchain configuration from existing rust-toolchain.toml
fn detect_toolchain_config(workspace_root: &Path) -> RailResult<ToolchainConfig> {
  let toolchain_path = workspace_root.join("rust-toolchain.toml");

  if !toolchain_path.exists() {
    // Try rust-toolchain without .toml extension
    let alt_path = workspace_root.join("rust-toolchain");
    if !alt_path.exists() {
      return Ok(ToolchainConfig::default());
    }

    // rust-toolchain (no extension) is just a channel string
    let content =
      fs::read_to_string(&alt_path).map_err(|e| RailError::message(format!("Failed to read rust-toolchain: {}", e)))?;
    let channel = content.trim().to_string();

    return Ok(ToolchainConfig {
      channel,
      ..ToolchainConfig::default()
    });
  }

  // Parse rust-toolchain.toml
  let content = fs::read_to_string(&toolchain_path)
    .map_err(|e| RailError::message(format!("Failed to read rust-toolchain.toml: {}", e)))?;

  #[derive(serde::Deserialize)]
  struct RustToolchainFile {
    toolchain: RustToolchainSection,
  }

  #[derive(serde::Deserialize)]
  struct RustToolchainSection {
    channel: Option<String>,
    path: Option<String>,
    profile: Option<String>,
    components: Option<Vec<String>>,
    targets: Option<Vec<String>>,
  }

  let parsed: RustToolchainFile = toml_edit::de::from_str(&content).map_err(|e| {
    RailError::with_help(
      format!("Failed to parse rust-toolchain.toml: {}", e),
      "Check the syntax of rust-toolchain.toml or remove it to use defaults",
    )
  })?;

  Ok(ToolchainConfig {
    channel: parsed.toolchain.channel.unwrap_or_else(|| "stable".to_string()),
    path: parsed.toolchain.path,
    profile: parsed.toolchain.profile.unwrap_or_else(|| "default".to_string()),
    components: parsed.toolchain.components.unwrap_or_default(),
    targets: parsed.toolchain.targets.unwrap_or_default(),
  })
}

/// Detect policy configuration from workspace Cargo.toml
fn detect_policy_config(workspace_root: &Path) -> RailResult<PolicyConfig> {
  let cargo_toml_path = workspace_root.join("Cargo.toml");

  let content = fs::read_to_string(&cargo_toml_path)
    .map_err(|e| RailError::message(format!("Failed to read Cargo.toml: {}", e)))?;

  let doc: toml_edit::DocumentMut = content
    .parse()
    .map_err(|e| RailError::message(format!("Failed to parse workspace Cargo.toml: {}", e)))?;

  let mut policy = PolicyConfig::default();

  // Extract workspace.package fields
  if let Some(workspace) = doc.get("workspace").and_then(|w| w.as_table()) {
    if let Some(package) = workspace.get("package").and_then(|p| p.as_table()) {
      if let Some(edition) = package.get("edition").and_then(|e| e.as_str()) {
        policy.edition = Some(edition.to_string());
      }
      if let Some(rust_version) = package.get("rust-version").and_then(|r| r.as_str()) {
        policy.msrv = Some(rust_version.to_string());
      }
    }
    if let Some(resolver) = workspace.get("resolver").and_then(|r| r.as_str()) {
      policy.resolver = Some(resolver.to_string());
    }
  }

  Ok(policy)
}

/// Auto-detect reasonable unify defaults
fn default_unify_config() -> UnifyConfig {
  UnifyConfig::default()
}

/// Auto-detect security defaults
fn default_security_config() -> SecurityConfig {
  SecurityConfig::default()
}

/// Build a complete RailConfig from detected/default values
fn build_rail_config(
  _workspace_root: PathBuf,
  toolchain: ToolchainConfig,
  policy: PolicyConfig,
  unify: UnifyConfig,
  security: SecurityConfig,
) -> RailConfig {
  RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    toolchain,
    policy,
    unify,
    security,
    splits: vec![],
  }
}

/// Serialize RailConfig to TOML string with helpful comments
///
/// Generates a comprehensive, self-documenting configuration file with:
/// - All available fields (active and commented)
/// - Explanatory comments for each section
/// - Smart grouping for better UX
/// - Detected values highlighted
fn serialize_config_with_comments(config: &RailConfig) -> RailResult<String> {
  let mut output = String::new();

  // Header
  output.push_str("# ═══════════════════════════════════════════════════════════════════════════\n");
  output.push_str("# cargo-rail configuration\n");
  output.push_str("# ═══════════════════════════════════════════════════════════════════════════\n");
  output.push_str("#\n");
  output.push_str("# Generated by: cargo rail init\n");
  output.push_str("# Documentation: https://github.com/loadingalias/cargo-rail\n");
  output.push_str("#\n");
  output.push_str("# This file controls cargo-rail's behavior for:\n");
  output.push_str("#  • Toolchain management (rust-toolchain.toml sync)\n");
  output.push_str("#  • Dependency unification (workspace-hack elimination)\n");
  output.push_str("#  • Workspace policy enforcement\n");
  output.push_str("#  • Monorepo↔split-repo synchronization\n");
  output.push_str("#\n");
  output.push_str("# ═══════════════════════════════════════════════════════════════════════════\n\n");

  // Workspace
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Workspace Root                                                          │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Relative path to workspace root (usually \".\")\n\n");
  output.push_str("[workspace]\n");
  output.push_str(&format!("root = \"{}\"\n\n", config.workspace.root.display()));

  // Toolchain
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Toolchain Configuration (Source of Truth for rust-toolchain.toml)      │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# This section drives 'cargo rail config sync' to generate/update\n");
  output.push_str("# rust-toolchain.toml automatically.\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   channel    - Rust release channel (stable, beta, nightly, or version)\n");
  output.push_str("#   path       - Path to custom toolchain (mutually exclusive with channel)\n");
  output.push_str("#   profile    - Toolchain profile (minimal, default, complete)\n");
  output.push_str("#   components - Additional components (clippy, rustfmt, rust-src, etc.)\n");
  output.push_str("#   targets    - Cross-compilation targets\n\n");

  output.push_str("[toolchain]\n");
  if let Some(ref path) = config.toolchain.path {
    output.push_str(&format!("path = \"{}\"  # Custom toolchain path\n", path));
    output.push_str("# channel = \"stable\"  # Not used when path is set\n");
  } else {
    output.push_str(&format!("channel = \"{}\"", config.toolchain.channel));
    if config.toolchain.channel == "stable" {
      output.push_str("  # stable, beta, nightly, or specific version (e.g., \"1.76.0\")\n");
    } else {
      output.push_str("  # Detected from rust-toolchain.toml\n");
    }
  }
  output.push_str(&format!(
    "profile = \"{}\"  # minimal, default, or complete\n",
    config.toolchain.profile
  ));

  if !config.toolchain.components.is_empty() {
    output.push_str("components = [");
    for (i, comp) in config.toolchain.components.iter().enumerate() {
      if i > 0 {
        output.push_str(", ");
      }
      output.push_str(&format!("\"{}\"", comp));
    }
    output.push_str("]  # Detected from rust-toolchain.toml\n");
  } else {
    output.push_str("components = []  # e.g., [\"clippy\", \"rustfmt\", \"rust-src\"]\n");
  }

  if !config.toolchain.targets.is_empty() {
    output.push_str("targets = [  # Detected from rust-toolchain.toml\n");
    for target in &config.toolchain.targets {
      output.push_str(&format!("  \"{}\",\n", target));
    }
    output.push_str("]\n");
  } else {
    output.push_str("targets = []  # e.g., [\"x86_64-unknown-linux-gnu\", \"aarch64-apple-darwin\"]\n");
  }

  output.push('\n');

  // Dependency Unification
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Dependency Unification (Workspace-Hack Elimination)                     │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Automatically unify workspace dependencies using native Cargo features.\n");
  output.push_str("# Run: cargo rail unify analyze  (to preview changes)\n");
  output.push_str("#      cargo rail unify apply    (to apply unification)\n");
  output.push_str("#      cargo rail unify check    (for CI validation)\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   use_all_features           - Use --all-features for accurate analysis\n");
  output.push_str("#   sync_on_unify              - Auto-sync rust-toolchain.toml before unify\n");
  output.push_str("#   validate_targets           - Per-target validation (catches platform issues)\n");
  output.push_str("#   max_parallel_jobs          - Parallelism (0 = auto-detect)\n");
  output.push_str("#   pin_transitives            - Pin transitive deps with fragmented features\n");
  output.push_str("#   pin_hosts                  - Crates to host transitive pins\n\n");

  output.push_str("[unify]\n");
  output.push_str(&format!(
    "use_all_features = {}  # Ensure complete feature union analysis\n",
    config.unify.use_all_features
  ));
  output.push_str(&format!(
    "sync_on_unify = {}  # Keep toolchain in sync\n",
    config.unify.sync_on_unify
  ));

  if !config.unify.validate_targets.is_empty() {
    output.push_str("validate_targets = [");
    for (i, target) in config.unify.validate_targets.iter().enumerate() {
      if i > 0 {
        output.push_str(", ");
      }
      output.push_str(&format!("\"{}\"", target));
    }
    output.push_str("]\n");
  } else {
    output.push_str("validate_targets = []  # Optional: [\"x86_64-unknown-linux-gnu\", \"wasm32-unknown-unknown\"]\n");
  }

  output.push_str(&format!(
    "max_parallel_jobs = {}  # 0 = auto-detect CPU count\n",
    config.unify.max_parallel_jobs
  ));
  output.push_str(&format!(
    "pin_transitives = {}  # Auto-pin transitive deps with feature fragmentation\n",
    config.unify.pin_transitives
  ));

  if !config.unify.pin_hosts.is_empty() {
    output.push_str("pin_hosts = [");
    for (i, host) in config.unify.pin_hosts.iter().enumerate() {
      if i > 0 {
        output.push_str(", ");
      }
      output.push_str(&format!("\"{}\"", host));
    }
    output.push_str("]\n");
  } else {
    output.push_str("pin_hosts = []  # Optional: [\"workspace-root\"] (auto-selects if empty)\n");
  }

  output.push('\n');

  // Policy & Linting - ALWAYS include section
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Workspace Policy & Linting                                              │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Enforce consistency and quality standards across the workspace.\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   resolver                      - Cargo resolver version (\"2\" or \"3\")\n");
  output.push_str("#   msrv                          - Minimum Supported Rust Version\n");
  output.push_str("#   edition                       - Rust edition (\"2021\", \"2024\")\n");
  output.push_str("#   forbid_multiple_versions      - Deps that must have single version\n");
  output.push_str("#   require_workspace_inheritance - Force workspace.dependencies usage\n");
  output.push_str("#   allowed_licenses              - Permitted SPDX license identifiers\n");
  output.push_str("#   forbid_patch_replace          - Disallow [patch]/[replace] sections\n\n");

  output.push_str("[policy]\n");

  // Active fields
  if let Some(ref resolver) = config.policy.resolver {
    output.push_str(&format!(
      "resolver = \"{}\"  # Detected from workspace Cargo.toml\n",
      resolver
    ));
  } else {
    output.push_str("# resolver = \"2\"  # Enforce Cargo resolver version\n");
  }

  if let Some(ref msrv) = config.policy.msrv {
    output.push_str(&format!(
      "msrv = \"{}\"  # Detected from rust-version in Cargo.toml\n",
      msrv
    ));
  } else {
    output.push_str("# msrv = \"1.76.0\"  # Minimum Supported Rust Version\n");
  }

  if let Some(ref edition) = config.policy.edition {
    output.push_str(&format!(
      "edition = \"{}\"  # Detected from workspace Cargo.toml\n",
      edition
    ));
  } else {
    output.push_str("# edition = \"2021\"  # Enforce consistent Rust edition\n");
  }

  // Always-commented optional fields
  output.push_str("forbid_multiple_versions = []  # e.g., [\"tokio\", \"serde\", \"anyhow\"]\n");
  output.push_str("require_workspace_inheritance = false  # Enforce workspace.dependencies\n");
  output.push_str("# allowed_licenses = []  # e.g., [\"MIT\", \"Apache-2.0\", \"BSD-3-Clause\"]\n");
  output.push_str("# forbid_patch_replace = false  # Prevent [patch] and [replace] usage\n");

  output.push('\n');

  // Security
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Security & Git Operations                                               │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Configuration for split/sync operations and commit signing.\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   ssh_key_path          - SSH key for git operations\n");
  output.push_str("#   signing_key_path      - Key for signing commits (GPG/SSH)\n");
  output.push_str("#   require_signed_commits - Enforce commit signing\n");
  output.push_str("#   pr_branch_pattern     - Template for PR branch names\n");
  output.push_str("#   protected_branches    - Branches requiring PR workflow\n\n");

  output.push_str("[security]\n");
  output.push_str("# ssh_key_path = \"~/.ssh/id_ed25519\"  # Default: auto-detect from ~/.ssh/\n");
  output.push_str("# signing_key_path = \"~/.ssh/id_ed25519\"  # Default: same as ssh_key_path\n");
  output.push_str(&format!(
    "require_signed_commits = {}  # Enforce GPG/SSH signing\n",
    config.security.require_signed_commits
  ));
  output.push_str(&format!(
    "# pr_branch_pattern = \"{}\"  # Variables: {{{{crate}}}}, {{{{timestamp}}}}\n",
    config.security.pr_branch_pattern
  ));
  output.push_str("protected_branches = [");
  for (i, branch) in config.security.protected_branches.iter().enumerate() {
    if i > 0 {
      output.push_str(", ");
    }
    output.push_str(&format!("\"{}\"", branch));
  }
  output.push_str("]  # Cannot direct-push to these\n");

  output.push('\n');

  // Split/Sync
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Split/Sync Configuration (Monorepo ↔ Separate Repos)                   │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Configure crates to split from monorepo into standalone repositories\n");
  output.push_str("# with bidirectional synchronization.\n");
  output.push_str("#\n");
  output.push_str("# Commands:\n");
  output.push_str("#   cargo rail split <crate>   - Extract crate to separate repo\n");
  output.push_str("#   cargo rail sync <crate>    - Bidirectional sync\n");
  output.push_str("#   cargo rail status          - Show split/sync status\n");
  output.push_str("#\n");
  output.push_str("# Example configuration:\n");
  output.push_str("#\n");
  output.push_str("# [[splits]]\n");
  output.push_str("# name = \"my-crate\"  # Crate name\n");
  output.push_str("# remote = \"git@github.com:org/my-crate.git\"  # Target repository\n");
  output.push_str("# branch = \"main\"  # Branch to sync\n");
  output.push_str("# mode = \"single\"  # \"single\" or \"multi\" (layout mode)\n");
  output.push_str("#\n");
  output.push_str("# # Paths to include in split (workspace members)\n");
  output.push_str("# [[splits.paths]]\n");
  output.push_str("# crate = \"crates/my-crate\"  # Relative path from workspace root\n");
  output.push_str("#\n");
  output.push_str("# # Optional: Additional paths to sync\n");
  output.push_str("# [[splits.paths]]\n");
  output.push_str("# crate = \"crates/my-crate-macros\"\n");

  Ok(output)
}

/// Check if config file already exists at any location
fn check_existing_config(workspace_root: &Path) -> Option<PathBuf> {
  crate::config::RailConfig::find_config_path(workspace_root)
}

/// Ensure output directory exists (create if needed)
fn ensure_output_dir(output_path: &Path) -> RailResult<()> {
  if let Some(parent) = output_path.parent()
    && !parent.exists() {
      println!("📁 Creating directory: {}", parent.display());
      fs::create_dir_all(parent)
        .map_err(|e| RailError::message(format!("Failed to create directory {}: {}", parent.display(), e)))?;
    }
  Ok(())
}

/// Write config to file with atomic write (write to temp, then rename)
fn write_config_file(config_path: &Path, content: &str) -> RailResult<()> {
  // Use atomic write pattern (write to temp, then rename)
  let temp_path = config_path.with_extension("toml.tmp");

  fs::write(&temp_path, content).map_err(|e| {
    RailError::with_help(
      format!("Failed to write config to {}: {}", temp_path.display(), e),
      "Check file permissions and ensure the directory is writable",
    )
  })?;

  fs::rename(&temp_path, config_path)
    .map_err(|e| RailError::message(format!("Failed to finalize config file: {}", e)))?;

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  #[test]
  fn test_detect_toolchain_config_default() {
    let temp_dir = TempDir::new().unwrap();
    let config = detect_toolchain_config(temp_dir.path()).unwrap();
    assert_eq!(config.channel, "stable");
    assert_eq!(config.profile, "default");
  }

  #[test]
  fn test_detect_toolchain_config_from_toml() {
    let temp_dir = TempDir::new().unwrap();
    let toolchain_path = temp_dir.path().join("rust-toolchain.toml");
    fs::write(
      &toolchain_path,
      r#"[toolchain]
channel = "1.76.0"
profile = "minimal"
components = ["clippy", "rustfmt"]
"#,
    )
    .unwrap();

    let config = detect_toolchain_config(temp_dir.path()).unwrap();
    assert_eq!(config.channel, "1.76.0");
    assert_eq!(config.profile, "minimal");
    assert_eq!(config.components, vec!["clippy", "rustfmt"]);
  }

  #[test]
  fn test_detect_toolchain_config_from_plain_file() {
    let temp_dir = TempDir::new().unwrap();
    let toolchain_path = temp_dir.path().join("rust-toolchain");
    fs::write(&toolchain_path, "1.80.0\n").unwrap();

    let config = detect_toolchain_config(temp_dir.path()).unwrap();
    assert_eq!(config.channel, "1.80.0");
  }

  #[test]
  fn test_detect_policy_config() {
    let temp_dir = TempDir::new().unwrap();
    let cargo_path = temp_dir.path().join("Cargo.toml");
    fs::write(
      &cargo_path,
      r#"[workspace]
resolver = "2"

[workspace.package]
edition = "2024"
rust-version = "1.91"
"#,
    )
    .unwrap();

    let config = detect_policy_config(temp_dir.path()).unwrap();
    assert_eq!(config.edition, Some("2024".to_string()));
    assert_eq!(config.resolver, Some("2".to_string()));
    assert_eq!(config.msrv, Some("1.91".to_string()));
  }

  #[test]
  fn test_check_existing_config() {
    let temp_dir = TempDir::new().unwrap();

    // No config exists
    assert!(check_existing_config(temp_dir.path()).is_none());

    // Create config at .config/rail.toml
    let config_dir = temp_dir.path().join(".config");
    fs::create_dir(&config_dir).unwrap();
    fs::write(config_dir.join("rail.toml"), "").unwrap();

    assert!(check_existing_config(temp_dir.path()).is_some());
  }

  #[test]
  fn test_ensure_output_dir() {
    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join(".config/rail.toml");

    // Directory doesn't exist yet
    assert!(!output_path.parent().unwrap().exists());

    ensure_output_dir(&output_path).unwrap();

    // Directory should now exist
    assert!(output_path.parent().unwrap().exists());
  }

  #[test]
  fn test_serialize_config_with_comments() {
    let config = RailConfig {
      workspace: WorkspaceConfig {
        root: PathBuf::from("."),
      },
      toolchain: ToolchainConfig::default(),
      policy: PolicyConfig::default(),
      unify: UnifyConfig::default(),
      security: SecurityConfig::default(),
      splits: vec![],
    };

    let toml = serialize_config_with_comments(&config).unwrap();

    // Should contain section headers
    assert!(toml.contains("[workspace]"));
    assert!(toml.contains("[toolchain]"));
    assert!(toml.contains("[unify]"));

    // Should contain helpful comments
    assert!(toml.contains("cargo-rail configuration"));
    assert!(toml.contains("Documentation:"));
  }
}
