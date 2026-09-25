//! Golden renderings of Unify output in each output mode.
//!
//! Each snapshot records the complete stdout and stderr of one production run, with host paths,
//! durations, and host-dependent byte counts masked. A changed rendering fails here, so wording
//! and layout changes are reviewed as diffs. Set `CARGO_RAIL_BLESS=1` to rewrite the snapshots.
//! Cancellation keeps its exact assertions in `test_unify_diagnostics`, because a timed signal
//! cannot produce a stable transcript.

use crate::helpers::{TestWorkspace, cargo_command, cargo_rail_command};
use anyhow::{Context as _, Result, ensure};
use std::fs;
use std::path::{Path, PathBuf};

fn snapshot_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/output")
}

/// A member with an unused path dependency and a restated workspace package field.
fn pending_edits_workspace() -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named("unify-output-snapshots")?;
    let manifest = ws.path.join("Cargo.toml");
    let root = fs::read_to_string(&manifest)?.replace(
        "members = [\"crates/*\"]",
        "members = [\"crates/*\"]\nexclude = [\"vendor\"]",
    );
    fs::write(&manifest, root)?;
    let helper = ws.path.join("vendor/helper");
    fs::create_dir_all(helper.join("src"))?;
    fs::write(
        helper.join("Cargo.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(helper.join("src/lib.rs"), "pub fn help() {}\n")?;
    let app = ws.path.join("crates/app");
    fs::create_dir_all(app.join("src"))?;
    fs::write(
        app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition.workspace = true\nlicense = \"MIT\"\n\n\
         [dependencies]\nhelper = { path = \"../../vendor/helper\" }\n",
    )?;
    fs::write(app.join("src/lib.rs"), "pub fn value() {}\n")?;
    let lockfile = cargo_command(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    ensure!(lockfile.status.success(), "{lockfile:?}");
    ws.commit("Pending Unify edits")?;
    Ok(ws)
}

/// Replace the digits (and decimal point) that follow each `marker` with `<n>`.
fn mask_numbers_after(text: &str, marker: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once(marker) {
        output.push_str(before);
        output.push_str(marker);
        let tail = after.trim_start_matches(|character: char| character.is_ascii_digit() || character == '.');
        if tail.len() != after.len() {
            output.push_str("<n>");
        }
        rest = tail;
    }
    output.push_str(rest);
    output
}

/// Replace every run of 12 or more lowercase hex digits, such as commit and content identities
/// that derive from the fixture's commit time, with `<hex>`.
fn mask_hex_identities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, output: &mut String| {
        output.push_str(if run.len() >= 12 { "<hex>" } else { run.as_str() });
        run.clear();
    };
    for character in text.chars() {
        if character.is_ascii_digit() || ('a'..='f').contains(&character) {
            run.push(character);
        } else {
            flush(&mut run, &mut output);
            output.push(character);
        }
    }
    flush(&mut run, &mut output);
    output
}

fn normalize(text: &str, workspace: &Path, cargo_home: &Path) -> String {
    let mut text = mask_hex_identities(&text.replace("\r\n", "\n"));
    for (path, label) in [(workspace, "<workspace>"), (cargo_home, "<cargo-home>")] {
        if let Ok(canonical) = path.canonicalize() {
            text = text.replace(&canonical.display().to_string(), label);
        }
        text = text.replace(&path.display().to_string(), label);
    }
    if let Ok(host) = crate::helpers::rustc_host_target() {
        text = text.replace(&host, "<host>");
    }
    text = mask_numbers_after(&text, "\"rustc_release\": \"");
    // Windows renders the same paths with backslashes.
    text = text.replace('\\', "/");
    // The selected Cargo executable lives in the host's toolchain directory.
    text = text
        .split(' ')
        .map(|word| {
            if word.contains("/toolchains/") && word.ends_with("/bin/cargo") {
                "<cargo>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    for marker in [
        "elapsed=",
        "owned bytes=",
        " current / ",
        "processes; ",
        "sandboxes; ",
        " views; ",
        " planned views; ",
    ] {
        text = mask_numbers_after(&text, marker);
    }
    // Artifact limits derive from the host's free disk space.
    text.lines()
        .map(|line| match line.split_once(" bytes soft; ") {
            Some((before, after)) => {
                let before = before.rsplit_once(' ').map_or("", |(head, _)| head);
                let after = after.split_once(' ').map_or("", |(_, tail)| tail);
                format!("{before} <n> bytes soft; <n> {after}")
            }
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn check_snapshot(name: &str, actual: &str) -> Result<()> {
    let path = snapshot_directory().join(format!("{name}.txt"));
    if std::env::var_os("CARGO_RAIL_BLESS").is_some() {
        fs::create_dir_all(snapshot_directory())?;
        fs::write(&path, actual)?;
        return Ok(());
    }
    let expected = fs::read_to_string(&path)
        .with_context(|| format!("missing snapshot {}; rerun with CARGO_RAIL_BLESS=1", path.display()))?;
    ensure!(
        expected == actual,
        "{name} output changed; review it and rerun with CARGO_RAIL_BLESS=1 to accept\n--- expected\n{expected}--- actual\n{actual}"
    );
    Ok(())
}

fn transcript(output: &std::process::Output) -> String {
    format!(
        "exit: {:?}\n--- stdout\n{}--- stderr\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn unify_output_matches_its_golden_rendering_in_each_mode() {
    let result: Result<()> = (|| {
        let ws = pending_edits_workspace()?;
        let cargo_home = tempfile::TempDir::new()?;
        let run = |arguments: &[&str]| -> Result<std::process::Output> {
            Ok(cargo_rail_command(&ws.path)?
                .args(arguments)
                .env("CARGO_HOME", cargo_home.path())
                .env("NO_COLOR", "1")
                .output()?)
        };
        // A cold run first, so every snapshot below reuses the same stored evidence.
        run(&["rail", "unify", "--check"])?;
        for (name, arguments) in [
            ("unify-check-text", &["rail", "unify", "--check"][..]),
            ("unify-check-quiet", &["rail", "--quiet", "unify", "--check"][..]),
            ("unify-check-verbose", &["rail", "--verbose", "unify", "--check"][..]),
            ("unify-check-explain", &["rail", "unify", "--check", "--explain"][..]),
            (
                "unify-check-json",
                &["rail", "unify", "--check", "--format", "json"][..],
            ),
        ] {
            let output = run(arguments)?;
            let rendered = normalize(&transcript(&output), &ws.path, cargo_home.path());
            check_snapshot(name, &rendered)?;
        }
        #[cfg(unix)]
        {
            let interactive = run_in_terminal(&ws.path, cargo_home.path(), &["rail", "unify", "--check"])?;
            check_snapshot(
                "unify-check-interactive",
                &normalize(&interactive, &ws.path, cargo_home.path()),
            )?;
        }
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// Run with stdout and stderr on one pseudo-terminal and return what a person would see.
#[cfg(unix)]
fn run_in_terminal(workspace: &Path, cargo_home: &Path, arguments: &[&str]) -> Result<String> {
    use rustix::fs::{Mode, OFlags, open};
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::ffi::OsStr;
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::process::Stdio;

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let slave_name = ptsname(&master, Vec::new())?;
    let slave = open(
        OsStr::from_bytes(slave_name.to_bytes()),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let slave = fs::File::from(slave);
    let mut command = cargo_rail_command(workspace)?;
    command
        .args(arguments)
        .env("CARGO_HOME", cargo_home)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    let mut child = command.spawn()?;
    // Our copies of the slave closed when `command` consumed them; the master reads until the
    // child's side closes.
    drop(command);
    let mut master = fs::File::from(master);
    let reader = std::thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => captured.extend_from_slice(&buffer[..read]),
                // macOS and Linux report EIO once the last slave descriptor closes.
                Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => break,
                Err(error) => return Err(error),
            }
        }
        Ok(captured)
    });
    let status = child.wait()?;
    let captured = reader
        .join()
        .map_err(|_| anyhow::anyhow!("terminal reader panicked"))??;
    Ok(format!(
        "exit: {:?}\n--- terminal\n{}",
        status.code(),
        String::from_utf8_lossy(&captured)
    ))
}
