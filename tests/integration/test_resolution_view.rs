use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use cargo_rail::cargo::{ResolutionFeatures, ResolutionPackages, ResolutionRequest};
use cargo_rail::workspace::WorkspaceContext;

use crate::helpers::{TestWorkspace, run_cargo_rail};

#[test]
fn resolution_views_are_lazy_exact_and_single_flight_cached() -> Result<()> {
  let workspace = TestWorkspace::new_named("resolution-views")?;
  workspace.add_crate("provider", "0.1.0", &[])?;
  let consumer_a_path = workspace.add_crate("consumer-a", "0.1.0", &[])?;
  let consumer_b_path = workspace.add_crate("consumer-b", "0.1.0", &[])?;
  std::fs::write(
    consumer_a_path.join("Cargo.toml"),
    r#"[package]
name = "consumer-a"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[features]
default = ["use-provider"]
use-provider = ["dep:provider"]

[dependencies]
provider = { path = "../provider", optional = true }

[target.'cfg(windows)'.dependencies]
consumer-b = { path = "../consumer-b", default-features = false }
"#,
  )?;
  std::fs::write(
    consumer_b_path.join("Cargo.toml"),
    r#"[package]
name = "consumer-b"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[features]
default = ["use-provider"]
use-provider = ["dep:provider"]

[dependencies]
provider = { path = "../provider", optional = true }
"#,
  )?;
  workspace.commit("Add resolution view fixture")?;

  let context = WorkspaceContext::build_with_snapshot(&workspace.path)?;
  let provider = context.graph().workspace_package_by_name("provider")?.id.clone();
  let consumer_a = context.graph().workspace_package_by_name("consumer-a")?.id.clone();
  let consumer_b = context.graph().workspace_package_by_name("consumer-b")?.id.clone();

  let base = context.resolution_view(ResolutionRequest::default())?;
  assert!(std::ptr::eq(base.graph(), context.graph()));
  assert_eq!(base.graph().dependency_edges(&consumer_a, &provider).count(), 1);
  assert_eq!(base.graph().dependency_edges(&consumer_b, &provider).count(), 1);

  let selected_packages = ResolutionPackages::Selected(BTreeSet::from([consumer_a.clone()]));
  let no_defaults = ResolutionRequest::new(
    selected_packages.clone(),
    ResolutionFeatures::NoDefaultFeatures,
    Some("x86_64-unknown-linux-gnu".to_string()),
  )?;
  let loaded = std::thread::scope(|scope| {
    let handles: Vec<_> = (0..4)
      .map(|_| {
        let request = no_defaults.clone();
        scope.spawn(|| context.resolution_view(request))
      })
      .collect();
    handles
      .into_iter()
      .map(|handle| handle.join().expect("resolution thread should not panic"))
      .collect::<cargo_rail::RailResult<Vec<_>>>()
  })?;
  let first = &loaded[0];
  assert!(
    loaded.iter().all(|view| Arc::ptr_eq(first, view)),
    "concurrent identical requests must share one loaded view"
  );
  assert_eq!(first.graph().dependency_edges(&consumer_a, &provider).count(), 0);
  assert_eq!(first.graph().dependency_edges(&consumer_b, &provider).count(), 0);
  assert_eq!(first.graph().dependency_edges(&consumer_a, &consumer_b).count(), 0);

  let selected_default = context.resolution_view(ResolutionRequest::new(
    selected_packages.clone(),
    ResolutionFeatures::Default,
    Some("x86_64-unknown-linux-gnu".to_string()),
  )?)?;
  assert_eq!(
    selected_default
      .graph()
      .dependency_edges(&consumer_a, &provider)
      .count(),
    1
  );
  assert_eq!(
    selected_default
      .graph()
      .dependency_edges(&consumer_b, &provider)
      .count(),
    0
  );

  let selected_all = context.resolution_view(ResolutionRequest::new(
    selected_packages.clone(),
    ResolutionFeatures::AllFeatures,
    Some("x86_64-unknown-linux-gnu".to_string()),
  )?)?;
  assert_eq!(selected_all.graph().dependency_edges(&consumer_a, &provider).count(), 1);
  assert_eq!(selected_all.graph().dependency_edges(&consumer_b, &provider).count(), 0);

  let windows = context.resolution_view(ResolutionRequest::new(
    selected_packages.clone(),
    ResolutionFeatures::NoDefaultFeatures,
    Some("x86_64-pc-windows-msvc".to_string()),
  )?)?;
  assert_eq!(windows.graph().dependency_edges(&consumer_a, &consumer_b).count(), 1);
  assert_eq!(windows.graph().dependency_edges(&consumer_b, &provider).count(), 0);

  let selected_features = ResolutionRequest::new(
    selected_packages,
    ResolutionFeatures::Selected(BTreeMap::from([(
      consumer_a.clone(),
      BTreeSet::from(["use-provider".to_string()]),
    )])),
    Some("x86_64-unknown-linux-gnu".to_string()),
  )?;
  let selected = context.resolution_view(selected_features)?;
  assert_eq!(selected.graph().dependency_edges(&consumer_a, &provider).count(), 1);
  assert_eq!(selected.graph().dependency_edges(&consumer_b, &provider).count(), 0);
  assert_eq!(selected.graph().dependency_edges(&consumer_a, &consumer_b).count(), 0);
  Ok(())
}

