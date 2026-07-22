use std::fs;
use std::process::Command;

use anyhow::{Context, Result};

use crate::helpers::TestWorkspace;

#[test]
fn rustdoc_proxy_preserves_cargo_docs_and_records_dep_info() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("rustdoc-observation", "0.1.0")?;
  fs::write(
    workspace.path.join("src/lib.rs"),
    "mod nested;\npub use nested::value;\n",
  )?;
  fs::write(workspace.path.join("src/nested.rs"), "pub fn value() -> u8 { 1 }\n")?;
  let observation_directory = workspace.path.join("observations");
  let target_directory = workspace.path.join("target-observation");

  let output = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["doc", "--no-deps", "--message-format=json", "--target-dir"])
    .arg(&target_directory)
    .env("RUSTDOC", env!("CARGO_BIN_EXE_cargo-rail"))
    .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
    .env("CARGO_RAIL_INNER_RUSTDOC", "rustdoc")
    .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observation_directory)
    .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", &workspace.path)
    .output()
    .context("run cargo doc through the rustdoc observation proxy")?;
  assert!(
    output.status.success(),
    "cargo doc failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let index = target_directory.join("doc/rustdoc_observation/index.html");
  assert!(
    index.is_file(),
    "rustdoc proxy must preserve HTML output at {}",
    index.display()
  );
  assert!(
    String::from_utf8_lossy(&output.stdout).contains(index.to_string_lossy().as_ref()),
    "Cargo's stable artifact message must retain the documentation index\n{}",
    String::from_utf8_lossy(&output.stdout)
  );

  let records = fs::read_dir(&observation_directory)?
    .map(|entry| -> Result<serde_json::Value> {
      let path = entry?.path();
      Ok(serde_json::from_slice(&fs::read(path)?)?)
    })
    .collect::<Result<Vec<_>>>()?;
  let record = records
    .iter()
    .find(|record| record["crate_name"] == "rustdoc_observation")
    .context("rustdoc crate invocation observation")?;
  assert_eq!(record["mode"], "rustdoc");
  assert_eq!(record["success"], true);
  assert!(
    record["compiler_arguments"]
      .as_array()
      .is_some_and(|arguments| arguments.iter().any(|argument| argument
        .as_str()
        .is_some_and(|argument| argument.starts_with("--emit=") && argument.contains("dep-info"))))
  );
  let observed_paths = record["observed_reads"]
    .as_array()
    .context("observed rustdoc reads")?
    .iter()
    .filter_map(|read| read["path"]["path"].as_str())
    .collect::<Vec<_>>();
  assert!(
    observed_paths.contains(&"src/lib.rs"),
    "crate root missing from {record}"
  );
  assert!(
    observed_paths.contains(&"src/nested.rs"),
    "module source missing from {record}"
  );
  assert!(
    record["emitted_outputs"].as_array().is_some_and(|outputs| outputs
      .iter()
      .any(|output| { output["path"]["path"] == "target-observation/doc/rustdoc_observation.d" })),
    "rustdoc dep-info output missing from {record}"
  );
  assert!(
    record["bypasses"].as_array().is_some_and(|bypasses| bypasses
      .iter()
      .any(|reason| reason == "rustdoc_output_tree_unavailable")),
    "Cargo does not enumerate the complete HTML tree, so reuse must remain disabled: {record}"
  );
  let encoded = serde_json::to_string(record)?;
  let canonical_workspace = fs::canonicalize(&workspace.path)?;
  for root in [&workspace.path, &canonical_workspace] {
    assert!(
      !encoded.contains(root.to_string_lossy().as_ref()),
      "portable compiler argv must not retain checkout root '{}': {record}",
      root.display()
    );
  }

  Ok(())
}
