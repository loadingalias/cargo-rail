//! Semantic Cargo manifest and lockfile change localization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use cargo_metadata::{Package, PackageId};
use serde_json::{Map, Value};

use super::classify::{FileProfile, classify_path};
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;

/// Localized semantic impact for one changed Cargo input.
#[derive(Debug)]
pub(crate) struct SemanticFileChange {
  pub(crate) scope: SemanticScope,
  pub(crate) input: &'static str,
  pub(crate) fallback: Option<&'static str>,
}

/// Packages authorized by a semantic diff, or a conservative workspace fallback.
#[derive(Debug)]
pub(crate) enum SemanticScope {
  None,
  Packages(BTreeSet<PackageId>),
  Workspace,
}

pub(crate) fn analyze(
  ctx: &WorkspaceContext,
  changed_files: &[String],
  base_ref: &str,
  head_ref: Option<&str>,
) -> RailResult<BTreeMap<String, SemanticFileChange>> {
  let semantic_paths = changed_files
    .iter()
    .filter(|path| {
      matches!(
        classify_path(std::path::Path::new(path)),
        FileProfile::TomlManifest | FileProfile::TomlWorkspace | FileProfile::CargoLock
      )
    })
    .collect::<Vec<_>>();
  if semantic_paths.is_empty() {
    return Ok(BTreeMap::new());
  }

  let absolute_paths = semantic_paths
    .iter()
    .map(|path| ctx.workspace_root().join(path))
    .collect::<Vec<_>>();
  let base_items = absolute_paths
    .iter()
    .map(|path| (base_ref, path.as_path()))
    .collect::<Vec<_>>();
  let base = ctx.git()?.git().read_files_bulk(&base_items)?;
  let head = if let Some(head_ref) = head_ref {
    let head_items = absolute_paths
      .iter()
      .map(|path| (head_ref, path.as_path()))
      .collect::<Vec<_>>();
    ctx.git()?.git().read_files_bulk(&head_items)?
  } else {
    absolute_paths
      .iter()
      .map(|path| match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
      })
      .collect::<Result<Vec<_>, _>>()?
  };

  semantic_paths
    .into_iter()
    .zip(base)
    .zip(head)
    .map(|((path, before), after)| {
      let mut change = match classify_path(std::path::Path::new(path)) {
        FileProfile::TomlWorkspace => workspace_manifest_change(ctx, &before, &after),
        FileProfile::TomlManifest => package_manifest_change(ctx, path, &before, &after),
        FileProfile::CargoLock => lockfile_change(ctx, &before, &after),
        _ => unreachable!("semantic paths were filtered by profile"),
      };
      if head_ref.is_some() && !matches!(change.scope, SemanticScope::None) {
        change = fallback(change.input, "historical_resolution_unavailable");
      }
      Ok((path.clone(), change))
    })
    .collect()
}

fn package_manifest_change(ctx: &WorkspaceContext, path: &str, before: &[u8], after: &[u8]) -> SemanticFileChange {
  if before.is_empty()
    && parse_toml(after).is_some()
    && let Some(package) = ctx.graph().file_to_package(std::path::Path::new(path))
  {
    return SemanticFileChange {
      scope: SemanticScope::Packages(BTreeSet::from([package.id.clone()])),
      input: "manifest",
      fallback: None,
    };
  }
  let (Some(mut before), Some(mut after)) = (parse_toml(before), parse_toml(after)) else {
    return fallback("manifest", "manifest_parse_unknown");
  };
  strip_package_metadata(&mut before);
  strip_package_metadata(&mut after);
  if before == after {
    return unchanged("manifest");
  }
  let Some(package) = ctx.graph().file_to_package(std::path::Path::new(path)) else {
    return fallback("manifest", "manifest_owner_unknown");
  };
  SemanticFileChange {
    scope: SemanticScope::Packages(BTreeSet::from([package.id.clone()])),
    input: "manifest",
    fallback: None,
  }
}

