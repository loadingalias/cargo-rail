use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

fn setup_frontdoor_workspace(name: &str) -> Result<TestWorkspace> {
  let ws = TestWorkspace::new_named(name)?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;
  git(&ws.path, &["checkout", "-b", "feature/frontdoor-smoke"])?;
  ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
  ws.commit("feat: frontdoor smoke change")?;
  Ok(ws)
}

#[test]
fn test_documented_frontdoor_commands_smoke() -> Result<()> {
  let ws = setup_frontdoor_workspace("frontdoor-smoke")?;
  let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
  let readme = std::fs::read_to_string(repo_root.join("README.md"))?;
  let justfile = std::fs::read_to_string(repo_root.join("justfile"))?;
  let build_script = std::fs::read_to_string(repo_root.join("scripts/build/build.sh"))?;
  let test_script = std::fs::read_to_string(repo_root.join("scripts/test/test.sh"))?;
  assert!(
    justfile.contains("cargo build --workspace --all-targets --all-features --release --locked"),
    "just build-release should produce the complete release artifact set"
  );
  assert!(justfile.contains("@scripts/build/build.sh"));
  assert!(justfile.contains("rail plan --merge-base -f json"));
  assert!(!justfile.contains("rail run"));
  for (script, surface, executor) in [
    (&build_script, "build", "cargo build"),
    (&test_script, "test", "cargo nextest run"),
  ] {
    assert!(script.contains("rail)"));
    assert!(script.contains(&format!(".surfaces.{surface}.scope.cargo_args[]")));
    assert!(script.contains(executor));
    assert!(!script.contains(" rail run"));
  }

  let cases: &[(&str, &[&str], &str)] = &[
    (
      "README plan",
      &["rail", "plan", "--merge-base"],
      "cargo rail plan --merge-base",
    ),
    (
      "README unify",
      &["rail", "unify", "--check"],
      "cargo rail unify --check",
    ),
  ];
  for (name, args, snippet) in cases {
    assert!(
      readme.contains(snippet),
      "{name} should stay documented in README.\nmissing snippet: {snippet}"
    );
    let output = run_cargo_rail(&ws.path, args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
      output.status.success(),
      "{} should succeed.\nargs: {:?}\nstdout:\n{}\nstderr:\n{}",
      name,
      args,
      stdout,
      stderr
    );
  }

  Ok(())
}
