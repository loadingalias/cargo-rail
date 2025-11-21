//! `cargo rail init` - Initialize cargo-rail configuration
//!
//! Auto-detects workspace structure, toolchain settings, and generates
//! a sensible .config/rail.toml with smart defaults.

use crate::config::{CratePath, RailConfig, SplitConfig, SplitMode, UnifyConfig, WorkspaceConfig, WorkspaceMode};
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
  let mut unify = default_unify_config();

  // Auto-detect targets from rust-toolchain.toml and .cargo/config.toml
  let detected_targets = detect_targets(workspace_root);
  if !detected_targets.is_empty() {
    println!(
      "  📍 Detected {} target triple(s) from rust-toolchain.toml/.cargo/config.toml",
      detected_targets.len()
    );
    unify.validate_targets = detected_targets;
  }

  let splits = detect_workspace_splits(ctx);

  // 3. Display summary
  println!("Workspace Analysis:");
  println!("  Root: {}", workspace_root.display());
  println!("{}", workspace_patterns.format_summary());

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

  let config = build_rail_config(workspace_root.to_path_buf(), unify, splits);

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

    println!("  3. Run 'cargo rail unify --dry-run' to preview dependency unification");
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

/// Auto-detect reasonable unify defaults
fn default_unify_config() -> UnifyConfig {
  UnifyConfig::default()
}

/// Detect target triples from rust-toolchain.toml and .cargo/config.toml
///
/// Intelligently merges targets from both sources:
/// 1. rust-toolchain.toml: [toolchain].targets array
/// 2. .cargo/config.toml: [target.<triple>] sections
///
/// Returns deduplicated list preserving order (rust-toolchain first, then cargo config additions)
fn detect_targets(workspace_root: &Path) -> Vec<String> {
  let mut targets = Vec::new();
  let mut seen = std::collections::HashSet::new();

  // 1. Check rust-toolchain.toml (or rust-toolchain)
  for toolchain_file in ["rust-toolchain.toml", "rust-toolchain"] {
    let toolchain_path = workspace_root.join(toolchain_file);
    if let Ok(content) = std::fs::read_to_string(&toolchain_path) {
      // Parse using toml_edit
      if let Ok(parsed) = content.parse::<toml_edit::DocumentMut>()
        && let Some(toolchain) = parsed.get("toolchain")
        && let Some(targets_item) = toolchain.get("targets")
        && let Some(targets_array) = targets_item.as_array()
      {
        for target in targets_array.iter() {
          if let Some(target_str) = target.as_str()
            && seen.insert(target_str.to_string())
          {
            targets.push(target_str.to_string());
          }
        }
      }
    }
  }

  // 2. Check .cargo/config.toml (or .cargo/config)
  for config_file in [".cargo/config.toml", ".cargo/config"] {
    let config_path = workspace_root.join(config_file);
    if let Ok(content) = std::fs::read_to_string(&config_path) {
      // Parse using toml_edit
      if let Ok(parsed) = content.parse::<toml_edit::DocumentMut>() {
        // Look for [target.<triple>] sections
        for (key, _value) in parsed.iter() {
          if let Some(triple) = key.strip_prefix("target.") {
            // Skip non-triple keys like target.dir
            if triple.contains('-') && seen.insert(triple.to_string()) {
              targets.push(triple.to_string());
            }
          }
        }
      }
    }
  }

  targets
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

    // Detect per-crate CHANGELOG file
    let changelog_path = detect_crate_changelog(crate_dir);

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
      changelog_path,
    });
  }

  splits
}

/// Detect CHANGELOG file in a crate directory
///
/// Looks for common CHANGELOG file patterns:
/// - CHANGELOG.md
/// - CHANGELOG.txt
/// - CHANGELOG
/// - CHANGES.md
/// - CHANGES
fn detect_crate_changelog(crate_dir: &cargo_metadata::camino::Utf8Path) -> Option<PathBuf> {
  let changelog_patterns = [
    "CHANGELOG.md",
    "CHANGELOG.txt",
    "CHANGELOG",
    "Changelog.md",
    "changelog.md",
    "CHANGES.md",
    "CHANGES.txt",
    "CHANGES",
    "Changes.md",
    "changes.md",
  ];

  for pattern in &changelog_patterns {
    let changelog = crate_dir.join(pattern);
    if changelog.exists() {
      // Return relative path from crate root
      return Some(PathBuf::from(pattern));
    }
  }

  None
}