fn workspace_manifest_change(ctx: &WorkspaceContext, before: &[u8], after: &[u8]) -> SemanticFileChange {
  let (Some(before), Some(after)) = (parse_toml(before), parse_toml(after)) else {
    return fallback("workspace_manifest", "manifest_parse_unknown");
  };
  if before == after {
    return unchanged("workspace_manifest");
  }
  let (Some(before_root), Some(after_root)) = (before.as_object(), after.as_object()) else {
    return fallback("workspace_manifest", "manifest_shape_unknown");
  };

  let mut before_other = before_root.clone();
  let mut after_other = after_root.clone();
  let before_workspace = before_other.remove("workspace");
  let after_workspace = after_other.remove("workspace");
  let package_manifest_keys = [
    "package",
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "target",
    "features",
    "lib",
    "bin",
    "example",
    "test",
    "bench",
    "lints",
    "badges",
  ];
  if let Some(package) = before_other.get_mut("package").and_then(Value::as_object_mut) {
    package.remove("metadata");
  }
  if let Some(package) = after_other.get_mut("package").and_then(Value::as_object_mut) {
    package.remove("metadata");
  }
  let root_package_changed = package_manifest_keys
    .iter()
    .any(|key| before_other.get(*key) != after_other.get(*key));
  for key in package_manifest_keys {
    before_other.remove(key);
    after_other.remove(key);
  }
  if before_other != after_other {
    return fallback("workspace_manifest", "resolver_or_source_change");
  }
  let (mut before_workspace, mut after_workspace) = match (before_workspace, after_workspace) {
    (None, None) => (Map::new(), Map::new()),
    (Some(before), Some(after)) => {
      let (Some(before), Some(after)) = (before.as_object().cloned(), after.as_object().cloned()) else {
        return fallback("workspace_manifest", "workspace_shape_unknown");
      };
      (before, after)
    }
    (None, Some(_)) | (Some(_), None) => return fallback("workspace_manifest", "workspace_shape_unknown"),
  };

  let dependency_keys = changed_table_keys(
    before_workspace.remove("dependencies").as_ref(),
    after_workspace.remove("dependencies").as_ref(),
  );
  let package_keys = changed_table_keys(
    before_workspace.remove("package").as_ref(),
    after_workspace.remove("package").as_ref(),
  );
  let lints_changed = before_workspace.remove("lints") != after_workspace.remove("lints");
  before_workspace.remove("metadata");
  after_workspace.remove("metadata");
  if before_workspace != after_workspace {
    return fallback("workspace_manifest", "resolver_or_membership_change");
  }

  let mut packages = BTreeSet::new();
  if root_package_changed {
    let Some(package) = ctx.graph().file_to_package(std::path::Path::new("Cargo.toml")) else {
      return fallback("workspace_manifest", "manifest_owner_unknown");
    };
    packages.insert(package.id.clone());
  }
  for package in ctx.cargo().metadata().workspace_packages() {
    let Ok(document) = fs::read(package.manifest_path.as_std_path()) else {
      return fallback("workspace_manifest", "member_manifest_read_unknown");
    };
    let Some(document) = parse_toml(&document) else {
      return fallback("workspace_manifest", "member_manifest_parse_unknown");
    };
    if consumes_workspace_dependency(&document, &dependency_keys)
      || inherits_workspace_package_key(&document, &package_keys)
      || (lints_changed && inherits_workspace_lints(&document))
    {
      packages.insert(package.id.clone());
    }
  }

  SemanticFileChange {
    scope: if packages.is_empty() {
      SemanticScope::None
    } else {
      SemanticScope::Packages(packages)
    },
    input: "workspace_manifest",
    fallback: None,
  }
}

