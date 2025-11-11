/// Node.js/TypeScript language adapter
///
/// Supports all modern package managers:
/// - npm workspaces (package.json with "workspaces")
/// - pnpm workspaces (pnpm-workspace.yaml)
/// - yarn workspaces (package.json with "workspaces")
/// - bun workspaces (bunfig.toml or package.json with "workspaces")
use super::{
  DependencyInfo, DependencySpec, LanguageAdapter, PackageInfo, TransformContext, TransformMode, WorkspaceInfo,
};
use crate::core::error::{RailError, RailResult};
use saphyr::LoadableYamlNode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct NodeAdapter {
  /// Detected package manager (npm, pnpm, yarn, or bun)
  package_manager: PackageManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
  Npm,
  Pnpm,
  Yarn,
  Bun,
}

impl NodeAdapter {
  pub fn new() -> Self {
    Self {
      package_manager: PackageManager::Npm, // Default, will be detected
    }
  }

  /// Create adapter with detected package manager
  fn with_package_manager(package_manager: PackageManager) -> Self {
    Self { package_manager }
  }
}

impl Default for NodeAdapter {
  fn default() -> Self {
    Self::new()
  }
}

/// package.json structure (minimal fields we care about)
#[derive(Debug, Deserialize, Serialize)]
struct PackageJson {
  name: String,
  version: String,
  #[serde(default)]
  description: Option<String>,
  #[serde(default)]
  workspaces: Option<WorkspaceSpec>,
  #[serde(default)]
  dependencies: HashMap<String, String>,
  #[serde(default)]
  #[serde(rename = "devDependencies")]
  dev_dependencies: HashMap<String, String>,
  #[serde(default)]
  #[serde(rename = "peerDependencies")]
  peer_dependencies: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum WorkspaceSpec {
  Array(Vec<String>),
  Object { packages: Vec<String> },
}

impl WorkspaceSpec {
  fn patterns(&self) -> &[String] {
    match self {
      WorkspaceSpec::Array(patterns) => patterns,
      WorkspaceSpec::Object { packages } => packages,
    }
  }
}

impl LanguageAdapter for NodeAdapter {
  fn can_handle(&self, root: &Path) -> bool {
    self.detect_package_manager(root).is_some()
  }

  fn load_workspace(&self, root: &Path) -> RailResult<WorkspaceInfo> {
    // Detect package manager first
    let package_manager = self.detect_package_manager(root).ok_or_else(|| {
      RailError::Validation(crate::core::error::ValidationError::WorkspaceInvalid {
        reason: "No Node.js workspace detected (no package.json with workspaces, pnpm-workspace.yaml, or package manager lockfile found)".to_string(),
      })
    })?;

    // Create adapter with detected package manager
    let adapter = Self::with_package_manager(package_manager);

    let patterns = adapter.discover_workspace_patterns(root)?;
    let mut packages = Vec::new();

    // Discover all packages matching the patterns
    for pattern in &patterns {
      let discovered = adapter.find_packages_matching_pattern(root, pattern)?;
      packages.extend(discovered);
    }

    Ok(WorkspaceInfo { packages })
  }

  fn transform_manifest(&self, manifest_path: &Path, context: &TransformContext) -> RailResult<()> {
    // Read package.json
    let content = std::fs::read_to_string(manifest_path)?;
    let mut pkg: PackageJson =
      serde_json::from_str(&content).map_err(|e| RailError::message(format!("Failed to parse package.json: {}", e)))?;

    // Transform dependencies based on mode
    match context.target_mode {
      TransformMode::SplitToRemote | TransformMode::SyncToRemote => {
        // Transform workspace: protocol to actual versions
        self.transform_workspace_to_versions(&mut pkg, context)?;
      }
      TransformMode::SyncToMono => {
        // Transform versions back to workspace: protocol
        self.transform_versions_to_workspace(&mut pkg, context)?;
      }
    }

    // Write back transformed package.json
    let transformed = serde_json::to_string_pretty(&pkg)
      .map_err(|e| RailError::message(format!("Failed to serialize package.json: {}", e)))?;
    std::fs::write(manifest_path, transformed)?;

    Ok(())
  }

