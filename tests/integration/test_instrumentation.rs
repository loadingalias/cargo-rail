use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;

use crate::helpers::{TestWorkspace, run_cargo_rail};

fn read_counters(path: &Path) -> Result<serde_json::Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading diagnostics from {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn host_target() -> Result<String> {
    let output = Command::new("rustc").arg("-vV").output()?;
    ensure!(output.status.success(), "rustc -vV failed");
    String::from_utf8(output.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .context("rustc -vV did not report a host target")
}

#[test]
fn plan_diagnostics_are_out_of_band_and_count_real_boundaries() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("diagnostic-plan")?;
        ws.add_crate("member-a", "0.1.0", &[])?;
        ws.add_crate("member-b", "0.1.0", &[("member-a", "{ path = \"../member-a\" }")])?;
        let lockfile = Command::new("cargo")
            .current_dir(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(
            lockfile.status.success(),
            "offline lockfile generation failed: {}",
            String::from_utf8_lossy(&lockfile.stderr)
        );
        ws.commit("Add members")?;
        ws.modify_file("member-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("Change member-a")?;

        let args = ["rail", "plan", "--since", "HEAD~1", "--format", "json"];
        let expected = run_cargo_rail(&ws.path, &args)?;
        ensure!(expected.status.success(), "warm-up plan failed");

        let output_dir = TempDir::new()?;
        let diagnostics = output_dir.path().join("plan.json");
        let measured = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "plan",
                "--since",
                "HEAD~1",
                "--format",
                "json",
            ],
        )?;

        assert_eq!(measured.status, expected.status);
        assert_eq!(measured.stdout, expected.stdout, "diagnostics changed normal stdout");
        assert_eq!(measured.stderr, expected.stderr, "diagnostics changed normal stderr");

        let counters = read_counters(&diagnostics)?;
        assert_eq!(counters["schema_version"], 12);
        assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
        assert!(
            counters["phases"]["cli_pre_context_preparation"]["elapsed_ns"]
                .as_u64()
                .is_some_and(|elapsed| elapsed > 0)
        );
        assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 1);
        assert!(
            counters["phases"]["workspace_capture_cargo_metadata"]["elapsed_ns"]
                .as_u64()
                .is_some_and(|elapsed| elapsed > 0)
        );
        assert_eq!(counters["phases"]["sysroot_fingerprinting"]["invocations"], 0);
        assert_eq!(counters["phases"]["sysroot_fingerprinting"]["elapsed_ns"], 0);
        assert!(
            counters["snapshot_id"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("v1-sha256-")),
            "current plan commands must expose one versioned authoritative identity"
        );
        assert_eq!(counters["cargo_metadata_loads"], 1);
        assert_eq!(counters["cargo_metadata_cache_hits"], 0);
        assert_eq!(counters["target_view_loads"], 0);
        assert!(counters["hash_operations"].as_u64().is_some_and(|count| count >= 3));
        assert!(counters["hash_input_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
        assert!(
            counters["hashed_file_bytes_read"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
        );
        assert_eq!(counters["git_subprocesses"], 11);
        assert_eq!(counters["graph_traversals"], 1);
        assert!(counters["graph_node_visits"].as_u64().is_some_and(|count| count >= 2));
        assert!(counters["graph_edge_visits"].as_u64().is_some_and(|count| count >= 1));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn split_diagnostics_prove_bounded_git_object_streams() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("diagnostic-split-streams")?;
        ws.add_crate("streamed", "0.1.0", &[])?;
        ws.commit("Add streamed crate")?;
        for revision in 1..=8 {
            ws.modify_file(
                "streamed",
                "src/lib.rs",
                &format!("pub const REVISION: u8 = {revision};\n"),
            )?;
            ws.commit(&format!("Streamed revision {revision}"))?;
        }
        let target = TempDir::new()?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[workspace]\nroot = \".\"\n\n[crates.streamed.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
                target.path().display().to_string().replace('\\', "\\\\")
            ),
        )?;
        let output = TempDir::new()?;
        let diagnostics = output.path().join("split.json");
        let measured = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "split",
                "run",
                "streamed",
                "--yes",
                "--allow-dirty",
            ],
        )?;
        ensure!(
            measured.status.success(),
            "measured split failed: {}",
            String::from_utf8_lossy(&measured.stderr)
        );
        let counters = read_counters(&diagnostics)?;
        let objects = counters["git_object_reads"].as_u64().context("missing object count")?;
        let batches = counters["git_object_read_batches"]
            .as_u64()
            .context("missing object batch count")?;
        let subprocesses = counters["git_subprocesses"]
            .as_u64()
            .context("missing Git subprocess count")?;
        ensure!(objects > 0 && batches > 0);
        assert!(
            batches < objects,
            "{objects} object reads used {batches} batches; per-object subprocess behavior regressed"
        );
        assert!(objects <= 24, "bounded split object-read baseline regressed: {objects}");
        assert!(batches <= 4, "bounded split stream baseline regressed: {batches}");
        assert!(
            subprocesses <= 170,
            "bounded split Git subprocess baseline regressed: {subprocesses}"
        );
        eprintln!(
            "P5 split measurement: git_subprocesses={subprocesses}, object_reads={objects}, object_batches={batches}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn snapshot_identity_records_credential_capability_not_raw_token_material() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("diagnostic-credential-capability")?;
        ws.add_crate("member", "0.1.0", &[])?;
        let cargo_home = TempDir::new()?;
        let cargo_config_dir = ws.path.join(".cargo");
        std::fs::create_dir_all(&cargo_config_dir)?;
        let credential_config = cargo_config_dir.join("config.toml");
        std::fs::write(
            &credential_config,
            "[registries.private]\ntoken = \"first-private-token\"\n",
        )?;
        let lockfile = Command::new("cargo")
            .current_dir(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(lockfile.status.success(), "offline lockfile generation failed");
        ws.commit("Add credential capability fixture")?;

        let output_dir = TempDir::new()?;
        let first_diagnostics = output_dir.path().join("first.json");
        let second_diagnostics = output_dir.path().join("second.json");
        let run = |diagnostics: &Path| -> Result<std::process::Output> {
            Ok(Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
                .current_dir(&ws.path)
                .env("CARGO_HOME", cargo_home.path())
                .args([
                    "rail",
                    "--diagnostics-file",
                    diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                    "plan",
                    "--since",
                    "HEAD",
                    "--format",
                    "json",
                ])
                .output()?)
        };

        let first = run(&first_diagnostics)?;
        ensure!(
            first.status.success(),
            "first capability plan failed: stdout={} stderr={}",
            String::from_utf8_lossy(&first.stdout),
            String::from_utf8_lossy(&first.stderr)
        );
        std::fs::write(
            &credential_config,
            "[registries.private]\ntoken = \"different-private-token\"\n",
        )?;
        let second = run(&second_diagnostics)?;
        ensure!(
            second.status.success(),
            "second capability plan failed: stdout={} stderr={}",
            String::from_utf8_lossy(&second.stdout),
            String::from_utf8_lossy(&second.stderr)
        );

        let first_counters = read_counters(&first_diagnostics)?;
        let second_counters = read_counters(&second_diagnostics)?;
        assert_eq!(first_counters["snapshot_id"], second_counters["snapshot_id"]);
        for bytes in [
            first.stdout,
            first.stderr,
            second.stdout,
            second.stderr,
            std::fs::read(first_diagnostics)?,
            std::fs::read(second_diagnostics)?,
        ] {
            let rendered = String::from_utf8_lossy(&bytes);
            assert!(!rendered.contains("private-token"), "raw credential escaped capture");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unify_diagnostics_distinguish_base_and_target_metadata_loads() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("diagnostic-target-metadata")?;
        ws.add_crate("demo", "0.1.0", &[])?;
        std::fs::write(
            ws.path.join(".config/rail.toml"),
            format!("targets = [\"{}\"]\n\n[unify]\nmsrv = false\n", host_target()?),
        )?;
        ws.commit("Add target fixture")?;

        let output_dir = TempDir::new()?;
        let diagnostics = output_dir.path().join("unify.json");
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "unify",
                "--check",
                "--format",
                "json",
            ],
        )?;
        ensure!(
            output.status.success() || output.status.code() == Some(1),
            "unify diagnostics failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let counters = read_counters(&diagnostics)?;
        assert_eq!(counters["cargo_metadata_loads"], 2);
        assert_eq!(counters["cargo_metadata_cache_hits"], 0);
        assert_eq!(counters["target_view_loads"], 1);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn diagnostics_refuse_to_replace_existing_files_before_dispatch() {
    let result: Result<()> = (|| {
        let root = TempDir::new()?;
        let diagnostics = root.path().join("existing.json");
        std::fs::write(&diagnostics, "keep\n")?;

        let output = run_cargo_rail(
            root.path(),
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "plan",
                "--schema",
            ],
        )?;

        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("failed to reserve diagnostic counter file"));
        assert_eq!(std::fs::read_to_string(diagnostics)?, "keep\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn pre_context_diagnostics_have_one_fixed_phase_schema() {
    let result: Result<()> = (|| {
        let output = TempDir::new()?;
        let diagnostics = output.path().join("schema.json");
        let measured = run_cargo_rail(
            output.path(),
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "plan",
                "--schema",
            ],
        )?;
        ensure!(measured.status.success(), "schema output failed");

        let counters = read_counters(&diagnostics)?;
        assert_eq!(counters["schema_version"], 12);
        assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
        assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
        assert_eq!(
            counters["phases"].as_object().map(serde_json::Map::len),
            Some(3),
            "phase keys are a versioned fixed contract"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}
