/// Cargo/Rust language adapter
use super::{
  DependencyInfo, DependencySpec, LanguageAdapter, PackageInfo, TransformContext, TransformMode, WorkspaceInfo,
};
use crate::cargo::files::AuxiliaryFiles;
use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::error::{RailError, RailResult};
use crate::core::transform::Transform;
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

  fn load_workspace(&self, root: &Path) -> RailResult<WorkspaceInfo> {
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

        let pkg_path = pkg
          .manifest_path
          .parent()
          .ok_or_else(|| RailError::message(format!("Package '{}' manifest has no parent directory", pkg.name)))?;

        Ok(PackageInfo {
          name: pkg.name.to_string(),
          version: pkg.version.to_string(),
          path: pkg_path.to_path_buf().into(),
          manifest_path: pkg.manifest_path.clone().into_std_path_buf(),
          dependencies,
        })
      })
      .collect::<RailResult<Vec<_>>>()?;

    Ok(WorkspaceInfo { packages })
  }

  fn transform_manifest(&self, manifest_path: &Path, context: &TransformContext) -> RailResult<()> {
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

  fn discover_aux_files(&self, package_path: &Path) -> RailResult<Vec<PathBuf>> {
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
