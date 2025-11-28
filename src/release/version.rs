//! Version bumping and Cargo.toml manipulation

use crate::error::{RailError, RailResult};
use semver::Version;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use toml_edit::DocumentMut;

/// Version bump strategy
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BumpType {
  /// Increment patch version (0.0.Z or X.Y.z)
  Patch,
  /// Increment minor version (0.Y.0 or X.y.0)
  Minor,
  /// Increment major version (X.0.0 or 0.Y.0 for 0.x)
  Major,
  /// Set exact version
  Exact(Version),
}

impl FromStr for BumpType {
  type Err = RailError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_lowercase().as_str() {
      "major" => Ok(Self::Major),
      "minor" => Ok(Self::Minor),
      "patch" => Ok(Self::Patch),
      _ => {
        // Try parsing as explicit version
        let version = Version::parse(s).map_err(|e| {
          RailError::with_help(
            format!("Invalid version bump: {}", s),
            format!(
              "Use 'major', 'minor', 'patch', or a valid semver version (e.g., '1.2.3'). Parse error: {}",
              e
            ),
          )
        })?;
        Ok(Self::Exact(version))
      }
    }
  }
}

impl BumpType {
  /// Apply bump to a version
  pub fn apply(&self, current: &Version) -> Version {
    match self {
      Self::Patch => {
        let mut next = current.clone();
        next.patch += 1;
        next.pre = semver::Prerelease::EMPTY;
        next.build = semver::BuildMetadata::EMPTY;
        next
      }
      Self::Minor => {
        let mut next = current.clone();
        next.minor += 1;
        next.patch = 0;
        next.pre = semver::Prerelease::EMPTY;
        next.build = semver::BuildMetadata::EMPTY;
        next
      }
      Self::Major => {
        let mut next = current.clone();
        // Always bump major version - including 0.x -> 1.0.0
        // Users who want semver-compatible "breaking change" on 0.x can use --bump minor
        next.major += 1;
        next.minor = 0;
        next.patch = 0;
        next.pre = semver::Prerelease::EMPTY;
        next.build = semver::BuildMetadata::EMPTY;
        next
      }
      Self::Exact(version) => version.clone(),
    }
  }
}

/// Version bumper for Cargo.toml files
pub struct VersionBumper;

impl VersionBumper {
  /// Get current version from Cargo.toml
  pub fn get_version(manifest_path: &Path) -> RailResult<Version> {
    let content = fs::read_to_string(manifest_path)
      .map_err(|e| RailError::message(format!("Failed to read {}: {}", manifest_path.display(), e)))?;

    let doc: DocumentMut = content
      .parse()
      .map_err(|e| RailError::message(format!("Failed to parse {} as TOML: {}", manifest_path.display(), e)))?;

    let version_str = doc
      .get("package")
      .and_then(|p| p.get("version"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| {
        RailError::with_help(
          format!("No version field in {}", manifest_path.display()),
          "Ensure [package.version] is set or use workspace inheritance",
        )
      })?;

    Version::parse(version_str).map_err(|e| {
      RailError::message(format!(
        "Invalid version '{}' in {}: {}",
        version_str,
        manifest_path.display(),
        e
      ))
    })
  }

  /// Bump version in Cargo.toml
  ///
  /// Uses lossless TOML editing to preserve comments and formatting.
  ///
  /// # Errors
  /// Returns an error if the crate uses workspace version inheritance.
  pub fn bump_version(manifest_path: &Path, bump_type: BumpType) -> RailResult<Version> {
    use crate::toml::editor::TomlEditor;

    let mut editor = TomlEditor::open(manifest_path)?;
    let doc = editor.doc();

    // Get current version - check for workspace inheritance
    let version_item = doc.get("package").and_then(|p| p.get("version"));

    // Check if using workspace inheritance
    if let Some(item) = version_item
      && (item.is_inline_table() || item.is_table())
    {
      // Check for { workspace = true }
      if let Some(tbl) = item.as_inline_table() {
        if tbl.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
          return Err(RailError::with_help(
            format!(
              "Cannot bump version in {}: uses workspace inheritance",
              manifest_path.display()
            ),
            "Set a specific version in this crate's Cargo.toml, or update [workspace.package.version] in the root Cargo.toml",
          ));
        }
      } else if let Some(tbl) = item.as_table()
        && tbl.get("workspace").and_then(|v| v.as_bool()) == Some(true)
      {
        return Err(RailError::with_help(
          format!(
            "Cannot bump version in {}: uses workspace inheritance",
            manifest_path.display()
          ),
          "Set a specific version in this crate's Cargo.toml, or update [workspace.package.version] in the root Cargo.toml",
        ));
      }
    }

    let current_str = version_item.and_then(|v| v.as_str()).ok_or_else(|| {
      RailError::with_help(
        format!("No version field in {}", manifest_path.display()),
        "Ensure [package.version] is set",
      )
    })?;

    let current = Version::parse(current_str).map_err(|e| {
      RailError::message(format!(
        "Invalid version '{}' in {}: {}",
        current_str,
        manifest_path.display(),
        e
      ))
    })?;

    // Calculate new version
    let new_version = bump_type.apply(&current);

    // Update in document
    editor.set("package.version", new_version.to_string())?;

    // Write back (atomic write handled by editor)
    editor.write()?;

    Ok(new_version)
  }

