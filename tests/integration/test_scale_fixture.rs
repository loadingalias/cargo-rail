use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use cargo_metadata::MetadataCommand;
use tempfile::TempDir;

use crate::helpers::{git, run_cargo_rail};

const MEMBER_COUNTS: [usize; 4] = [1, 10, 100, 1_000];

fn generate_fixture(member_count: usize, destination: &Path) -> Result<()> {
  let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/generate-workspace.sh");
  let output = Command::new("bash")
    .arg(script)
    .arg(member_count.to_string())
    .arg(destination)
    .output()
    .context("running scale fixture generator")?;

  ensure!(
    output.status.success(),
    "fixture generation failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

fn revision(path: &Path, revision: &str) -> Result<String> {
  let output = git(path, &["rev-parse", revision])?;
  Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[test]
fn scale_fixtures_are_deterministic_and_cargo_valid() -> Result<()> {
  let root = TempDir::new()?;

  for member_count in MEMBER_COUNTS {
    let first = root.path().join(format!("first-{member_count}"));
    let second = root.path().join(format!("second-{member_count}"));
    generate_fixture(member_count, &first)?;
    generate_fixture(member_count, &second)?;

    assert_eq!(revision(&first, "HEAD")?, revision(&second, "HEAD")?);
    assert_eq!(revision(&first, "HEAD^{tree}")?, revision(&second, "HEAD^{tree}")?);
    assert!(git(&first, &["status", "--porcelain"])?.stdout.is_empty());
    assert!(std::fs::read(first.join(".config/rail.toml"))?.is_empty());

    let changed = git(&first, &["diff", "--name-only", "HEAD~1", "HEAD"])?;
    assert_eq!(String::from_utf8(changed.stdout)?, "crates/member-0000/src/lib.rs\n");

    let metadata = MetadataCommand::new()
      .current_dir(&first)
      .no_deps()
      .other_options(vec!["--offline".into(), "--locked".into()])
      .exec()?;
    assert_eq!(metadata.workspace_members.len(), member_count);
    assert_eq!(metadata.packages.len(), member_count);

    for index in 0..member_count {
      let name = format!("member-{index:04}");
      let package = metadata
        .packages
        .iter()
        .find(|package| package.name == name)
        .with_context(|| format!("missing generated package {name}"))?;

      if index == 0 {
        assert!(package.dependencies.is_empty());
      } else {
        assert_eq!(package.dependencies.len(), 1);
        let dependency = &package.dependencies[0];
        let expected_name = format!("member-{:04}", index - 1);
        let expected_path = std::fs::canonicalize(first.join("crates").join(&expected_name))?;
        assert_eq!(dependency.name, expected_name);
        assert_eq!(
          dependency.path.as_ref().map(|path| path.as_std_path()),
          Some(expected_path.as_path())
        );
        assert!(dependency.source.is_none());
      }
    }

    let output = run_cargo_rail(&first, &["rail", "plan", "--since", "HEAD~1", "--format", "json"])?;
    ensure!(
      output.status.success(),
      "cargo-rail rejected the {member_count}-member fixture: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
      output.stderr.is_empty(),
      "cargo-rail warned for the {member_count}-member fixture: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let expected_transitive: Vec<_> = (1..member_count).map(|index| format!("member-{index:04}")).collect();
    assert_eq!(plan["impact"]["direct_crates"], serde_json::json!(["member-0000"]));
    assert_eq!(
      plan["impact"]["transitive_crates"],
      serde_json::json!(expected_transitive)
    );
    assert_eq!(
      plan["impact"]["execution_transitive_crates"],
      plan["impact"]["transitive_crates"]
    );
  }

  let ten_member = root.path().join("first-10");
  let output = Command::new("cargo")
    .args(["check", "--workspace", "--offline", "--locked", "--quiet"])
    .current_dir(ten_member)
    .output()?;
  ensure!(
    output.status.success(),
    "generated workspace failed to compile: {}",
    String::from_utf8_lossy(&output.stderr)
  );

  Ok(())
}

#[test]
fn scale_fixture_generator_refuses_nonempty_destinations() -> Result<()> {
  let root = TempDir::new()?;
  std::fs::write(root.path().join("keep"), "do not replace\n")?;

  let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/generate-workspace.sh");
  let output = Command::new("bash").arg(script).arg("1").arg(root.path()).output()?;

  assert_eq!(output.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&output.stderr).contains("fixture destination must be empty"));
  assert_eq!(std::fs::read_to_string(root.path().join("keep"))?, "do not replace\n");
  Ok(())
}
