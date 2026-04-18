use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

struct SmokeCase<'a> {
  name: &'a str,
  args: &'a [&'a str],
  readme_snippet: Option<&'a str>,
  justfile_snippet: Option<&'a str>,
}

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
  let cases = [
    SmokeCase {
      name: "README plan",
      args: &["rail", "plan", "--merge-base"],
      readme_snippet: Some("cargo rail plan --merge-base"),
      justfile_snippet: None,
    },
    SmokeCase {
      name: "README run",
      args: &["rail", "run", "--merge-base", "--profile", "ci", "--dry-run"],
      readme_snippet: Some("cargo rail run --merge-base --profile ci"),
      justfile_snippet: None,
    },
    SmokeCase {
      name: "README unify",
      args: &["rail", "unify", "--check"],
      readme_snippet: Some("cargo rail unify --check"),
      justfile_snippet: None,
    },
    SmokeCase {
      name: "just build plan",
      args: &["rail", "plan", "--merge-base", "--explain"],
      readme_snippet: None,
      justfile_snippet: Some("rail plan --merge-base --explain"),
    },
    SmokeCase {
      name: "just build run",
      args: &["rail", "run", "--merge-base", "--surface", "build", "--dry-run"],
      readme_snippet: None,
      justfile_snippet: Some("rail run --merge-base --surface build"),
    },
    SmokeCase {
      name: "just build-release",
      args: &[
        "rail",
        "run",
        "--merge-base",
        "--surface",
        "build",
        "--dry-run",
        "--",
        "--release",
      ],
      readme_snippet: None,
      justfile_snippet: Some("rail run --merge-base --surface build -- --release"),
    },
    SmokeCase {
      name: "just ci-build",
      args: &["rail", "run", "--since", "HEAD~1", "--surface", "build", "--dry-run"],
      readme_snippet: None,
      justfile_snippet: Some("rail run --since \"${RAIL_SINCE:-HEAD~1}\" --surface build"),
    },
    SmokeCase {
      name: "just plan",
      args: &["rail", "plan", "--merge-base", "-f", "json"],
      readme_snippet: None,
      justfile_snippet: Some("rail plan --merge-base -f json"),
    },
    SmokeCase {
      name: "just dry-run",
      args: &[
        "rail",
        "run",
        "--merge-base",
        "--surface",
        "test",
        "--dry-run",
        "--print-cmd",
        "--explain",
      ],
      readme_snippet: None,
      justfile_snippet: Some("rail run --merge-base --surface {{ surface }} --dry-run --print-cmd --explain"),
    },
  ];

  for case in cases {
    if let Some(snippet) = case.readme_snippet {
      assert!(
        readme.contains(snippet),
        "{} should stay documented in README.\nmissing snippet: {}",
        case.name,
        snippet
      );
    }
    if let Some(snippet) = case.justfile_snippet {
      assert!(
        justfile.contains(snippet),
        "{} should stay wired through justfile.\nmissing snippet: {}",
        case.name,
        snippet
      );
    }

    let output = run_cargo_rail(&ws.path, case.args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
      output.status.success(),
      "{} should succeed.\nargs: {:?}\nstdout:\n{}\nstderr:\n{}",
      case.name,
      case.args,
      stdout,
      stderr
    );
  }

  Ok(())
}