  /// Update dependency version in a Cargo.toml
  ///
  /// Updates dependencies that reference a specific crate to use a new version.
  /// This is needed when bumping versions in a workspace - dependent crates
  /// need to update their dependency declarations.
  pub fn update_dependency_version(manifest_path: &Path, dep_name: &str, new_version: &Version) -> RailResult<bool> {
    use crate::toml::editor::TomlEditor;

    let mut editor = TomlEditor::open(manifest_path)?;
    let doc = editor.doc_mut();
    let mut updated = false;

    // Check all dependency sections
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
      if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_like_mut())
        && let Some(dep) = deps.get_mut(dep_name)
      {
        // Update version field
        if let Some(dep_table) = dep.as_inline_table_mut()
          && dep_table.contains_key("version")
        {
          dep_table.insert("version", format!("^{}", new_version).into());
          updated = true;
        } else if let Some(dep_table) = dep.as_table_mut()
          && dep_table.contains_key("version")
        {
          dep_table["version"] = toml_edit::value(format!("^{}", new_version));
          updated = true;
        }
      }
    }

    if updated {
      // Write back
      editor.write()?;
    }

    Ok(updated)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_bump_patch() {
    let v = Version::parse("1.2.3").unwrap();
    let next = BumpType::Patch.apply(&v);
    assert_eq!(next, Version::parse("1.2.4").unwrap());
  }

  #[test]
  fn test_bump_minor() {
    let v = Version::parse("1.2.3").unwrap();
    let next = BumpType::Minor.apply(&v);
    assert_eq!(next, Version::parse("1.3.0").unwrap());
  }

  #[test]
  fn test_bump_major() {
    let v = Version::parse("1.2.3").unwrap();
    let next = BumpType::Major.apply(&v);
    assert_eq!(next, Version::parse("2.0.0").unwrap());
  }

  #[test]
  fn test_bump_major_pre_1() {
    // For 0.x versions, major bump goes to 1.0.0
    let v = Version::parse("0.2.3").unwrap();
    let next = BumpType::Major.apply(&v);
    assert_eq!(next, Version::parse("1.0.0").unwrap());
  }

  #[test]
  fn test_bump_strips_prerelease() {
    let v = Version::parse("1.2.3-beta.1").unwrap();
    let next = BumpType::Patch.apply(&v);
    assert_eq!(next, Version::parse("1.2.4").unwrap());
  }
}
