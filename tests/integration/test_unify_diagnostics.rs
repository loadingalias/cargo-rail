//! Unify failure diagnostics and progress through the production binary.
//!
//! A failing build script must reduce to one class, one cause, and one recovery section in
//! text and JSON, directly and behind a rustc wrapper. Inherited environment values must never
//! appear. Machine stdout stays one JSON value while progress stays on stderr.

use crate::helpers::{TestWorkspace, cargo_command, cargo_rail_command};
use anyhow::{Context as _, Result, ensure};
use std::fs;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Output, Stdio};

const INHERITED_VALUE: &str = "d2-inherited-value-7f3a";

/// A member whose build script fails, plus a non-member path dependency that makes Unify
/// acquire compiler evidence for it. Only path packages are used, so an isolated `CARGO_HOME`
/// works offline.
fn failing_build_script_workspace() -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named("unify-diagnostics")?;
    let root = ws.path.join("Cargo.toml");
    let manifest = fs::read_to_string(&root)?.replace(
        "members = [\"crates/*\"]",
        "members = [\"crates/*\"]\nexclude = [\"vendor\"]",
    );
    fs::write(&root, manifest)?;
    fs::create_dir_all(ws.path.join("vendor/helper/src"))?;
    fs::write(
        ws.path.join("vendor/helper/Cargo.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(ws.path.join("vendor/helper/src/lib.rs"), "pub fn help() {}\n")?;
    let consumer = ws.path.join("crates/consumer");
    fs::create_dir_all(consumer.join("src"))?;
    fs::write(
        consumer.join("Cargo.toml"),
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nhelper = { path = \"../../vendor/helper\" }\n",
    )?;
    fs::write(consumer.join("src/lib.rs"), "pub fn hello() {}\n")?;
    fs::write(
        consumer.join("build.rs"),
        r#"fn main() {
    println!("cargo:rerun-if-env-changed=NATIVE_SDK_ROOT");
    println!("cargo:rerun-if-env-changed=D2_INHERITED_SECRET");
    let inherited = std::env::var("D2_INHERITED_SECRET").unwrap_or_default();
    eprintln!("native SDK prerequisite missing; stdout={inherited}");
    println!("inherited={inherited}");
    std::process::exit(1);
}
"#,
    )?;
    let lockfile = cargo_command(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    ensure!(
        lockfile.status.success(),
        "offline lockfile generation failed: {lockfile:?}"
    );
    ws.commit("Add a member whose build script fails")?;
    Ok(ws)
}

fn unify(ws: &TestWorkspace, cargo_home: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Result<Output> {
    let mut command = cargo_rail_command(&ws.path)?;
    command
        .args(arguments)
        .env("CARGO_HOME", cargo_home)
        .env("D2_INHERITED_SECRET", INHERITED_VALUE);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().context("running cargo-rail")
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn failing_build_script_reports_one_safe_cause_in_text_json_and_wrapped_runs() {
    let result: Result<()> = (|| {
        let ws = failing_build_script_workspace()?;
        let cargo_home = tempfile::TempDir::new()?;
        let cargo_home = cargo_home.path();

        let direct = unify(&ws, cargo_home, &["rail", "unify", "--check"], &[])?;
        let (stdout, stderr) = text(&direct);
        assert_eq!(direct.status.code(), Some(2), "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert_eq!(stderr.matches("error: ").count(), 1, "one primary cause:\n{stderr}");
        for expected in [
            "error: the build script of `consumer v0.1.0` failed in compiler evidence view `consumer / ",
            "help: provide the native tools, files, or environment that this build script requires \
             (the build script declares that it reads `D2_INHERITED_SECRET`, `NATIVE_SDK_ROOT`)",
            "unify.compiler_targets = \"none\"",
            "rerun with --verbose to see Cargo's output",
            "the build script reported:\nnative SDK prerequisite missing; stdout=<env:D2_INHERITED_SECRET>",
            "reproduce with Cargo: cd ",
            "cargo check --locked --all-targets",
            "--package consumer",
        ] {
            assert!(stderr.contains(expected), "missing `{expected}`:\n{stderr}");
        }
        assert!(
            !stderr.contains("--message-format") && !stderr.contains("compiler-artifacts-v1/generation-"),
            "the reproduction is the user's Cargo command, not the private sandbox:\n{stderr}"
        );

        let json = unify(&ws, cargo_home, &["rail", "unify", "--check", "--format", "json"], &[])?;
        let (json_stdout, json_stderr) = text(&json);
        assert_eq!(json.status.code(), Some(2), "{json_stdout}\n{json_stderr}");
        let value: serde_json::Value = serde_json::from_str(&json_stdout).context("stdout must be one JSON value")?;
        assert_eq!(value["failure_class"], "build_script", "{value:#}");
        assert_eq!(value["code"], 2, "{value:#}");
        assert!(
            value["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("the build script of `consumer v0.1.0` failed")),
            "{value:#}"
        );
        assert!(
            json_stderr.contains("Detecting unused dependencies")
                && json_stderr.contains("Compiler acquisition progress"),
            "machine mode keeps progress on stderr:\n{json_stderr}"
        );
        assert!(
            !json_stdout.contains("--- stderr"),
            "machine output embeds no Cargo log:\n{json_stdout}"
        );

        // The same failure behind a transparent rustc wrapper.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let wrapper = ws.path.join("target/passthrough-wrapper");
            fs::create_dir_all(wrapper.parent().context("wrapper directory")?)?;
            fs::write(&wrapper, "#!/bin/sh\nexec \"$@\"\n")?;
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
            let wrapper = wrapper.to_str().context("non-UTF-8 wrapper path")?;
            let wrapped = unify(
                &ws,
                cargo_home,
                &["rail", "unify", "--check", "--format", "json"],
                &[("RUSTC_WRAPPER", wrapper)],
            )?;
            let (wrapped_stdout, wrapped_stderr) = text(&wrapped);
            assert_eq!(wrapped.status.code(), Some(2), "{wrapped_stdout}\n{wrapped_stderr}");
            let value: serde_json::Value = serde_json::from_str(&wrapped_stdout)?;
            assert_eq!(value["failure_class"], "build_script", "{value:#}");
            assert!(!wrapped_stdout.contains(INHERITED_VALUE) && !wrapped_stderr.contains(INHERITED_VALUE));
        }

        // --verbose adds Cargo's bounded output to text only.
        let verbose = unify(&ws, cargo_home, &["rail", "--verbose", "unify", "--check"], &[])?;
        let (_, verbose_stderr) = text(&verbose);
        assert_eq!(verbose.status.code(), Some(2), "{verbose_stderr}");
        assert!(
            verbose_stderr.contains("Cargo reported:") && verbose_stderr.contains("native SDK prerequisite missing"),
            "{verbose_stderr}"
        );

        // With a Cargo credential capability, Cargo's output is withheld even with --verbose.
        fs::create_dir_all(ws.path.join(".cargo"))?;
        fs::write(
            ws.path.join(".cargo/config.toml"),
            "[registries.private]\ntoken = \"secret-token-value\"\n",
        )?;
        let credentialed = unify(&ws, cargo_home, &["rail", "--verbose", "unify", "--check"], &[])?;
        let (credentialed_stdout, credentialed_stderr) = text(&credentialed);
        assert_eq!(credentialed.status.code(), Some(2), "{credentialed_stderr}");
        assert!(
            credentialed_stderr.contains("Cargo's output is withheld because a Cargo credential capability is active"),
            "{credentialed_stderr}"
        );
        assert!(
            !credentialed_stderr.contains("Cargo reported:"),
            "{credentialed_stderr}"
        );

        for (name, output) in [
            ("direct", stdout + &stderr),
            ("json", json_stdout + &json_stderr),
            ("credentialed", credentialed_stdout + &credentialed_stderr),
        ] {
            assert!(
                !output.contains(INHERITED_VALUE),
                "{name} leaked an inherited value:\n{output}"
            );
            assert!(
                !output.contains("secret-token-value"),
                "{name} leaked a credential:\n{output}"
            );
        }
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn cargo_file_lock_wait_is_reported_while_cargo_waits() {
    let result: Result<()> = (|| {
        let ws = failing_build_script_workspace()?;
        let cargo_home = tempfile::TempDir::new()?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(cargo_home.path().join(".package-cache"))?;
        lock.lock()?;

        let mut command = cargo_rail_command(&ws.path)?;
        let mut child = command
            .args(["rail", "unify", "--check", "--format", "json"])
            .env("CARGO_HOME", cargo_home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().context("stdout pipe")?;
        let stdout = std::thread::spawn(move || std::io::read_to_string(stdout));
        let (lines_tx, lines_rx) = std::sync::mpsc::channel();
        let stderr = BufReader::new(child.stderr.take().context("stderr pipe")?);
        let reader = std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });

        // Hold Cargo's package-cache lock until Cargo-Rail reports the wait.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut stderr = Vec::new();
        let reported = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match lines_rx.recv_timeout(remaining) {
                Ok(line) => {
                    let reported = line.contains("cargo metadata: Cargo is waiting for a file lock on package cache");
                    stderr.push(line);
                    if reported {
                        break true;
                    }
                }
                Err(_) => break false,
            }
        };
        lock.unlock()?;
        let status = child.wait()?;
        reader.join().map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
        stderr.extend(lines_rx.try_iter());
        let stderr = stderr.join("\n");
        let stdout = stdout.join().map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;

        assert!(reported, "the lock wait was not reported while Cargo waited:\n{stderr}");
        assert_eq!(status.code(), Some(2), "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).context("stdout must be one JSON value")?;
        assert_eq!(
            value["failure_class"], "build_script",
            "the run continues after the lock is released"
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn preview_with_pending_edits_succeeds_and_keeps_proof_in_explanation_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("unify-preview")?;
        ws.add_crate("alpha", "0.1.0", &[])?;
        ws.add_crate("beta", "0.1.0", &[("alpha", "{ path = \"../alpha\" }")])?;
        fs::write(
            ws.path.join("crates/beta/src/lib.rs"),
            "pub fn value() -> &'static str { alpha::hello() }\n",
        )?;
        let lockfile = cargo_command(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(
            lockfile.status.success(),
            "offline lockfile generation failed: {lockfile:?}"
        );
        ws.commit("Add a path dependency that Unify can inherit")?;

        let preview = crate::helpers::run_cargo_rail(&ws.path, &["rail", "unify"])?;
        let (stdout, stderr) = text(&preview);
        assert_eq!(preview.status.code(), Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(stdout.contains("Next: cargo rail unify apply"), "{stdout}");

        let check = crate::helpers::run_cargo_rail(&ws.path, &["rail", "unify", "--check"])?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&check.stderr)
        );
        assert_eq!(check.stdout, preview.stdout, "check and preview report the same edits");

        let explain = crate::helpers::run_cargo_rail(&ws.path, &["rail", "unify", "--explain"])?;
        let (explain_stdout, _) = text(&explain);
        assert_eq!(explain.status.code(), Some(0));
        assert!(
            explain_stdout.len() > stdout.len() && explain_stdout.starts_with(stdout.trim_end()),
            "explanation extends the default output:\ndefault:\n{stdout}\nexplain:\n{explain_stdout}"
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// Replace the failing build script with one that runs until it is interrupted.
#[cfg(unix)]
fn sleep_in_build_script(ws: &TestWorkspace) -> Result<()> {
    fs::write(
        ws.path.join("crates/consumer/build.rs"),
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(300)); }\n",
    )?;
    ws.commit("Make the build script run until interrupted")?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn interrupted_acquisition_names_its_phase_and_recovery() {
    let result: Result<()> = (|| {
        let ws = failing_build_script_workspace()?;
        sleep_in_build_script(&ws)?;
        let cargo_home = tempfile::TempDir::new()?;
        let mut child = cargo_rail_command(&ws.path)?
            .args(["rail", "unify", "--check", "--format", "json"])
            .env("CARGO_HOME", cargo_home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().context("stdout pipe")?;
        let stdout = std::thread::spawn(move || std::io::read_to_string(stdout));
        let mut stderr = BufReader::new(child.stderr.take().context("stderr pipe")?);
        let mut seen = String::new();
        loop {
            let mut line = String::new();
            ensure!(stderr.read_line(&mut line)? > 0, "acquisition never started:\n{seen}");
            seen.push_str(&line);
            if line.contains("Compiler acquisition progress:") && line.contains("active view=consumer") {
                break;
            }
        }
        // Interrupt only once the build script runs, so Cargo-Rail owns a live process tree.
        let script_pattern = format!("{}.*build-script-build", ws.path.display());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !std::process::Command::new("pgrep")
            .args(["-f", &script_pattern])
            .output()?
            .status
            .success()
        {
            ensure!(
                std::time::Instant::now() < deadline,
                "the build script never started:\n{seen}"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let interrupted = std::time::Instant::now();
        rustix::process::kill_process(rustix::process::Pid::from_child(&child), rustix::process::Signal::INT)?;
        let status = child.wait()?;
        std::io::Read::read_to_string(&mut stderr, &mut seen)?;
        let stdout = stdout.join().map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;

        assert!(
            interrupted.elapsed() < std::time::Duration::from_secs(60),
            "the build script's process tree was not stopped"
        );
        assert_eq!(status.code(), Some(2), "{seen}");
        let value: serde_json::Value = serde_json::from_str(&stdout).context("stdout must be one JSON value")?;
        assert_eq!(value["failure_class"], "interrupted", "{value:#}");
        assert_eq!(
            value["message"],
            "compiler acquisition was interrupted by SIGINT or SIGTERM during `Detecting unused dependencies`",
            "{value:#}"
        );
        assert!(
            value["help"]
                .as_str()
                .is_some_and(|help| help.contains("changed no workspace files; rerun the same command")),
            "{value:#}"
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn diagnostics_record_each_progress_phase_and_its_activity() {
    let result: Result<()> = (|| {
        let ws = failing_build_script_workspace()?;
        let cargo_home = tempfile::TempDir::new()?;
        let diagnostics = tempfile::TempDir::new()?;
        let file = diagnostics.path().join("unify.json");
        let output = unify(
            &ws,
            cargo_home.path(),
            &[
                "rail",
                "--quiet",
                "--diagnostics-file",
                file.to_str().context("non-UTF-8 diagnostics path")?,
                "unify",
                "--check",
            ],
            &[],
        )?;
        let (_, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            !stderr.contains("Detecting unused dependencies"),
            "--quiet suppresses progress:\n{stderr}"
        );

        let counters: serde_json::Value = serde_json::from_slice(&fs::read(&file)?)?;
        assert_eq!(counters["schema_version"], 17);
        let phases = counters["progress_phases"].as_array().context("progress phases")?;
        let find = |phase: &str| phases.iter().find(|entry| entry["phase"] == phase);
        for (phase, activity) in [
            ("running cargo metadata", "cargo"),
            ("Computing MSRV from dependency graph", "analysis"),
            ("acquiring compiler evidence", "cargo"),
            ("Detecting unused dependencies", "analysis"),
        ] {
            let entry = find(phase).with_context(|| format!("missing phase `{phase}`: {phases:#?}"))?;
            assert_eq!(entry["activity"], activity, "{entry:#}");
            assert!(
                entry["elapsed_ns"].as_u64().is_some_and(|elapsed| elapsed > 0),
                "{entry:#}"
            );
        }
        assert!(
            counters["compiler_acquisition"]["cargo_views"].as_u64() == Some(1),
            "Cargo process count sits beside the phases: {:#}",
            counters["compiler_acquisition"]
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}
