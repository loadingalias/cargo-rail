//! Integration tests for `cargo rail affected` command

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::Result;

#[test]
fn test_affected_basic() -> Result<()> {
  // Setup workspace with two crates
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add lib-a and lib-b")?;

  // Create a baseline (origin/main)
  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> &'static str { \"Modified\" }")?;
  ws.commit("Modify lib-a")?;

  // Run affected command
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--since", "origin/main"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should show lib-a as directly affected and lib-b as dependent
  assert!(stdout.contains("lib-a"), "lib-a should be affected");
  assert!(stdout.contains("lib-b"), "lib-b should be in dependents");

  Ok(())
}

#[test]
fn test_affected_no_changes() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  // Create baseline
  git(&ws.path, &["branch", "origin/main"])?;

  // Run affected with no changes
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--since", "origin/main"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should indicate no changes
  assert!(
    stdout.contains("changed files: 0") || stdout.contains("test targets: 0"),
    "Should indicate no changes, got: {}",
    stdout
  );

  Ok(())
}

#[test]
fn test_affected_json_output() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.commit("Add lib-a")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "README.md", "# Modified\n")?;
  ws.commit("Modify lib-a README")?;

  // Run with --format json
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "affected", "--since", "origin/main", "--format", "json"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be valid JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.is_object(), "Output should be JSON object");

  Ok(())
}

#[test]
fn test_affected_names_only() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  ws.add_crate("lib-b", "0.1.0", &[("lib-a", r#"{ path = "../lib-a" }"#)])?;
  ws.commit("Add crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Modify lib-a
  ws.modify_file("lib-a", "src/lib.rs", "pub fn hello() -> &'static str { \"Changed\" }")?;
  ws.commit("Change lib-a")?;

  // Run with --format names
  let output = run_cargo_rail(
    &ws.path,
    &["rail", "affected", "--since", "origin/main", "--format", "names"],
  )?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  // Should be simple list of names
  let lines: Vec<&str> = stdout.trim().lines().collect();
  assert!(lines.contains(&"lib-a"), "Should contain lib-a");
  assert!(lines.contains(&"lib-b"), "Should contain lib-b");

  Ok(())
}

#[test]
fn test_affected_sha_pair_mode() -> Result<()> {
  // Setup workspace
  let ws = TestWorkspace::new()?;
  ws.add_crate("lib-a", "0.1.0", &[])?;
  let sha1 = ws.commit("Add lib-a")?;

  // Make a change (source file, not docs-only)
  ws.modify_file(
    "lib-a",
    "src/lib.rs",
    "pub fn updated() -> &'static str { \"updated\" }",
  )?;
  let sha2 = ws.commit("Update lib-a")?;

  // Run with --from/--to
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--from", &sha1, "--to", &sha2])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(stdout.contains("lib-a"), "lib-a should be affected");

  Ok(())
}

/// Test affected --all flag shows all workspace crates regardless of changes
#[test]
fn test_affected_all_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-all")?;
  ws.add_crate("crate-a", "0.1.0", &[])?;
  ws.add_crate("crate-b", "0.2.0", &[])?;
  ws.add_crate("crate-c", "0.3.0", &[])?;
  ws.commit("Add three crates")?;

  git(&ws.path, &["branch", "origin/main"])?;

  // Run with --all flag (ignore changes, show all crates)
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "affected --all should succeed");
  assert!(stdout.contains("crate-a"), "Should show crate-a");
  assert!(stdout.contains("crate-b"), "Should show crate-b");
  assert!(stdout.contains("crate-c"), "Should show crate-c");

  Ok(())
}

/// Test affected --all with --format json
#[test]
fn test_affected_all_json() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-all-json")?;
  ws.add_crate("json-a", "0.1.0", &[])?;
  ws.add_crate("json-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  // Run with --all and --format json
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all", "--format", "json"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "affected --all --format json should succeed");

  // Parse JSON
  let json: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
  assert!(json.get("crates").is_some(), "Should have crates array");
  assert!(json.get("count").is_some(), "Should have count");

  let count = json["count"].as_u64().unwrap();
  assert_eq!(count, 2, "Should have 2 crates");

  Ok(())
}

/// Test affected --all with --format names-only
#[test]
fn test_affected_all_names_only() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-all-names")?;
  ws.add_crate("names-a", "0.1.0", &[])?;
  ws.add_crate("names-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  // Run with --all and --format names-only
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all", "--format", "names-only"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    output.status.success(),
    "affected --all --format names-only should succeed"
  );

  let lines: Vec<&str> = stdout.trim().lines().collect();
  assert!(lines.contains(&"names-a"), "Should contain names-a");
  assert!(lines.contains(&"names-b"), "Should contain names-b");

  Ok(())
}