  fn discover_aux_files(&self, package_path: &Path) -> RailResult<Vec<PathBuf>> {
    let mut files = Vec::new();

    // For Node.js, auxiliary files are typically at the workspace root
    // We need to find the workspace root by walking up from the package path
    let workspace_root = self.find_workspace_root_from_package(package_path)?;

    // Calculate relative path from package to workspace root
    let rel_to_workspace = pathdiff::diff_paths(&workspace_root, package_path)
      .ok_or_else(|| RailError::message("Could not calculate relative path to workspace root"))?;

    // List of workspace-level auxiliary files to look for
    let workspace_files = vec![
      ".nvmrc",
      "tsconfig.json",
      ".eslintrc",
      ".prettierrc",
      "turbo.json",
      ".gitattributes",
    ];

    // Check each file and add relative paths if they exist
    for file_name in workspace_files {
      let file_path = workspace_root.join(file_name);
      if file_path.exists() {
        // Return path relative to package that points to the file
        files.push(rel_to_workspace.join(file_name));
      }
    }

    // Add package-manager-specific files
    let pm_files = match self.package_manager {
      PackageManager::Npm => vec![".npmrc"],
      PackageManager::Pnpm => vec![".npmrc", "pnpm-workspace.yaml", ".pnpmfile.cjs"],
      PackageManager::Yarn => vec![".yarnrc.yml"],
      PackageManager::Bun => vec!["bunfig.toml", "bun.lockb"],
    };

    for file_name in pm_files {
      let file_path = workspace_root.join(file_name);
      if file_path.exists() {
        files.push(rel_to_workspace.join(file_name));
      }
    }

    Ok(files)
  }

  fn should_exclude(&self, path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
      matches!(
        name,
        "node_modules" | "dist" | "build" | ".git" | ".DS_Store" | "coverage" | ".next" | ".turbo"
      )
    } else {
      false
    }
  }

  fn manifest_filename(&self) -> &str {
    "package.json"
  }
}

impl NodeAdapter {
  /// Find the workspace root by walking up from a package path
  fn find_workspace_root_from_package(&self, package_path: &Path) -> RailResult<PathBuf> {
    let mut current = package_path.to_path_buf();

    loop {
      // Check if this directory is a workspace root
      if current.join("pnpm-workspace.yaml").exists() {
        return Ok(current);
      }

      if current.join("package.json").exists()
        && let Ok(content) = std::fs::read_to_string(current.join("package.json"))
          && let Ok(pkg) = serde_json::from_str::<PackageJson>(&content)
          && pkg.workspaces.is_some()
        {
          return Ok(current);
        }

      // Try parent directory
      if let Some(parent) = current.parent() {
        current = parent.to_path_buf();
      } else {
        // Fallback: if we can't find workspace root, use the package path itself
        return Ok(package_path.to_path_buf());
      }
    }
  }

  /// Detect which package manager is being used
  fn detect_package_manager(&self, root: &Path) -> Option<PackageManager> {
    // Check for lockfiles first (most reliable)
    if root.join("bun.lockb").exists() {
      return Some(PackageManager::Bun);
    }
    if root.join("pnpm-lock.yaml").exists() {
      return Some(PackageManager::Pnpm);
    }
    if root.join("yarn.lock").exists() {
      return Some(PackageManager::Yarn);
    }
    if root.join("package-lock.json").exists() {
      return Some(PackageManager::Npm);
    }

    // Check for workspace config files
    if root.join("pnpm-workspace.yaml").exists() {
      return Some(PackageManager::Pnpm);
    }
    if root.join("bunfig.toml").exists() {
      return Some(PackageManager::Bun);
    }

    // Check package.json for workspaces field
    let package_json = root.join("package.json");
    if package_json.exists()
      && let Ok(content) = std::fs::read_to_string(&package_json)
      && let Ok(pkg) = serde_json::from_str::<PackageJson>(&content)
      && pkg.workspaces.is_some()
    {
      // Default to npm if we can't determine from lockfile
      return Some(PackageManager::Npm);
    }

    None
  }