fn build_rail_config(_workspace_root: PathBuf, unify: UnifyConfig, splits: Vec<SplitConfig>) -> RailConfig {
  RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    unify,
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
  output.push_str("#  • Dependency unification (workspace-hack elimination)\n");
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

  // Dependency Unification
  output.push_str("# ┌─────────────────────────────────────────────────────────────────────────┐\n");
  output.push_str("# │ Dependency Unification (Workspace-Hack Elimination)                     │\n");
  output.push_str("# └─────────────────────────────────────────────────────────────────────────┘\n");
  output.push_str("# Automatically unify workspace dependencies using native Cargo features.\n");
  output.push_str("# Run: cargo rail unify --dry-run  (to preview changes)\n");
  output.push_str("#      cargo rail unify            (to apply unification)\n");
  output.push_str("#\n");
  output.push_str("# Fields:\n");
  output.push_str("#   use_all_features           - Use --all-features for accurate analysis\n");
  output.push_str("#   validate_targets           - Per-target validation (catches platform issues)\n");
  output.push_str("#   max_parallel_jobs          - Parallelism (0 = auto-detect)\n");
  output.push_str("#   pin_transitives               - Pin transitive deps with fragmented features\n");
  output.push_str("#   pin_hosts                     - Crates to host transitive pins\n");
  output.push_str("#   auto_resolve_version_conflicts - Auto-resolve version conflicts (pick highest)\n");
  output.push_str("#   add_conflict_comments         - Add conflict markers to Cargo.toml\n");
  output.push_str("#   generate_report               - Generate unify-report.md\n");
  output.push_str("#   allow_renamed                 - Allow renamed dependencies (package = \"...\")\n\n");

  output.push_str("[unify]\n");
  output.push_str(&format!(
    "use_all_features = {}  # Ensure complete feature union analysis\n",
    config.unify.use_all_features
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

  output.push_str(&format!(
    "auto_resolve_version_conflicts = {}  # Pick highest version on conflicts\n",
    config.unify.auto_resolve_version_conflicts
  ));
  output.push_str(&format!(
    "add_conflict_comments = {}  # Add # ⚠️ markers to Cargo.toml\n",
    config.unify.add_conflict_comments
  ));
  output.push_str(&format!(
    "generate_report = {}  # Create unify-report.md\n",
    config.unify.generate_report
  ));
  output.push_str(&format!(
    "allow_renamed = {}  # Allow renamed dependencies (package = \"actual-name\")\n",
    config.unify.allow_renamed
  ));

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

  // Auto-detect targets from rust-toolchain.toml and .cargo/config.toml
  let detected_targets = detect_targets(workspace_root);
  if !detected_targets.is_empty() {
    println!(
      "  📍 Detected {} target triple(s) from rust-toolchain.toml/.cargo/config.toml",
      detected_targets.len()
    );
  }

  // 3. Build config
  let config = RailConfig {
    workspace: WorkspaceConfig {
      root: PathBuf::from("."),
    },
    unify: UnifyConfig {
      use_all_features: true,
      validate_targets: detected_targets,
      max_parallel_jobs: 0,
      pin_transitives: false,
      pin_hosts: vec![],
      auto_resolve_version_conflicts: true,
      conflict_resolution: "permissive".to_string(),
      add_conflict_comments: true,
      generate_report: true,
      allow_renamed: false,
      exclude: vec![],
      include: vec![],
    },
    release: crate::config::ReleaseConfig::default(),
    splits: vec![],
  };

  // 4. Try to load workspace context to detect splits
  //    If this fails (e.g., invalid workspace), we'll just use an empty splits list
  let splits = match WorkspaceContext::build(workspace_root) {
    Ok(ctx) => {
      let detected_splits = detect_workspace_splits(&ctx);
      if !detected_splits.is_empty() {
        println!("  Detected {} workspace member(s)", detected_splits.len());
      }
      detected_splits
    }
    Err(_) => {
      // Failed to load workspace - maybe not a cargo workspace yet
      // This is OK for init - just use empty splits
      vec![]
    }
  };

  // Update config with detected splits
  let config = RailConfig { splits, ..config };

  // 5. Serialize with comments
  let config_toml = serialize_config_with_comments(&config)?;

  // 6. Output
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

  #[test]
  fn test_serialize_config_with_comments() {
    let config = RailConfig {
      workspace: WorkspaceConfig {
        root: PathBuf::from("."),
      },
      unify: UnifyConfig::default(),
      release: crate::config::ReleaseConfig::default(),
      splits: vec![],
    };

    let toml = serialize_config_with_comments(&config).unwrap();

    // Should contain section headers
    assert!(toml.contains("[workspace]"));
    assert!(toml.contains("[unify]"));

    // Should contain helpful comments
    assert!(toml.contains("cargo-rail configuration"));
    assert!(toml.contains("Documentation:"));
  }
}