#[test]
fn native_default_resolution_remains_available_without_snapshot_capture() -> Result<()> {
  let workspace = TestWorkspace::new_named("native-resolution-fast-path")?;
  workspace.add_crate("member", "0.1.0", &[])?;
  workspace.commit("Add native resolution member")?;

  let context = WorkspaceContext::build(&workspace.path)?;
  let first = context.resolution_view(ResolutionRequest::default())?;
  let second = context.resolution_view(ResolutionRequest::default())?;

  assert!(Arc::ptr_eq(&first, &second));
  assert!(std::ptr::eq(first.metadata(), context.cargo().metadata()));
  assert!(std::ptr::eq(first.graph(), context.graph()));
  assert!(context.snapshot_id().is_none());
  Ok(())
}

#[test]
fn resolution_view_rejects_inexact_package_and_credential_url_identity() -> Result<()> {
  let workspace = TestWorkspace::new_named("resolution-view-fail-closed")?;
  workspace.add_crate("member", "0.1.0", &[])?;
  std::fs::create_dir_all(workspace.path.join(".cargo"))?;
  std::fs::write(
    workspace.path.join(".cargo/config.toml"),
    "[registries.private]\nindex = \"https://user:password@example.invalid/index\"\n",
  )?;
  std::fs::write(
    workspace.path.join(".config/rail.toml"),
    "targets = [\"x86_64-unknown-linux-gnu\"]\n\n[unify]\ndetect_unused = false\ndetect_undeclared_features = false\nmsrv = false\n",
  )?;
  workspace.commit("Add fail-closed resolution fixture")?;

  let output = run_cargo_rail(&workspace.path, &["rail", "unify", "--check", "--format", "json"])?;
  assert_eq!(output.status.code(), Some(2));
  let combined = format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(combined.contains("credentials in URL-valued setting"), "{combined}");

  let context = WorkspaceContext::build_with_snapshot(&workspace.path)?;
  let member = context.graph().workspace_package_by_name("member")?.id.clone();
  let unknown = cargo_metadata::PackageId {
    repr: "path+file:///missing#unknown@0.1.0".to_string(),
  };
  let error = match ResolutionRequest::new(
    ResolutionPackages::Selected(BTreeSet::from([unknown])),
    ResolutionFeatures::NoDefaultFeatures,
    Some("x86_64-unknown-linux-gnu".to_string()),
  )
  .and_then(|request| context.resolution_view(request))
  {
    Ok(_) => anyhow::bail!("unknown exact package IDs must fail closed"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("is not an exact workspace member"));
  assert!(context.graph().package(&member).is_some());
  Ok(())
}
