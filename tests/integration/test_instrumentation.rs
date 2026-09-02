use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;

use crate::helpers::{TestWorkspace, cargo_command, git, run_cargo_rail, rustc_host_target};

fn read_counters(path: &Path) -> Result<serde_json::Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading diagnostics from {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
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

        let args = ["rail", "plan", "--since", "HEAD~1", "--json"];
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
                "--json",
            ],
        )?;

        assert_eq!(measured.status, expected.status);
        assert_eq!(measured.stdout, expected.stdout, "diagnostics changed normal stdout");
        assert_eq!(measured.stderr, expected.stderr, "diagnostics changed normal stderr");

        let counters = read_counters(&diagnostics)?;
        assert_eq!(counters["schema_version"], 15);
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
        assert_eq!(
            counters["hashed_file_bytes_read"], 0,
            "a committed one-file plan must use captured Git object identities without hashing tracked files"
        );
        assert!(
            counters["git_subprocesses"].as_u64().is_some_and(|count| count <= 10),
            "sparse planning must improve on the 11-process v7 capture baseline"
        );
        assert_eq!(
            counters["graph_traversals"], 1,
            "all Cargo work kinds must derive from one shared structural reverse-dependency closure"
        );
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
        git(target.path(), &["init", "--initial-branch=main"])?;
        std::fs::write(
            ws.path.join("rail.toml"),
            format!(
                "[crates.streamed.split]\nremote = \"{}\"\nbranch = \"main\"\nmode = \"single\"\n",
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
        eprintln!(
            "P5 split measurement: git_subprocesses={subprocesses}, object_reads={objects}, object_batches={batches}"
        );
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
                    "--json",
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
            format!(
                "targets = [\"{}\"]\n\n[unify]\nmsrv_policy = {{ mode = \"disabled\" }}\n",
                rustc_host_target()?
            ),
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
                "--json",
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
fn unify_diagnostics_measure_bounded_compiler_acquisition_and_warm_outcome() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("diagnostic-compiler-acquisition")?;
        let root_manifest_path = ws.path.join("Cargo.toml");
        let root_manifest = std::fs::read_to_string(&root_manifest_path)?.replace(
            "members = [\"crates/*\"]",
            "members = [\"crates/*\"]\nexclude = [\"dependency\"]",
        );
        std::fs::write(root_manifest_path, root_manifest)?;
        let dependency = ws.path.join("dependency");
        std::fs::create_dir_all(dependency.join("src"))?;
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"measured-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        std::fs::write(dependency.join("src/lib.rs"), "pub fn unused() {}\n")?;
        let package = ws.add_crate(
            "measured",
            "0.1.0",
            &[("measured-dependency", "{ path = \"../../dependency\" }")],
        )?;
        let manifest_path = package.join("Cargo.toml");
        let mut manifest = std::fs::read_to_string(&manifest_path)?;
        manifest.push_str("\n[features]\nextra = []\n");
        std::fs::write(manifest_path, manifest)?;
        let lockfile = cargo_command(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(
            lockfile.status.success(),
            "offline lockfile generation failed: {}",
            String::from_utf8_lossy(&lockfile.stderr)
        );
        ws.commit("Add compiler acquisition fixture")?;

        let cold_path = ws.path.join("cold-acquisition.json");
        let warm_path = ws.path.join("warm-acquisition.json");
        let run = |diagnostics: &Path| -> Result<std::process::Output> {
            run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "--diagnostics-file",
                    diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                    "unify",
                    "--check",
                    "--json",
                ],
            )
        };

        let cold = run(&cold_path)?;
        ensure!(
            cold.status.success() || cold.status.code() == Some(1),
            "cold compiler acquisition failed: {}",
            String::from_utf8_lossy(&cold.stderr)
        );
        let cold = read_counters(&cold_path)?;
        let acquisition = &cold["compiler_acquisition"];
        assert_eq!(cold["schema_version"], 15);
        assert_eq!(acquisition["plans"], 1);
        assert!(
            acquisition["plan_identity"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("compiler-acquisition-plan-v1-sha256-"))
        );
        assert!(acquisition["views"].as_u64().is_some_and(|views| views >= 3));
        assert!(acquisition["cargo_views"].as_u64().is_some_and(|views| views > 0));
        assert!(
            acquisition["configured_process_slots"]
                .as_u64()
                .is_some_and(|slots| slots > 0)
        );
        assert!(
            acquisition["configured_work_permits"]
                .as_u64()
                .is_some_and(|permits| permits > 0)
        );
        assert_eq!(acquisition["live_cargo_processes"], 0);
        assert!(
            acquisition["max_live_cargo_processes"]
                .as_u64()
                .is_some_and(|processes| processes > 0)
        );
        assert!(
            acquisition["max_nonwaiting_cargo_views"]
                .as_u64()
                .is_some_and(|views| views > 0)
        );
        assert!(acquisition["max_live_cargo_processes"].as_u64() <= acquisition["configured_process_slots"].as_u64());
        assert!(acquisition["max_nonwaiting_cargo_views"].as_u64() <= acquisition["configured_work_permits"].as_u64());
        assert!(
            acquisition["compiler_actions"]
                .as_u64()
                .is_some_and(|actions| actions > 0)
        );
        assert!(
            acquisition["stdout_bytes_retained"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
        );
        assert!(
            acquisition["stdout_bytes_read"].as_u64() >= acquisition["stdout_bytes_retained"].as_u64(),
            "streaming cannot retain more Cargo stdout than it read"
        );
        assert!(
            acquisition["output_retention_high_water_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes <= 64 * 1024 * 1024 + 16 * 1024),
            "Cargo output retention exceeded its fixed stdout/stderr bounds"
        );
        assert!(
            acquisition["cargo_messages_read"]
                .as_u64()
                .is_some_and(|messages| messages > 0)
        );
        let sandboxes_created = acquisition["sandboxes_created"]
            .as_u64()
            .context("sandbox create count is not an integer")?;
        let sandboxes_reused = acquisition["sandboxes_reused"]
            .as_u64()
            .context("sandbox reuse count is not an integer")?;
        assert!(sandboxes_created > 0);
        assert_eq!(sandboxes_created + sandboxes_reused, acquisition["cargo_views"]);
        assert_eq!(acquisition["sandboxes_deleted"], sandboxes_created);
        assert_eq!(acquisition["sandboxes_poisoned"], 0);
        assert_eq!(acquisition["process_tree_terminations"], 0);
        assert!(
            acquisition["artifact_tree_walks"]
                .as_u64()
                .is_some_and(|walks| walks > 0)
        );
        assert!(
            acquisition["evidence_cache_lookups"]
                .as_u64()
                .is_some_and(|lookups| lookups > 0)
        );
        assert_eq!(acquisition["journal_writes"], 0, "Unify does not own a Surface journal");

        let warm_output = run(&warm_path)?;
        ensure!(
            warm_output.status.success() || warm_output.status.code() == Some(1),
            "warm compiler acquisition failed: {}",
            String::from_utf8_lossy(&warm_output.stderr)
        );
        let warm = read_counters(&warm_path)?;
        let warm_acquisition = &warm["compiler_acquisition"];
        assert_eq!(warm_acquisition["plan_identity"], acquisition["plan_identity"]);
        assert_eq!(warm_acquisition["views"], acquisition["views"]);
        let warm_views = warm_acquisition["cargo_views"]
            .as_u64()
            .context("warm Cargo view count is not an integer")?;
        let warm_hits = warm_acquisition["evidence_cache_hits"]
            .as_u64()
            .context("warm evidence hit count is not an integer")?;
        assert_eq!(
            warm_views + warm_hits,
            warm_acquisition["views"]
                .as_u64()
                .context("planned view count is not an integer")?,
            "each warm view must be an exact hit or execute normally"
        );
        let warm_actions = warm_acquisition["compiler_actions"]
            .as_u64()
            .context("warm compiler action count is not an integer")?;
        if warm_views == 0 {
            assert_eq!(warm_actions, 0, "an exact warm hit must start no compiler action");
        } else {
            assert!(
                warm_actions > 0,
                "a fail-closed warm bypass must measure its compiler work"
            );
            assert!(
                String::from_utf8_lossy(&warm_output.stdout).contains("miss_reasons"),
                "a repeated warm acquisition must explain why reuse was unavailable"
            );
        }
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
        assert!(
            output.stdout.is_empty(),
            "raw schema failure must not change stdout protocols: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains("failed to reserve diagnostic counter file"),
            "raw schema failure must report its diagnostic on stderr: {stderr}"
        );
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
        assert_eq!(counters["schema_version"], 15);
        assert_eq!(counters["phases"]["cli_pre_context_preparation"]["invocations"], 1);
        assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
        assert_eq!(
            counters["phases"].as_object().map(serde_json::Map::len),
            Some(3),
            "phase keys are a versioned fixed contract"
        );
        assert_eq!(counters["compiler_acquisition"]["plans"], 0);
        assert_eq!(counters["compiler_acquisition"]["cargo_views"], 0);
        assert_eq!(counters["compiler_acquisition"]["journal_writes"], 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn clean_captures_no_cargo_metadata_or_dependency_graph() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("diagnostic-clean-context")?;
        workspace.add_crate("member", "0.1.0", &[])?;
        let report = workspace.path.join("target/cargo-rail/report.md");
        std::fs::create_dir_all(report.parent().context("report parent")?)?;
        std::fs::write(&report, "generated report\n")?;

        let output_dir = TempDir::new()?;
        let diagnostics = output_dir.path().join("clean.json");
        let measured = run_cargo_rail(
            &workspace.path,
            &[
                "rail",
                "--diagnostics-file",
                diagnostics.to_str().context("non-UTF-8 diagnostics path")?,
                "clean",
                "--reports",
                "--check",
                "--json",
            ],
        )?;
        assert_eq!(measured.status.code(), Some(1), "clean preview failed: {measured:?}");
        assert!(report.is_file(), "check mode must not remove the report");

        let counters = read_counters(&diagnostics)?;
        assert_eq!(counters["cargo_metadata_loads"], 0);
        assert_eq!(counters["cargo_metadata_cache_hits"], 0);
        assert_eq!(counters["graph_traversals"], 0);
        assert_eq!(counters["graph_node_visits"], 0);
        assert_eq!(counters["graph_edge_visits"], 0);
        assert_eq!(counters["phases"]["workspace_capture_cargo_metadata"]["invocations"], 0);
        Ok(())
    })();
    super::helpers::finish_test(result);
}
