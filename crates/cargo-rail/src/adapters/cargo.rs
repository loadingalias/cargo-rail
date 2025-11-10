/// Cargo/Rust language adapter
use super::{
  DependencyInfo, DependencySpec, LanguageAdapter, PackageInfo, TransformContext, TransformMode, WorkspaceInfo,
};
use crate::cargo::files::AuxiliaryFiles;
use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::transform::Transform;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct CargoAdapter;

impl CargoAdapter {
  pub fn new() -> Self {
    Self
  }
}

impl Default for CargoAdapter {
  fn default() -> Self {
    Self::new()
  }
}

impl LanguageAdapter for CargoAdapter {
  fn can_handle(&self, root: &Path) -> bool {
    let cargo_toml = root.join("Cargo.toml");
    if !cargo_toml.exists() {
      return false;
    }

    // Check if it's a workspace
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
      content.contains("[workspace]")
    } else {
      false
    }
  }

  fn load_workspace(&self, root: &Path) -> Result<WorkspaceInfo> {
    let metadata = WorkspaceMetadata::load(root)?;
    let cargo_packages = metadata.list_crates();

    let packages: Vec<PackageInfo> = cargo_packages
      .iter()
      .map(|pkg| {
        let dependencies = pkg
          .dependencies
          .iter()
          .map(|dep| {
            let spec = if let Some(path) = &dep.path {
              DependencySpec::Path(path.clone().into_std_path_buf())
            } else {
              DependencySpec::Version(dep.req.to_string())
            };

            DependencyInfo {
              name: dep.name.to_string(),
              spec,
            }
          })
          .collect();

        PackageInfo {
          name: pkg.name.to_string(),
          version: pkg.version.to_string(),
          path: pkg.manifest_path.parent().unwrap().to_path_buf().into(),
          manifest_path: pkg.manifest_path.clone().into_std_path_buf(),
          dependencies,
        }
      })
      .collect();

    Ok(WorkspaceInfo {
      root: root.to_path_buf(),
      packages,
    })
  }

  fn transform_manifest(&self, manifest_path: &Path, context: &TransformContext) -> Result<()> {
    // Load workspace metadata to create the transformer
    let metadata = WorkspaceMetadata::load(&context.workspace_root)?;
    let transform = CargoTransform::new(metadata);

    // Read the manifest
    let content = std::fs::read_to_string(manifest_path)?;

    // Apply transformation based on mode
    let transformed = match context.target_mode {
      TransformMode::SplitToRemote | TransformMode::SyncToRemote => {
        let ctx = crate::core::transform::TransformContext {
          crate_name: context.package_name.clone(),
          workspace_root: context.workspace_root.clone(),
        };
        transform.transform_to_split(&content, &ctx)?
      }
      TransformMode::SyncToMono => {
        let ctx = crate::core::transform::TransformContext {
          crate_name: context.package_name.clone(),
          workspace_root: context.workspace_root.clone(),
        };
        transform.transform_to_mono(&content, &ctx)?
      }
    };

    // Write back the transformed manifest
    std::fs::write(manifest_path, transformed)?;
    Ok(())
  }

  fn discover_aux_files(&self, package_path: &Path) -> Result<Vec<PathBuf>> {
    let aux = AuxiliaryFiles::discover(package_path)?;
    Ok(aux.list_target_paths())
  }

  fn should_exclude(&self, path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
      matches!(name, "target" | ".git" | "node_modules" | ".DS_Store" | "Cargo.lock")
    } else {
      false
    }
  }

  fn manifest_filename(&self) -> &str {
    "Cargo.toml"
  }
}

/// Cargo package descriptor implementation
pub struct CargoPackageDescriptor {
  name: String,
  version: String,
  description: Option<String>,
  path: PathBuf,
  dependencies: Vec<super::descriptor::Dependency>,
}

impl CargoPackageDescriptor {
  /// Create a package descriptor from a path
  pub fn from_path(path: &Path) -> Result<Self> {
    let manifest_path = path.join("Cargo.toml");
    if !manifest_path.exists() {
      anyhow::bail!("No Cargo.toml found at {}", path.display());
    }

    // Read and parse Cargo.toml
    let content = std::fs::read_to_string(&manifest_path)?;
    let manifest: toml::Value = toml::from_str(&content)?;

    let package = manifest
      .get("package")
      .ok_or_else(|| anyhow::anyhow!("No [package] section in Cargo.toml"))?;

    let name = package
      .get("name")
      .and_then(|v| v.as_str())
      .ok_or_else(|| anyhow::anyhow!("No package name in Cargo.toml"))?
      .to_string();

    let version = package
      .get("version")
      .and_then(|v| v.as_str())
      .ok_or_else(|| anyhow::anyhow!("No package version in Cargo.toml"))?
      .to_string();

    let description = package
      .get("description")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());