fn lockfile_change(ctx: &WorkspaceContext, before: &[u8], after: &[u8]) -> SemanticFileChange {
  let (Some(before), Some(after)) = (parse_toml(before), parse_toml(after)) else {
    return fallback("lockfile", "lockfile_parse_unknown");
  };
  if before.get("version") != after.get("version") {
    return fallback("lockfile", "lockfile_format_changed");
  }
  let (Some(before_packages), Some(after_packages)) = (lock_packages(&before), lock_packages(&after)) else {
    return fallback("lockfile", "lockfile_package_shape_unknown");
  };
  if before_packages == after_packages {
    return unchanged("lockfile");
  }

  let changed = before_packages
    .keys()
    .chain(after_packages.keys())
    .filter(|key| before_packages.get(*key) != after_packages.get(*key))
    .cloned()
    .collect::<BTreeSet<_>>();
  let current_names = after_packages
    .keys()
    .map(|key| key.name.as_str())
    .collect::<BTreeSet<_>>();
  if changed
    .iter()
    .any(|key| !after_packages.contains_key(key) && !current_names.contains(key.name.as_str()))
  {
    return fallback("lockfile", "removed_resolution_unknown");
  }

  let mut package_ids = BTreeSet::new();
  for key in changed.iter().filter(|key| after_packages.contains_key(*key)) {
    let matches = ctx
      .cargo()
      .metadata()
      .packages
      .iter()
      .filter(|package| lock_key_matches_package(key, package))
      .map(|package| package.id.clone())
      .collect::<Vec<_>>();
    if matches.is_empty() {
      return fallback("lockfile", "resolved_package_unknown");
    }
    package_ids.extend(matches);
  }
  SemanticFileChange {
    scope: SemanticScope::Packages(package_ids),
    input: "lockfile",
    fallback: None,
  }
}

fn parse_toml(bytes: &[u8]) -> Option<Value> {
  std::str::from_utf8(bytes)
    .ok()
    .and_then(|text| toml_edit::de::from_str(text).ok())
}

fn strip_package_metadata(document: &mut Value) {
  if let Some(package) = document.get_mut("package").and_then(Value::as_object_mut) {
    package.remove("metadata");
  }
}

fn changed_table_keys(before: Option<&Value>, after: Option<&Value>) -> BTreeSet<String> {
  let empty = Map::new();
  let before = before.and_then(Value::as_object).unwrap_or(&empty);
  let after = after.and_then(Value::as_object).unwrap_or(&empty);
  before
    .keys()
    .chain(after.keys())
    .filter(|key| before.get(*key) != after.get(*key))
    .cloned()
    .collect()
}

fn consumes_workspace_dependency(document: &Value, changed: &BTreeSet<String>) -> bool {
  if changed.is_empty() {
    return false;
  }
  dependency_tables(document).any(|table| {
    changed.iter().any(|alias| {
      table
        .get(alias)
        .and_then(Value::as_object)
        .and_then(|dependency| dependency.get("workspace"))
        .and_then(Value::as_bool)
        == Some(true)
    })
  })
}

fn dependency_tables(document: &Value) -> impl Iterator<Item = &Map<String, Value>> {
  let root = ["dependencies", "dev-dependencies", "build-dependencies"]
    .into_iter()
    .filter_map(|key| document.get(key).and_then(Value::as_object));
  let targets = document
    .get("target")
    .and_then(Value::as_object)
    .into_iter()
    .flat_map(|targets| targets.values())
    .filter_map(Value::as_object)
    .flat_map(|target| {
      ["dependencies", "dev-dependencies", "build-dependencies"]
        .into_iter()
        .filter_map(|key| target.get(key).and_then(Value::as_object))
    });
  root.chain(targets)
}

fn inherits_workspace_package_key(document: &Value, changed: &BTreeSet<String>) -> bool {
  document
    .get("package")
    .and_then(Value::as_object)
    .is_some_and(|package| {
      changed.iter().any(|key| {
        package
          .get(key)
          .and_then(Value::as_object)
          .and_then(|value| value.get("workspace"))
          .and_then(Value::as_bool)
          == Some(true)
      })
    })
}