/// Test affected --output flag writes to file
#[test]
fn test_affected_output_flag() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-output")?;
  ws.add_crate("out-a", "0.1.0", &[])?;
  ws.add_crate("out-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  let output_file = ws.path.join("affected-output.txt");

  // Run with --all and --output
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "affected",
      "--all",
      "--format",
      "names-only",
      "--output",
      output_file.to_str().unwrap(),
    ],
  )?;

  assert!(output.status.success(), "affected --output should succeed");
  assert!(output_file.exists(), "Output file should be created");

  let content = std::fs::read_to_string(&output_file)?;
  assert!(content.contains("out-a"), "File should contain out-a");
  assert!(content.contains("out-b"), "File should contain out-b");

  Ok(())
}

/// Test affected --format github (GitHub Actions output format)
#[test]
fn test_affected_format_github() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-github")?;
  ws.add_crate("gh-a", "0.1.0", &[])?;
  ws.add_crate("gh-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  // Run with --all and --format github
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all", "--format", "github"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "affected --format github should succeed");

  // GitHub Actions format uses key=value pairs
  assert!(
    stdout.contains("affected=") || stdout.contains("crates="),
    "Should have GitHub Actions output format, got: {}",
    stdout
  );

  Ok(())
}

/// Test affected --format github-matrix (GitHub Actions matrix format)
#[test]
fn test_affected_format_github_matrix() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-ghmatrix")?;
  ws.add_crate("mx-a", "0.1.0", &[])?;
  ws.add_crate("mx-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  // Run with --all and --format github-matrix
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all", "--format", "github-matrix"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    output.status.success(),
    "affected --format github-matrix should succeed"
  );

  // GitHub matrix format should be JSON with matrix structure
  assert!(
    stdout.contains("matrix=") || stdout.contains("["),
    "Should have GitHub Actions matrix format, got: {}",
    stdout
  );

  Ok(())
}

/// Test affected --format jsonl (JSON Lines format)
#[test]
fn test_affected_format_jsonl() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-jsonl")?;
  ws.add_crate("jl-a", "0.1.0", &[])?;
  ws.add_crate("jl-b", "0.2.0", &[])?;
  ws.commit("Add crates")?;

  // Run with --all and --format jsonl
  let output = run_cargo_rail(&ws.path, &["rail", "affected", "--all", "--format", "jsonl"])?;
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(output.status.success(), "affected --format jsonl should succeed");

  // JSONL format should have one JSON object per line
  let lines: Vec<&str> = stdout.trim().lines().collect();
  assert!(lines.len() >= 2, "Should have at least 2 lines for 2 crates");

  // Each line should be valid JSON
  for line in &lines {
    let _: serde_json::Value =
      serde_json::from_str(line).unwrap_or_else(|_| panic!("Each line should be valid JSON, got: {}", line));
  }

  Ok(())
}

/// Test affected with short flags (-a, -f, -o)
#[test]
fn test_affected_short_flags() -> Result<()> {
  let ws = TestWorkspace::new_named("affected-short")?;
  ws.add_crate("short-a", "0.1.0", &[])?;
  ws.commit("Add crate")?;

  let output_file = ws.path.join("short-output.txt");

  // Run with short flags: -a (all), -f (format), -o (output)
  let output = run_cargo_rail(
    &ws.path,
    &[
      "rail",
      "affected",
      "-a",
      "-f",
      "names-only",
      "-o",
      output_file.to_str().unwrap(),
    ],
  )?;

  assert!(output.status.success(), "affected with short flags should succeed");
  assert!(output_file.exists(), "Output file should be created");

  let content = std::fs::read_to_string(&output_file)?;
  assert!(content.contains("short-a"), "File should contain short-a");

  Ok(())
}