    // Parse dependencies
    let mut dependencies = Vec::new();

    // Regular dependencies
    if let Some(deps) = manifest.get("dependencies").and_then(|v| v.as_table()) {
      for (dep_name, dep_spec) in deps {
        dependencies.push(parse_cargo_dependency(dep_name, dep_spec, false, false)?);
      }
    }

    // Dev dependencies
    if let Some(dev_deps) = manifest.get("dev-dependencies").and_then(|v| v.as_table()) {
      for (dep_name, dep_spec) in dev_deps {
        dependencies.push(parse_cargo_dependency(dep_name, dep_spec, true, false)?);
      }
    }

    // Build dependencies
    if let Some(build_deps) = manifest.get("build-dependencies").and_then(|v| v.as_table()) {
      for (dep_name, dep_spec) in build_deps {
        dependencies.push(parse_cargo_dependency(dep_name, dep_spec, false, true)?);
      }
    }

    Ok(Self {
      name,
      version,
      description,
      path: path.to_path_buf(),
      dependencies,
    })
  }
}

impl super::descriptor::PackageDescriptor for CargoPackageDescriptor {
  fn name(&self) -> &str {
    &self.name
  }

  fn version(&self) -> &str {
    &self.version
  }

  fn description(&self) -> Option<&str> {
    self.description.as_deref()
  }

  fn dependencies(&self) -> &[super::descriptor::Dependency] {
    &self.dependencies
  }

  fn path(&self) -> &Path {
    &self.path
  }

  fn manifest_filename(&self) -> &str {
    "Cargo.toml"
  }

  fn update_version(&self, new_version: &str) -> Result<()> {
    let manifest_path = self.manifest_path();
    let content = std::fs::read_to_string(&manifest_path)?;

    // Parse as toml_edit to preserve formatting
    let mut doc: toml_edit::DocumentMut = content.parse()?;

    if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_mut()) {
      package["version"] = toml_edit::value(new_version);
    } else {
      anyhow::bail!("No [package] section in Cargo.toml");
    }

    std::fs::write(&manifest_path, doc.to_string())?;
    Ok(())
  }
}

/// Helper to parse a Cargo dependency specification
fn parse_cargo_dependency(
  name: &str,
  spec: &toml::Value,
  is_dev: bool,
  is_build: bool,
) -> Result<super::descriptor::Dependency> {
  use super::descriptor::{Dependency, DependencySpec};

  let dep_spec = if let Some(version_str) = spec.as_str() {
    // Simple version string
    if version_str == "workspace" {
      DependencySpec::Workspace
    } else {
      DependencySpec::Version(version_str.to_string())
    }
  } else if let Some(table) = spec.as_table() {
    // Detailed dependency specification
    if table.contains_key("workspace") {
      DependencySpec::Workspace
    } else if let Some(path) = table.get("path").and_then(|v| v.as_str()) {
      DependencySpec::Path(PathBuf::from(path))
    } else if let Some(git_url) = table.get("git").and_then(|v| v.as_str()) {
      let rev = table.get("rev").and_then(|v| v.as_str()).map(|s| s.to_string());
      DependencySpec::Git {
        url: git_url.to_string(),
        rev,
      }
    } else if let Some(version) = table.get("version").and_then(|v| v.as_str()) {
      DependencySpec::Version(version.to_string())
    } else {
      anyhow::bail!("Invalid dependency specification for '{}'", name);
    }
  } else {
    anyhow::bail!("Invalid dependency specification for '{}'", name);
  };

  Ok(Dependency {
    name: name.to_string(),
    spec: dep_spec,
    is_dev,
    is_build,
  })
}

/// Create a package descriptor from a path (for use by descriptor::from_path)
pub fn create_package_descriptor(path: &Path) -> Result<Box<dyn super::descriptor::PackageDescriptor>> {
  Ok(Box::new(CargoPackageDescriptor::from_path(path)?))
}