fn inherits_workspace_lints(document: &Value) -> bool {
  document
    .get("lints")
    .and_then(Value::as_object)
    .and_then(|lints| lints.get("workspace"))
    .and_then(Value::as_bool)
    == Some(true)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LockKey {
  name: String,
  version: String,
  source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockNode {
  checksum: Option<String>,
  dependencies: BTreeSet<String>,
}

fn lock_packages(document: &Value) -> Option<BTreeMap<LockKey, LockNode>> {
  let packages = document.get("package")?.as_array()?;
  let mut nodes = BTreeMap::new();
  for package in packages {
    let package = package.as_object()?;
    if package.keys().any(|key| {
      !matches!(
        key.as_str(),
        "name" | "version" | "source" | "checksum" | "dependencies"
      )
    }) {
      return None;
    }
    let key = LockKey {
      name: package.get("name")?.as_str()?.to_string(),
      version: package.get("version")?.as_str()?.to_string(),
      source: package.get("source").and_then(Value::as_str).map(ToString::to_string),
    };
    let node = LockNode {
      checksum: package.get("checksum").and_then(Value::as_str).map(ToString::to_string),
      dependencies: package
        .get("dependencies")
        .map(|dependencies| {
          dependencies
            .as_array()?
            .iter()
            .map(|dependency| dependency.as_str().map(ToString::to_string))
            .collect()
        })
        .unwrap_or_else(|| Some(BTreeSet::new()))?,
    };
    if nodes.insert(key, node).is_some() {
      return None;
    }
  }
  Some(nodes)
}

fn lock_key_matches_package(key: &LockKey, package: &Package) -> bool {
  key.name == package.name.as_str()
    && key.version == package.version.to_string()
    && key.source.as_deref() == package.source.as_ref().map(|source| source.repr.as_str())
}

fn unchanged(input: &'static str) -> SemanticFileChange {
  SemanticFileChange {
    scope: SemanticScope::None,
    input,
    fallback: None,
  }
}

fn fallback(input: &'static str, reason: &'static str) -> SemanticFileChange {
  SemanticFileChange {
    scope: SemanticScope::Workspace,
    input,
    fallback: Some(reason),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lock_checksum_diff_maps_to_exact_resolved_package_id() {
    let ctx =
      WorkspaceContext::build(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).expect("workspace context should load");
    let package = ctx
      .cargo()
      .metadata()
      .packages
      .iter()
      .find(|package| package.name.as_str() == "serde" && package.source.is_some())
      .expect("workspace resolution should contain registry serde");
    let source = package.source.as_ref().expect("serde should have a registry source");
    let lock = |checksum: &str| {
      format!(
        "version = 4\n\n[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{}\"\nchecksum = \"{}\"\n",
        package.name, package.version, source.repr, checksum
      )
    };

    let change = lockfile_change(&ctx, lock("a").as_bytes(), lock("b").as_bytes());
    let SemanticScope::Packages(packages) = change.scope else {
      panic!("checksum-only change should localize to resolved packages");
    };
    assert_eq!(packages, BTreeSet::from([package.id.clone()]));
    assert_eq!(change.fallback, None);
  }

  #[test]
  fn lock_dependency_diff_maps_to_exact_resolved_package_id() {
    let ctx =
      WorkspaceContext::build(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).expect("workspace context should load");
    let workspace_packages = ctx.cargo().metadata().workspace_packages();
    let package = workspace_packages
      .iter()
      .find(|package| package.name.as_str() == "cargo-rail")
      .expect("workspace resolution should contain cargo-rail");
    let lock = |dependency: &str| {
      format!(
        "version = 4\n\n[[package]]\nname = \"{}\"\nversion = \"{}\"\ndependencies = [\"{}\"]\n",
        package.name, package.version, dependency
      )
    };

    let change = lockfile_change(&ctx, lock("serde").as_bytes(), lock("serde_json").as_bytes());
    let SemanticScope::Packages(packages) = change.scope else {
      panic!("dependency-edge change should localize to the resolved package");
    };
    assert_eq!(packages, BTreeSet::from([package.id.clone()]));
    assert_eq!(change.fallback, None);
  }
}
