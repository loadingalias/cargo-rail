use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use cargo_rail::cargo::ResolutionRequest;
use cargo_rail::source::SourceSnapshot;
use cargo_rail::workspace::WorkspaceContext;

use crate::helpers::{TestWorkspace, git};

fn generate_lockfile(root: &Path) -> Result<()> {
  let output = Command::new("cargo")
    .current_dir(root)
    .args(["generate-lockfile", "--offline"])
    .output()
    .context("failed to generate fixture lockfile")?;
  anyhow::ensure!(
    output.status.success(),
    "fixture lockfile generation failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

#[test]
fn workspace_snapshot_captures_exact_inputs_and_reuses_the_base_resolution() -> Result<()> {
  let workspace = TestWorkspace::new_named("workspace-snapshot")?;
  workspace.add_crate("member", "0.1.0", &[])?;
  let rail_config = "# retained snapshot comment\ntargets = []\n";
  std::fs::write(workspace.path.join(".config/rail.toml"), rail_config)?;
  generate_lockfile(&workspace.path)?;
  git(&workspace.path, &["rm", "--cached", ".config/rail.toml"])?;
  std::fs::write(workspace.path.join(".gitignore"), "target/\n.config/rail.toml\n")?;
  workspace.commit("Add snapshot inputs")?;

  let context = WorkspaceContext::build_with_snapshot(&workspace.path)?;
  let snapshot = context.snapshot()?;

  assert!(matches!(snapshot.source(), SourceSnapshot::GitBacked(_)));
  assert_eq!(
    snapshot
      .manifests()
      .iter()
      .map(|manifest| manifest.path().as_str())
      .collect::<Vec<_>>(),
    ["Cargo.toml", "crates/member/Cargo.toml"]
  );
  assert_eq!(
    snapshot
      .manifests()
      .iter()
      .find(|manifest| manifest.path().as_str() == "crates/member/Cargo.toml")
      .expect("member manifest should be captured")
      .bytes(),
    std::fs::read(workspace.path.join("crates/member/Cargo.toml"))?
  );
  assert_eq!(
    snapshot
      .lockfile()
      .expect("generated lockfile should be captured")
      .file()
      .bytes(),
    std::fs::read(workspace.path.join("Cargo.lock"))?
  );
  assert!(
    snapshot
      .lockfile()
      .expect("generated lockfile should be captured")
      .packages()
      .iter()
      .any(|package| package.name() == "member" && package.version() == "0.1.0")
  );
  assert_eq!(
    snapshot
      .rail_config()
      .expect("rail config should be captured losslessly")
      .bytes(),
    rail_config.as_bytes()
  );
  assert!(
    snapshot
      .source()
      .tree()
      .entries()
      .iter()
      .all(|entry| entry.path.as_str() != ".config/rail.toml"),
    "ignored configuration must be an explicit snapshot input, not canonical source"
  );

  let member = snapshot
    .packages()
    .iter()
    .find(|package| package.name() == "member")
    .expect("workspace package identity should be captured");
  assert!(member.is_workspace_member());
  assert_eq!(member.package_root(), Some(Path::new("crates/member")));
  assert_eq!(
    member.manifest_path().map(|path| path.as_str()),
    Some("crates/member/Cargo.toml")
  );
  assert!(
    member.source().is_none(),
    "workspace path packages have no Cargo source"
  );

  let base = context.resolution_view(ResolutionRequest::default())?;
  assert!(std::ptr::eq(base.as_ref(), snapshot.base_resolution()));
  assert!(std::ptr::eq(
    snapshot.base_resolution().metadata(),
    context.cargo.metadata()
  ));
  assert!(std::ptr::eq(snapshot.base_resolution().graph(), context.graph.as_ref()));
  assert!(snapshot.toolchain().cargo_verbose_version().starts_with("cargo "));
  assert!(snapshot.toolchain().rustc_verbose_version().starts_with("rustc "));
  assert!(snapshot.toolchain().rustdoc_verbose_version().starts_with("rustdoc "));
  assert_eq!(snapshot.targets().len(), 1);
  assert!(snapshot.targets()[0].is_host());
  assert!(snapshot.targets()[0].is_build_target());
  assert!(snapshot.cargo_config().effective_file_settings().is_object());
  Ok(())
}

#[test]
fn workspace_snapshot_preserves_the_filesystem_backed_cargo_only_path() -> Result<()> {
  let workspace = tempfile::tempdir()?;
  std::fs::create_dir_all(workspace.path().join("src"))?;
  std::fs::write(
    workspace.path().join("Cargo.toml"),
    "[package]\nname = \"filesystem-member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
  )?;
  std::fs::write(workspace.path().join("src/lib.rs"), "pub fn value() {}\n")?;
  std::fs::write(workspace.path().join("rail.toml"), "# exact empty configuration\n")?;
  generate_lockfile(workspace.path())?;

  let context = WorkspaceContext::build_with_snapshot(workspace.path())?;
  let snapshot = context.snapshot()?;

  assert!(matches!(snapshot.source(), SourceSnapshot::FilesystemBacked(_)));
  assert!(!context.has_git());
  assert_eq!(snapshot.manifests()[0].path().as_str(), "Cargo.toml");
  assert_eq!(snapshot.packages()[0].package_root(), Some(Path::new("")));
  assert_eq!(
    snapshot
      .rail_config()
      .expect("root rail config should be captured")
      .bytes(),
    b"# exact empty configuration\n"
  );
  assert!(snapshot.lockfile().is_some());
  Ok(())
}
