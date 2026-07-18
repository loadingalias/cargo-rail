use std::path::Path;

use anyhow::Result;
use cargo_rail::workspace::WorkspaceContext;

use crate::helpers::TestWorkspace;

fn write_package(root: &Path, name: &str) -> Result<()> {
  std::fs::create_dir_all(root.join("src"))?;
  std::fs::write(
    root.join("Cargo.toml"),
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition.workspace = true\nlicense.workspace = true\n"),
  )?;
  std::fs::write(root.join("src/lib.rs"), "pub fn value() {}\n")?;
  Ok(())
}

#[test]
fn ownership_uses_the_deepest_exact_package_root_without_filesystem_access() -> Result<()> {
  let workspace = TestWorkspace::new_named("ownership-longest-prefix")?;
  std::fs::write(
    workspace.path.join("Cargo.toml"),
    r#"[workspace]
members = ["crates/outer", "crates/outer/nested"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
"#,
  )?;
  write_package(&workspace.path.join("crates/outer"), "outer")?;
  write_package(&workspace.path.join("crates/outer/nested"), "nested")?;
  workspace.commit("Add nested workspace packages")?;

  let context = WorkspaceContext::build(&workspace.path)?;

  assert_eq!(
    context.graph.file_to_crate(Path::new("crates/outer/src/lib.rs")),
    Some("outer".to_string())
  );
  assert_eq!(
    context.graph.file_to_crate(Path::new("crates/outer/nested/src/lib.rs")),
    Some("nested".to_string())
  );
  assert_eq!(
    context
      .graph
      .file_to_crate(Path::new("crates/outer/nested/../src/deleted.rs")),
    Some("outer".to_string()),
    "lexical parent components must be normalized before longest-prefix lookup"
  );

  let deleted_absolute = workspace.path.join("crates/outer/nested/src/deleted.rs");
  assert!(!deleted_absolute.exists());
  assert_eq!(
    context.graph.file_to_crate(&deleted_absolute),
    Some("nested".to_string()),
    "ownership must not require the queried path to exist"
  );
  assert_eq!(
    context.graph.file_to_crate(&workspace.path.join("../outside.rs")),
    None,
    "absolute paths escaping the workspace must not acquire an owner"
  );
  assert_eq!(
    context.graph.file_to_crate(Path::new("crates/outerish/src/lib.rs")),
    None,
    "prefixes must end at path-component boundaries"
  );
  assert_eq!(
    context
      .graph
      .file_to_crate(Path::new("crates/outer/nested/target/generated.rs")),
    None,
    "ignored generated directories must remain unowned"
  );
  Ok(())
}

#[test]
fn ownership_maps_root_package_files() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("root-package", "0.1.0")?;
  let context = WorkspaceContext::build(&workspace.path)?;

  assert_eq!(
    context.graph.file_to_crate(Path::new("src/deleted.rs")),
    Some("root-package".to_string())
  );
  assert_eq!(
    context.graph.file_to_crate(Path::new("Cargo.toml")),
    Some("root-package".to_string())
  );
  Ok(())
}

#[test]
fn ownership_accepts_exact_absolute_roots_for_external_workspace_members() -> Result<()> {
  let root = tempfile::tempdir()?;
  let workspace_root = root.path().join("workspace");
  let external_root = root.path().join("external-member");
  std::fs::create_dir_all(&workspace_root)?;
  std::fs::write(
    workspace_root.join("Cargo.toml"),
    "[workspace]\nmembers = [\"../external-member\"]\nresolver = \"2\"\n",
  )?;
  std::fs::create_dir_all(external_root.join("src"))?;
  std::fs::write(
    external_root.join("Cargo.toml"),
    "[package]\nname = \"external-member\"\nversion = \"0.1.0\"\nedition = \"2024\"\nworkspace = \"../workspace\"\n",
  )?;
  std::fs::write(external_root.join("src/lib.rs"), "pub fn value() {}\n")?;

  let context = WorkspaceContext::build(&workspace_root)?;
  assert_eq!(
    context.graph.file_to_crate(&external_root.join("src/deleted.rs")),
    Some("external-member".to_string())
  );
  assert_eq!(
    context.graph.file_to_crate(Path::new("../external-member/src/lib.rs")),
    None,
    "workspace-relative inputs cannot escape the workspace root"
  );
  Ok(())
}
