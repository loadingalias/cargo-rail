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

fn commit_all(path: &Path, message: &str) -> Result<()> {
  git(path, &["add", "--all"])?;
  git(
    path,
    &[
      "-c",
      "user.name=cargo-rail fixture",
      "-c",
      "user.email=fixture@cargo-rail.invalid",
      "commit",
      "--quiet",
      "-m",
      message,
    ],
  )?;
  Ok(())
}

fn configure_development_chain(path: &Path, member_count: usize) -> Result<()> {
  for index in 0..member_count {
    let member = format!("member-{index:04}");
    let crate_dir = path.join("crates").join(&member);
    let dependency = if index == 0 {
      String::new()
    } else {
      let previous = format!("member-{:04}", index - 1);
      format!("{previous} = {{ path = \"../{previous}\" }}\n")
    };
    std::fs::write(
      crate_dir.join("Cargo.toml"),
      format!(
        "[package]\nname = \"{member}\"\nversion.workspace = true\nedition.workspace = true\n\
         rust-version.workspace = true\nlicense.workspace = true\npublish = false\n\n[dev-dependencies]\n{dependency}"
      ),
    )?;
    std::fs::write(crate_dir.join("src/lib.rs"), "pub fn value() -> usize { 0 }\n")?;
    if index > 0 {
      let previous = format!("member_{:04}", index - 1);
      let tests = crate_dir.join("tests");
      std::fs::create_dir_all(&tests)?;
      std::fs::write(
        tests.join("dependency.rs"),
        format!("#[test]\nfn observes_dependency() {{ assert_eq!({previous}::value(), 0); }}\n"),
      )?;
    }
  }
  let lockfile = Command::new("cargo")
    .args(["generate-lockfile", "--offline", "--quiet"])
    .current_dir(path)
    .output()?;
  ensure!(
    lockfile.status.success(),
    "development fixture lockfile failed: {}",
    String::from_utf8_lossy(&lockfile.stderr)
  );
  commit_all(path, "Configure development dependency chain")?;
  std::fs::write(
    path.join("crates/member-0000/src/lib.rs"),
    "pub fn value() -> usize { 1 }\n",
  )?;
  commit_all(path, "Mutate development dependency")?;
  Ok(())
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
      plan["impact"]["build_transitive_crates"],
      serde_json::json!(expected_transitive)
    );
    assert_eq!(plan["impact"]["development_transitive_crates"], serde_json::json!([]));
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

#[test]
fn semantic_impact_narrows_100_and_1000_member_mutations_without_missing_the_witness() -> Result<()> {
  let root = TempDir::new()?;

  for member_count in [100, 1_000] {
    let fixture = root.path().join(format!("development-{member_count}"));
    generate_fixture(member_count, &fixture)?;
    configure_development_chain(&fixture, member_count)?;

    let output = run_cargo_rail(&fixture, &["rail", "plan", "--since", "HEAD~1", "--format", "json"])?;
    ensure!(
      output.status.success(),
      "semantic plan failed for {member_count} members: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
      plan["surfaces"]["build"]["scope"]["crates"],
      serde_json::json!(["member-0000"])
    );
    assert_eq!(
      plan["surfaces"]["test"]["scope"]["crates"],
      serde_json::json!(["member-0000", "member-0001"])
    );
    assert_eq!(
      plan["surfaces"]["bench"]["scope"]["crates"],
      serde_json::json!(["member-0001"])
    );

    let witness = Command::new("cargo")
      .args([
        "test",
        "--offline",
        "--locked",
        "--quiet",
        "-p",
        "member-0000",
        "-p",
        "member-0001",
      ])
      .current_dir(&fixture)
      .output()?;
    assert!(
      !witness.status.success(),
      "the selected test closure must catch the member-0000 mutation"
    );
    let witness_output = format!(
      "{}{}",
      String::from_utf8_lossy(&witness.stdout),
      String::from_utf8_lossy(&witness.stderr)
    );
    assert!(
      witness_output.contains("observes_dependency"),
      "the mutation witness must fail in the selected development dependent: {witness_output}"
    );
  }

  Ok(())
}