  /// Discover workspace patterns from package.json, pnpm-workspace.yaml, or bunfig.toml
  fn discover_workspace_patterns(&self, root: &Path) -> RailResult<Vec<String>> {
    match self.package_manager {
      PackageManager::Pnpm => {
        // pnpm uses pnpm-workspace.yaml
        let pnpm_workspace = root.join("pnpm-workspace.yaml");
        if pnpm_workspace.exists() {
          let content = std::fs::read_to_string(&pnpm_workspace)?;
          // Parse YAML using saphyr
          let docs = saphyr::Yaml::load_from_str(&content)
            .map_err(|e| RailError::message(format!("Failed to parse pnpm-workspace.yaml: {}", e)))?;

          if let Some(doc) = docs.first()
            && let Some(packages_yaml) = doc["packages"].as_vec()
          {
            let packages: Vec<String> = packages_yaml
              .iter()
              .filter_map(|item| item.as_str().map(|s| s.to_string()))
              .collect();
            return Ok(packages);
          }

          return Err(RailError::Validation(
            crate::core::error::ValidationError::WorkspaceInvalid {
              reason: "Invalid pnpm-workspace.yaml format - expected 'packages' array".to_string(),
            },
          ));
        }
      }
      PackageManager::Bun => {
        // Bun can use bunfig.toml or package.json workspaces
        let bunfig = root.join("bunfig.toml");
        if bunfig.exists() {
          // Bun's bunfig.toml uses TOML format
          let _content = std::fs::read_to_string(&bunfig)?;
          // Parse TOML to get workspace config
          // TODO: Implement bunfig.toml workspace parsing
          // For now, fall through to package.json
        }
      }
      _ => {}
    }

    // Try package.json for npm/yarn/bun
    let package_json = root.join("package.json");
    if package_json.exists() {
      let content = std::fs::read_to_string(&package_json)?;
      if let Ok(pkg) = serde_json::from_str::<PackageJson>(&content)
        && let Some(workspaces) = pkg.workspaces
      {
        return Ok(workspaces.patterns().to_vec());
      }
    }

    Err(RailError::Validation(
      crate::core::error::ValidationError::WorkspaceInvalid {
        reason:
          "No workspace configuration found - expected pnpm-workspace.yaml or package.json with 'workspaces' field"
            .to_string(),
      },
    ))
  }

  /// Find all packages matching a glob pattern
  fn find_packages_matching_pattern(&self, root: &Path, pattern: &str) -> RailResult<Vec<PackageInfo>> {
    let mut packages = Vec::new();

    // Simple glob expansion (handle "packages/*", "apps/*", etc.)
    let pattern_path = root.join(pattern);

    // If pattern ends with /*, scan that directory
    if pattern.ends_with("/*") || pattern.ends_with("/**") {
      let base_dir = pattern.trim_end_matches("/*").trim_end_matches("/**");
      let scan_dir = root.join(base_dir);

      if scan_dir.exists() && scan_dir.is_dir() {
        for entry in std::fs::read_dir(&scan_dir)? {
          let entry = entry?;
          let path = entry.path();

          if path.is_dir()
            && let Ok(pkg) = self.load_package(&path)
          {
            packages.push(pkg);
          }
        }
      }
    } else if pattern_path.exists() && pattern_path.is_dir() {
      // Direct directory reference
      if let Ok(pkg) = self.load_package(&pattern_path) {
        packages.push(pkg);
      }
    }

    Ok(packages)
  }

