//! `cargo rail init` - Initialize cargo-rail configuration
//!
//! Auto-detects workspace structure, toolchain settings, and generates
//! a sensible .config/rail.toml with smart defaults.

use crate::config::{
  CratePath, PolicyConfig, RailConfig, SecurityConfig, SplitConfig, SplitMode, ToolchainConfig, UnifyConfig,
  WorkspaceConfig, WorkspaceMode,
};
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
  let splits = detect_workspace_splits(ctx);

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

  let config = build_rail_config(workspace_root.to_path_buf(), toolchain, policy, unify, security, splits);

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
      && dir_name != "Cargo.toml"
      && !dir_name.starts_with('.')
    {
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
///
/// If rust-toolchain.toml exists, imports it and sets managed_by_rail = true.
/// This enables bidirectional sync: rail.toml becomes the source of truth,
/// and future syncs will update rust-toolchain.toml from rail.toml.
fn detect_toolchain_config(workspace_root: &Path) -> RailResult<ToolchainConfig> {
  let toolchain_path = workspace_root.join("rust-toolchain.toml");

  if !toolchain_path.exists() {
    // Try rust-toolchain without .toml extension
    let alt_path = workspace_root.join("rust-toolchain");
    if !alt_path.exists() {
      // No existing toolchain file - use default and don't enable management yet
      // User can enable managed_by_rail = true manually if they want rail to create it
      return Ok(ToolchainConfig::default());
    }

    // rust-toolchain (no extension) is just a channel string
    let content =
      fs::read_to_string(&alt_path).map_err(|e| RailError::message(format!("Failed to read rust-toolchain: {}", e)))?;
    let channel = content.trim().to_string();

    return Ok(ToolchainConfig {
      channel,
      managed_by_rail: true, // Imported from existing file
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
    managed_by_rail: true, // Imported from existing file - rail now manages it
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

/// Auto-detect workspace members and create split configs
fn detect_workspace_splits(ctx: &WorkspaceContext) -> Vec<SplitConfig> {
  let workspace_root = ctx.workspace_root();
  let members = ctx.cargo.metadata().list_crates();

  let mut splits = Vec::new();

  for pkg in members {
    // Get relative path from workspace root to crate directory
    let crate_dir = pkg.manifest_path.parent().expect("manifest has parent");
    let rel_path = match crate_dir.strip_prefix(workspace_root) {
      Ok(p) => p.to_path_buf(),
      Err(_) => continue, // Skip if not under workspace root
    };

    // Generate a reasonable remote URL placeholder (GitHub org/repo pattern)
    let remote = format!("git@github.com:org/{}.git", pkg.name);

    // Check if crate has publish = false in Cargo.toml
    let publish = pkg.publish.as_ref().map(|p| !p.is_empty()).unwrap_or(true);

    splits.push(SplitConfig {
      name: pkg.name.to_string(),
      remote,
      branch: "main".to_string(),
      mode: SplitMode::Single,
      workspace_mode: WorkspaceMode::default(),
      paths: vec![CratePath { path: rel_path.into() }],
      include: vec![],
      exclude: vec![],
      publish,
      changelog_path: None, // Use default from ReleaseConfig
    });
  }

  splits
}

/// Build a complete RailConfig from detected/default values
fn build_rail_config(
  _workspace_root: PathBuf,
  toolchain: ToolchainConfig,
  policy: PolicyConfig,
  unify: UnifyConfig,
  security: SecurityConfig,
  splits: Vec<SplitConfig>,
) -> RailConfig {
  RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    toolchain,
    policy,
    unify,
    security,
    release: crate::config::ReleaseConfig::default(),
    splits,
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
  output.push_str("#   channel         - Rust release channel (stable, beta, nightly, or version)\n");
  output.push_str("#   path            - Path to custom toolchain (mutually exclusive with channel)\n");
  output.push_str("#   profile         - Toolchain profile (minimal, default, complete)\n");
  output.push_str("#   components      - Additional components (clippy, rustfmt, rust-src, etc.)\n");
  output.push_str("#   targets         - Cross-compilation targets\n");
  output.push_str("#   managed_by_rail - Enable rust-toolchain.toml sync (true if imported)\n\n");

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

  // Add managed_by_rail field
  if config.toolchain.managed_by_rail {
    output.push_str("managed_by_rail = true  # rust-toolchain.toml imported - rail now manages it\n");
  } else {
    output.push_str("managed_by_rail = false  # Set to true to enable rust-toolchain.toml sync\n");
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

  // Release
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Release & Publishing                                                    │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Workspace-wide release defaults for version bumping and publishing.\n");
  output.push_str("# Per-crate settings are configured in [[splits]] below.\n");
  output.push_str("#\n");
  output.push_str("# Commands:\n");
  output.push_str("#   cargo rail release plan             - Preview release changes (dry-run)\n");
  output.push_str("#   cargo rail release publish --execute - Execute release\n");
  output.push_str("#   cargo rail release check            - Validate release readiness (CI)\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   tag_prefix           - Prefix for git tags (default: \"v\")\n");
  output.push_str("#   tag_format           - Tag template: {crate}-v{version} for monorepos\n");
  output.push_str("#   require_clean        - Require clean working directory\n");
  output.push_str("#   publish_delay        - Seconds between crate publishes\n");
  output.push_str("#   create_github_release - Auto-create GitHub releases via gh CLI\n");
  output.push_str("#   sign_tags            - Sign git tags with GPG/SSH\n");
  output.push_str("#   changelog_path       - Default changelog filename\n");
  output.push_str("#   skip_changelog_for   - Crates to skip changelog generation for\n");
  output.push_str("#   require_changelog_entries - Error if no entries are found\n\n");

  output.push_str("[release]\n");
  output.push_str(&format!(
    "tag_prefix = \"{}\"  # Prefix for version tags\n",
    config.release.tag_prefix
  ));
  output.push_str(&format!(
    "tag_format = \"{}\"  # Variables: {{crate}}, {{version}}\n",
    config.release.tag_format
  ));
  output.push_str(&format!(
    "require_clean = {}  # Require clean working directory\n",
    config.release.require_clean
  ));
  output.push_str(&format!(
    "publish_delay = {}  # Seconds between publishes\n",
    config.release.publish_delay
  ));
  output.push_str(&format!(
    "create_github_release = {}  # Create GitHub releases\n",
    config.release.create_github_release
  ));
  output.push_str(&format!(
    "sign_tags = {}  # Sign tags with GPG/SSH\n",
    config.release.sign_tags
  ));
  output.push_str(&format!(
    "changelog_path = \"{}\"  # Default changelog file\n",
    config.release.changelog_path
  ));
  output.push_str(&format!(
    "skip_changelog_for = {}  # e.g., [\"internal-tooling\"]\n",
    if config.release.skip_changelog_for.is_empty() {
      "[]".to_string()
    } else {
      format!(
        "[{}]",
        config
          .release
          .skip_changelog_for
          .iter()
          .map(|s| format!("\"{}\"", s))
          .collect::<Vec<_>>()
          .join(", ")
      )
    }
  ));
  output.push_str(&format!(
    "require_changelog_entries = {}  # Fail if no commits for a release\n",
    config.release.require_changelog_entries
  ));

  output.push('\n');

  // Split/Sync
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Split/Sync Configuration (Monorepo ↔ Separate Repos)                   │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Each workspace member is auto-detected and configured for split/sync.\n");
  output.push_str("# Update 'remote' URLs and 'publish' flags as needed.\n");
  output.push_str("#\n");
  output.push_str("# Commands:\n");
  output.push_str("#   cargo rail split <crate>   - Extract crate to separate repo\n");
  output.push_str("#   cargo rail sync <crate>    - Bidirectional sync\n");
  output.push_str("#   cargo rail status          - Show split/sync status\n");
  output.push_str("#\n");
  output.push_str("# Fields per [[splits]] entry:\n");
  output.push_str("#   name           - Crate name\n");
  output.push_str("#   remote         - Target repository URL (update this!)\n");
  output.push_str("#   branch         - Branch to sync (default: main)\n");
  output.push_str("#   mode           - \"single\" or \"combined\" layout\n");
  output.push_str("#   publish        - Enable publishing to crates.io (default: true)\n");
  output.push_str("#   changelog_path - Per-crate changelog override (optional)\n");
  output.push_str("#\n");

  // Serialize detected splits
  if config.splits.is_empty() {
    output.push_str("# No workspace members detected. Example:\n");
    output.push_str("#\n");
    output.push_str("# [[splits]]\n");
    output.push_str("# name = \"my-crate\"\n");
    output.push_str("# remote = \"git@github.com:org/my-crate.git\"\n");
    output.push_str("# branch = \"main\"\n");
    output.push_str("# mode = \"single\"\n");
    output.push_str("# publish = true\n");
    output.push_str("#\n");
    output.push_str("# [[splits.paths]]\n");
    output.push_str("# crate = \"crates/my-crate\"\n");
  } else {
    output.push_str(&format!(
      "# Auto-detected {} workspace member(s):\n\n",
      config.splits.len()
    ));

    for split in &config.splits {
      output.push_str("[[splits]]\n");
      output.push_str(&format!("name = \"{}\"\n", split.name));
      output.push_str(&format!(
        "remote = \"{}\"  # TODO: Update with actual repository URL\n",
        split.remote
      ));
      output.push_str(&format!("branch = \"{}\"\n", split.branch));
      output.push_str(&format!(
        "mode = \"{}\"\n",
        match split.mode {
          SplitMode::Single => "single",
          SplitMode::Combined => "combined",
        }
      ));
      output.push_str(&format!(
        "publish = {}  # {}\n",
        split.publish,
        if split.publish {
          "Enable crates.io publishing"
        } else {
          "Skip publishing (publish = false in Cargo.toml)"
        }
      ));

      if let Some(ref changelog) = split.changelog_path {
        output.push_str(&format!("changelog_path = \"{}\"\n", changelog.display()));
      }

      output.push('\n');

      for path in &split.paths {
        output.push_str("[[splits.paths]]\n");
        output.push_str(&format!("crate = \"{}\"\n", path.path.display()));
      }

      output.push('\n');
    }
  }

  Ok(output)
}

/// Check if config file already exists at any location
fn check_existing_config(workspace_root: &Path) -> Option<PathBuf> {
  crate::config::RailConfig::find_config_path(workspace_root)
}

/// Ensure output directory exists (create if needed)
fn ensure_output_dir(output_path: &Path) -> RailResult<()> {
  if let Some(parent) = output_path.parent()
    && !parent.exists()
  {
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

/// Standalone init that doesn't require WorkspaceContext
///
/// This is used when init is called on a directory that may not have
/// a valid Cargo workspace yet (e.g., empty workspace or invalid state).
pub fn run_init_standalone(
  workspace_root: &Path,
  output_path: &str,
  force: bool,
  _non_interactive: bool,
  dry_run: bool,
) -> RailResult<()> {
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

  let toolchain_config = detect_toolchain_config(workspace_root)?;
  let policy_config = detect_policy_config(workspace_root)?;

  // Display detected settings
  println!(
    "  Toolchain: {} ({})",
    toolchain_config.channel, toolchain_config.profile
  );
  if let Some(ref resolver) = policy_config.resolver {
    println!("  Resolver: {}", resolver);
  }
  if let Some(ref edition) = policy_config.edition {
    println!("  Edition: {}", edition);
  }
  if let Some(ref msrv) = policy_config.msrv {
    println!("  MSRV: {}", msrv);
  }
  println!();

  // 3. Build config
  let config = RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    toolchain: toolchain_config,
    policy: policy_config,
    unify: UnifyConfig {
      use_all_features: true,
      sync_on_unify: true,
      validate_targets: vec![],
      max_parallel_jobs: 0,
      pin_transitives: false,
      pin_hosts: vec![],
    },
    security: SecurityConfig {
      ssh_key_path: None,
      signing_key_path: None,
      require_signed_commits: false,
      pr_branch_pattern: "rail/sync/{crate}/{timestamp}".to_string(),
      protected_branches: vec!["main".to_string(), "master".to_string()],
    },
    release: crate::config::ReleaseConfig::default(),
    splits: vec![],
  };

  // 4. Serialize with rich comments
  let config_toml = serialize_config_with_comments(&config)?;

  // 5. Output
  if dry_run {
    println!("--- {} ---", output_path);
    println!("{}", config_toml);
    println!("\n✅ Dry-run complete (no files written)");
  } else {
    // Create parent directory if needed
    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent).map_err(|e| {
        RailError::with_help(
          format!("Failed to create directory {}: {}", parent.display(), e),
          "Check file permissions",
        )
      })?;
    }

    write_config_file(&config_path, &config_toml)?;
    println!("✅ Created {}", config_path.display());
    println!("\nNext steps:");
    println!("  1. Review and customize {}", output_path);
    println!("  2. Run `cargo rail unify` to normalize dependencies");
    println!("  3. Run `cargo rail test` for change-based testing");
  }

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
      release: crate::config::ReleaseConfig::default(),
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