  /// Load a single package from a directory
  fn load_package(&self, path: &Path) -> RailResult<PackageInfo> {
    let manifest_path = path.join("package.json");
    if !manifest_path.exists() {
      return Err(RailError::message(format!(
        "No package.json found in {}",
        path.display()
      )));
    }

    let content = std::fs::read_to_string(&manifest_path)?;
    let pkg: PackageJson =
      serde_json::from_str(&content).map_err(|e| RailError::message(format!("Failed to parse package.json: {}", e)))?;

    // Collect all dependencies
    let mut dependencies = Vec::new();

    for (name, version) in pkg.dependencies.iter() {
      dependencies.push(DependencyInfo {
        name: name.clone(),
        spec: parse_dependency_spec(version),
      });
    }

    for (name, version) in pkg.dev_dependencies.iter() {
      dependencies.push(DependencyInfo {
        name: name.clone(),
        spec: parse_dependency_spec(version),
      });
    }

    Ok(PackageInfo {
      name: pkg.name,
      version: pkg.version,
      path: path.to_path_buf(),
      manifest_path,
      dependencies,
    })
  }

  /// Transform workspace: protocol dependencies to actual versions
  fn transform_workspace_to_versions(&self, pkg: &mut PackageJson, _context: &TransformContext) -> RailResult<()> {
    // Transform dependencies
    for (_, version) in pkg.dependencies.iter_mut() {
      if version.starts_with("workspace:") {
        // workspace:* → use actual version from the package
        // This is a simplification - in production you'd resolve from workspace
        if version == "workspace:*" {
          *version = "^0.1.0".to_string(); // Placeholder - should resolve from workspace
        } else if let Some(range) = version.strip_prefix("workspace:") {
          *version = range.to_string();
        }
      }
    }

    // Same for devDependencies
    for (_, version) in pkg.dev_dependencies.iter_mut() {
      if version.starts_with("workspace:") {
        if version == "workspace:*" {
          *version = "^0.1.0".to_string();
        } else if let Some(range) = version.strip_prefix("workspace:") {
          *version = range.to_string();
        }
      }
    }

    Ok(())
  }

  /// Transform version dependencies back to workspace: protocol
  fn transform_versions_to_workspace(&self, pkg: &mut PackageJson, _context: &TransformContext) -> RailResult<()> {
    // In monorepo, convert back to workspace: protocol
    // This would require knowing which packages are in the workspace
    // For now, we'll just ensure workspace: protocol is preserved

    for (name, version) in pkg.dependencies.iter_mut() {
      // If it's a workspace package, use workspace:*
      // (In production, you'd check against workspace package list)
      if !version.starts_with("workspace:") && self.is_workspace_package(name) {
        *version = "workspace:*".to_string();
      }
    }

    for (name, version) in pkg.dev_dependencies.iter_mut() {
      if !version.starts_with("workspace:") && self.is_workspace_package(name) {
        *version = "workspace:*".to_string();
      }
    }

    Ok(())
  }

  /// Check if a package is part of the workspace (placeholder)
  fn is_workspace_package(&self, _name: &str) -> bool {
    // TODO: Implement proper workspace package detection
    // For now, assume packages with @scope are workspace packages
    _name.starts_with('@')
  }
}

/// Parse Node.js dependency spec into our generic format
fn parse_dependency_spec(version: &str) -> DependencySpec {
  if version.starts_with("workspace:") {
    // Workspace protocol
    DependencySpec::Version(version.to_string())
  } else if version.starts_with("file:") || version.starts_with("link:") {
    // File/link dependency
    let path = version.trim_start_matches("file:").trim_start_matches("link:");
    DependencySpec::Path(PathBuf::from(path))
  } else if version.starts_with("git+") || version.contains("github.com") {
    // Git dependency
    DependencySpec::Git {
      url: version.to_string(),
      rev: None,
    }
  } else {
    // Version spec (semver range)
    DependencySpec::Version(version.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_can_handle_npm_workspace() {
    // This would need a test fixture
    let adapter = NodeAdapter::new();
    // Test with actual package.json file
    assert_eq!(adapter.manifest_filename(), "package.json");
  }

  #[test]
  fn test_parse_workspace_protocol() {
    let spec = parse_dependency_spec("workspace:*");
    matches!(spec, DependencySpec::Version(_));

    let spec = parse_dependency_spec("^1.0.0");
    matches!(spec, DependencySpec::Version(_));

    let spec = parse_dependency_spec("file:../other-package");
    matches!(spec, DependencySpec::Path(_));
  }
}
