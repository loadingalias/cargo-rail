//! Release planning, changelog, change-file, and local Git effect coverage for `cargo rail release`.
//!
//! Covers:
//! - reviewed and conventional release intent, bump selection, and skipped crates
//! - version groups and the dependent closure of named crates
//! - changelog paths, shapes, and compare links
//! - auxiliary Cargo lockfile projection and its safety checks
//! - API checks, publishability, confirmation, and branch preconditions
//! - the local Git effects: one release commit, annotated tags, and one atomic push
//!
//! A release starts only from origin's tip with a clean worktree. Every release fixture commits
//! its Cargo.lock and publishes its branch to a bare `origin` under the ignored `target/` directory.

use crate::helpers::{
    NestedWorkspace, TestWorkspace, cargo_command, cargo_rail_command, file_url, git, git_command, run_cargo_rail,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// ----- Fixtures --------------------------------------------------------------------------------

fn generate_lockfile(workspace: &Path) -> Result<()> {
    let output = cargo_command(workspace)
        .args(["generate-lockfile", "--offline", "--manifest-path", "Cargo.toml"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Cargo.lock generation failed in '{}': {}",
        workspace.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        workspace.join("Cargo.lock").is_file(),
        "Cargo succeeded without creating '{}'",
        workspace.join("Cargo.lock").display()
    );
    Ok(())
}

/// Commit every pending fixture file together with an up-to-date Cargo.lock.
///
/// cargo-rail reads Cargo metadata, and Cargo writes a missing Cargo.lock. A release refuses a
/// worktree with that untracked file, so a release fixture commits the lockfile first.
fn commit_with_lockfile(ws: &TestWorkspace, message: &str) -> Result<String> {
    generate_lockfile(&ws.path)?;
    if !git(&ws.path, &["status", "--porcelain"])?.stdout.is_empty() {
        ws.commit(message)?;
    }
    head(&ws.path)
}

/// Commit pending fixture files and publish the branch and its tags to a new bare `origin`.
fn publish_to_origin(ws: &TestWorkspace) -> Result<PathBuf> {
    commit_with_lockfile(ws, "Prepare the release fixture")?;
    ws.with_origin()
}

/// Commit pending fixture files and push the branch and its tags to `origin`.
fn commit_and_push(ws: &TestWorkspace, message: &str) -> Result<String> {
    let head = commit_with_lockfile(ws, message)?;
    ws.push()?;
    Ok(head)
}

/// Publish a nested workspace's repository to a new bare `origin` under its ignored `target/`.
fn publish_nested_to_origin(ws: &NestedWorkspace) -> Result<PathBuf> {
    let origin = ws.git_root.join("target/origin.git");
    std::fs::create_dir_all(&origin)?;
    git(&origin, &["init", "--bare", "--initial-branch=main"])?;
    let url = origin.to_str().context("origin path is not UTF-8")?;
    git(&ws.git_root, &["remote", "add", "origin", url])?;
    git(
        &ws.git_root,
        &["push", "--quiet", "--set-upstream", "origin", "HEAD:main", "--tags"],
    )?;
    Ok(origin)
}

/// Release policy for most fixtures: per-crate tags, no API checker, and no registry.
fn write_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
    ws.write_release_config(&format!(
        "tag_format = \"{{crate}}-v{{version}}\"\nsemver_check = \"off\"\npublish = \"none\"\n{extras}\n"
    ))
}

fn write_test_change(workspace: &Path, crates: &[&str]) -> Result<()> {
    let intents = crates
        .iter()
        .map(|crate_name| (*crate_name, "patch"))
        .collect::<Vec<_>>();
    write_test_change_levels(workspace, &intents)
}

fn write_test_change_levels(workspace: &Path, intents: &[(&str, &str)]) -> Result<()> {
    let changes = workspace.join(".changes");
    std::fs::create_dir_all(&changes)?;
    let mut contents = String::from("---\n");
    for (crate_name, bump) in intents {
        contents.push_str(&format!("\"{crate_name}\" = \"{bump}\"\n"));
    }
    contents.push_str("---\n\nExercise the current release contract.\n");
    std::fs::write(changes.join("release-test.md"), contents)?;
    Ok(())
}

fn tag_release(ws: &TestWorkspace, crate_name: &str, version: &str) -> Result<()> {
    ws.tag(
        &format!("{}-v{}", crate_name, version),
        &format!("Release {} {}", crate_name, version),
    )
}

/// A single-crate repository with reviewed patch intent, published to `origin`.
fn push_release_workspace(crate_name: &str) -> Result<(TestWorkspace, PathBuf)> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    ws.write_release_config("tag_format = \"v{version}\"\nsemver_check = \"off\"\npublish = \"none\"\n")?;
    write_test_change(&ws.path, &[crate_name])?;
    let origin = publish_to_origin(&ws)?;
    Ok((ws, origin))
}

fn install_pre_push_hook(ws: &TestWorkspace, script: &str) -> Result<()> {
    let hook_path = ws.path.join(".git/hooks/pre-push");
    std::fs::write(&hook_path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }
    Ok(())
}

/// Helper to create a crate with publish = false in Cargo.toml
fn add_unpublishable_crate(ws: &TestWorkspace, name: &str, version: &str) -> Result<()> {
    let crate_path = ws.path.join("crates").join(name);
    std::fs::create_dir_all(&crate_path)?;
    std::fs::create_dir_all(crate_path.join("src"))?;

    // Cargo.toml with publish = false
    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "{}"
edition = "2024"
publish = false

[dependencies]
"#,
        name, version
    );
    std::fs::write(crate_path.join("Cargo.toml"), cargo_toml)?;

    // Add a basic `lib.rs`
    std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}\n")?;

    Ok(())
}

fn add_workspace_dependency(ws: &TestWorkspace, name: &str, version: &str) -> Result<()> {
    let root_manifest = ws.path.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&root_manifest)?;
    let needle = "[workspace.dependencies]\n";
    let replacement = format!(
        "{}{} = {{ version = \"{}\", path = \"crates/{}\" }}\n",
        needle, name, version, name
    );
    let updated = manifest.replacen(needle, &replacement, 1);
    std::fs::write(root_manifest, updated)?;
    Ok(())
}

/// Helper to add a crate with a path-only dep
fn add_crate_with_path_dep(ws: &TestWorkspace, name: &str, version: &str, dep_name: &str, publish: bool) -> Result<()> {
    let crate_path = ws.path.join("crates").join(name);
    std::fs::create_dir_all(&crate_path)?;
    std::fs::create_dir_all(crate_path.join("src"))?;

    let publish_line = if publish { "" } else { "publish = false\n" };
    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "{}"
edition = "2021"
{}
[dependencies]
{} = {{ path = "../{}" }}
"#,
        name, version, publish_line, dep_name, dep_name
    );
    std::fs::write(crate_path.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(crate_path.join("src/lib.rs"), "pub fn hello() {}\n")?;

    Ok(())
}

/// A loopback sparse registry that holds no packages.
///
/// A dry run against it reaches registry preflight, API checks, and package verification without
/// the network. It answers reads only, so nothing can be published to it.
struct EmptyRegistry {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl EmptyRegistry {
    /// The Cargo registry name fixtures configure as `release.publish`.
    const NAME: &'static str = "fixture";

    fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    drop(serve_empty_registry(stream, address));
                }
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    /// Cargo configuration for the registry, with a token so preflight finds credentials.
    fn environment(&self) -> [(String, String); 2] {
        [
            (
                "CARGO_REGISTRIES_FIXTURE_INDEX".to_owned(),
                format!("sparse+http://{}/", self.address),
            ),
            ("CARGO_REGISTRIES_FIXTURE_TOKEN".to_owned(), "fixture-token".to_owned()),
        ]
    }

    fn release(&self, cwd: &Path, args: &[&str]) -> Result<Output> {
        let mut command = cargo_rail_command(cwd)?;
        command.args(["rail", "release"]).args(args).envs(self.environment());
        Ok(command.output()?)
    }
}

impl Drop for EmptyRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        drop(TcpStream::connect(self.address));
        if let Some(worker) = self.worker.take() {
            drop(worker.join());
        }
    }
}

fn serve_empty_registry(stream: TcpStream, address: SocketAddr) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    let (status, body) = if request.starts_with("GET ") && path.ends_with("/config.json") {
        ("200 OK", format!("{{\"dl\":\"http://{address}/dl\"}}"))
    } else {
        ("404 Not Found", String::new())
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

// ----- Command and repository helpers ----------------------------------------------------------

fn release(cwd: &Path, args: &[&str]) -> Result<Output> {
    let mut arguments = vec!["rail", "release"];
    arguments.extend_from_slice(args);
    run_cargo_rail(cwd, &arguments)
}

/// Run `cargo rail release ARGS --format json` and parse its one JSON document.
fn release_json(cwd: &Path, args: &[&str]) -> Result<(Output, Value)> {
    let mut arguments = args.to_vec();
    arguments.extend(["--format", "json"]);
    let output = release(cwd, &arguments)?;
    let value =
        serde_json::from_slice(&output.stdout).with_context(|| format!("release JSON\n{}", describe(&output)))?;
    Ok((output, value))
}

fn describe(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn rev_parse(repository: &Path, revision: &str) -> Result<String> {
    Ok(String::from_utf8(git(repository, &["rev-parse", revision])?.stdout)?
        .trim()
        .to_owned())
}

fn head(repository: &Path) -> Result<String> {
    rev_parse(repository, "HEAD")
}

/// The content of `path` in HEAD.
fn committed(repository: &Path, path: &str) -> Result<String> {
    Ok(String::from_utf8(
        git(repository, &["show", &format!("HEAD:{path}")])?.stdout,
    )?)
}

/// Whether HEAD's tree contains `path`.
fn committed_exists(repository: &Path, path: &str) -> bool {
    git(repository, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok()
}

fn local_tags(repository: &Path) -> Result<Vec<String>> {
    Ok(String::from_utf8(git(repository, &["tag", "--list"])?.stdout)?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The commit a ref names in a bare repository, if the ref exists.
fn ref_commit(repository: &Path, reference: &str) -> Option<String> {
    rev_parse(repository, &format!("{reference}^{{commit}}")).ok()
}

fn crate_report<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["crates"]
        .as_array()
        .and_then(|crates| crates.iter().find(|entry| entry["name"] == name))
        .unwrap_or(&Value::Null)
}

fn crate_names(report: &Value) -> Vec<String> {
    report["crates"]
        .as_array()
        .map(|crates| {
            crates
                .iter()
                .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn skipped_reason<'a>(report: &'a Value, name: &str) -> Option<&'a str> {
    report["skipped"]
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == name)?["reason"]
        .as_str()
}

// ----- Release sources and presentation --------------------------------------------------------

#[test]
fn current_configuration_preserves_release_sources_and_presentation() {
    let result: Result<()> = (|| {
        for source in ["commits", "both"] {
            let ws = TestWorkspace::new_named("release-config-presentation")?;
            ws.add_crate("lib-a", "1.2.3", &[])?;
            write_release_config(
                &ws,
                &format!(
                    "source = '{source}'\n[release.changelog]\nemoji = false\nentry_format = '- CUSTOM: {{description}}'\n[release.changelog.filters]\nskip_types = ['docs']\n"
                ),
            )?;
            commit_with_lockfile(&ws, "fixture")?;
            tag_release(&ws, "lib-a", "1.2.3")?;
            ws.modify_file("lib-a", "src/lib.rs", "pub fn added() {}\n")?;
            ws.commit("feat: add the API")?;
            if source == "both" {
                std::fs::create_dir_all(ws.path.join(".changes"))?;
                std::fs::write(
                    ws.path.join(".changes/reviewed.md"),
                    "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed addition.\n",
                )?;
                ws.commit("docs: review the addition")?;
            }
            publish_to_origin(&ws)?;
            let config_path = ws.path.join(".config/rail.toml");
            let original = std::fs::read(&config_path)?;

            let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run"])?;
            assert_eq!(output.status.code(), Some(0), "{source}: {}", describe(&output));
            assert_eq!(report["result"], "planned", "{source}");
            let planned = crate_report(&report, "lib-a");
            assert_eq!(planned["version"], "1.3.0", "{source}: {report}");
            let notes = planned["notes"].as_str().unwrap_or_default();
            assert!(notes.contains("CUSTOM: add the API"), "{notes}");
            assert!(
                !notes.contains("review the addition"),
                "skipped docs commit leaked: {notes}"
            );
            if source == "both" {
                assert!(notes.contains("Reviewed addition."), "{notes}");
                assert_eq!(planned["reason"], "auto: reviewed change files -> minor");
            } else {
                assert_eq!(planned["reason"], "auto: conventional commits -> minor");
            }
            assert_eq!(std::fs::read(config_path)?, original);
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_works_on_single_crate_repo() {
    let result: Result<()> = (|| {
        // Test that release plan works on a split repo (single-crate, non-workspace)
        let ws = TestWorkspace::new_single_crate("private-tool", "0.1.0")?;

        // Add release config (what a split repo would have)
        ws.write_release_config(
            r#"tag_format = "release-{crate}-v{version}"
semver_check = "off"
publish = "none"
"#,
        )?;
        commit_with_lockfile(&ws, "Configure custom release tags")?;
        ws.tag("release-private-tool-v0.1.0", "Initial release")?;
        publish_to_origin(&ws)?;

        let explained = run_cargo_rail(
            &ws.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(explained.status.success(), "config explain failed: {explained:?}");
        let explained: Value = serde_json::from_slice(&explained.stdout)?;
        assert_eq!(explained["fields"][0]["configured"], "release-{crate}-v{version}");
        assert_eq!(explained["fields"][0]["effective"], "release-{crate}-v{version}");

        // The configured tag format finds the previous release.
        let (output, report) = release_json(&ws.path, &["--dry-run"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(report["result"], "nothing_to_release");
        let reason = skipped_reason(&report, "private-tool").unwrap_or_default();
        assert!(reason.contains("since release-private-tool-v0.1.0"), "{report}");

        write_test_change(&ws.path, &["private-tool"])?;
        commit_and_push(&ws, "Review the patch")?;

        let output = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("private-tool"),
            "Plan should include private-tool. Output:\n{text}"
        );
        assert!(
            text.contains("0.1.0 -> 0.1.1"),
            "Plan should show version bump. Output:\n{text}"
        );
        assert!(
            text.contains("release-private-tool-v0.1.1"),
            "explicit tag format must control the single-package plan. Output:\n{text}"
        );

        let (output, report) = release_json(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let planned = crate_report(&report, "private-tool");
        assert_eq!(planned["tag"], "release-private-tool-v0.1.1");
        assert_eq!(planned["previous_version"], "0.1.0");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_tag_defaults_align_across_explain_and_planning() {
    let result: Result<()> = (|| {
        let single = TestWorkspace::new_single_crate("single", "0.1.0")?;
        single.write_release_config("semver_check = \"off\"\npublish = \"none\"\n")?;
        write_test_change(&single.path, &["single"])?;
        publish_to_origin(&single)?;
        let single_explain = run_cargo_rail(
            &single.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(
            single_explain.status.success(),
            "config explain failed: {single_explain:?}"
        );
        let single_explain: Value = serde_json::from_slice(&single_explain.stdout)?;
        assert_eq!(single_explain["fields"][0]["default"], "{crate}-v{version}");
        assert_eq!(single_explain["fields"][0]["effective"], "{crate}-v{version}");
        let (output, single_plan) = release_json(&single.path, &["--dry-run", "--bump", "patch"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(crate_report(&single_plan, "single")["tag"], "single-v0.1.1");

        let multi = TestWorkspace::new_named("release-default-tag-multi")?;
        multi.add_crate("crate-a", "0.1.0", &[])?;
        multi.add_crate("crate-b", "0.1.0", &[])?;
        multi.write_release_config("semver_check = \"off\"\npublish = \"none\"\n")?;
        write_test_change(&multi.path, &["crate-a"])?;
        publish_to_origin(&multi)?;
        let multi_explain = run_cargo_rail(
            &multi.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(
            multi_explain.status.success(),
            "config explain failed: {multi_explain:?}"
        );
        let multi_explain: Value = serde_json::from_slice(&multi_explain.stdout)?;
        assert_eq!(multi_explain["fields"][0]["default"], "{crate}-v{version}");
        assert_eq!(multi_explain["fields"][0]["effective"], "{crate}-v{version}");
        let (output, multi_plan) = release_json(&multi.path, &["crate-a", "--dry-run", "--bump", "patch"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(crate_names(&multi_plan), ["crate-a"]);
        assert_eq!(crate_report(&multi_plan, "crate-a")["tag"], "crate-a-v0.1.1");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn reviewed_changes_are_the_only_release_source() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-source-changes-default")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "1.2.3", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "1.2.3")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn redesigned() {}\n")?;
        ws.commit("feat!: conventional history must not control this release")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch intent.\n",
        )?;
        publish_to_origin(&ws)?;

        let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let planned = crate_report(&report, "lib-a");
        assert_eq!(planned["version"], "1.2.4");
        assert_eq!(planned["reason"], "auto: reviewed change files -> patch");
        assert!(
            planned["notes"]
                .as_str()
                .unwrap_or_default()
                .contains("Reviewed patch intent.")
        );
        assert!(
            !stdout(&output).contains("conventional history must not control"),
            "changes mode leaked commit prose: {}",
            stdout(&output)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Change-file coverage and consumption ----------------------------------------------------

#[test]
fn no_release_change_intent_satisfies_default_coverage_without_a_bump() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-no-release-intent")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub(crate) fn reorganized() {}\n")?;
        ws.commit("internal reorganization")?;

        let add = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "none",
                "--message",
                "Internal-only refactor; no released behavior changed.",
            ],
        )?;
        assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));
        publish_to_origin(&ws)?;

        let check = release(&ws.path, &["lib-a", "--dry-run"])?;
        assert!(
            check.status.success(),
            "reviewed no-release intent should satisfy coverage\n{}",
            describe(&check)
        );
        let text = stdout(&check);
        assert!(text.contains("nothing to release"), "{text}");
        assert!(!text.contains("Internal-only refactor"), "{text}");

        let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(report["result"], "nothing_to_release");
        let reason = skipped_reason(&report, "lib-a").unwrap_or_default();
        assert!(
            reason.contains("no reviewed release intent or dependency updates"),
            "{report}"
        );
        assert!(!stdout(&output).contains("Internal-only refactor"), "{report}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_retains_unconsumed_no_release_intent_from_a_shared_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-retain-no-release-intent")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn released_change() {}\n")?;
        ws.modify_file("lib-b", "src/lib.rs", "pub fn internal_change() {}\n")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/shared.md");
        std::fs::write(
            &change_path,
            "---\n\"lib-a\" = \"patch\"\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n",
        )?;
        ws.commit("Add reviewed shared change")?;
        publish_to_origin(&ws)?;

        let (preview, report) = release_json(&ws.path, &["lib-a", "--dry-run"])?;
        assert_eq!(preview.status.code(), Some(0), "{}", describe(&preview));
        assert_eq!(crate_names(&report), ["lib-a"]);

        let (apply, report) = release_json(&ws.path, &["lib-a", "--yes"])?;
        assert_eq!(
            apply.status.code(),
            Some(0),
            "release should retain lib-b coverage\n{}",
            describe(&apply)
        );
        assert_eq!(report["result"], "released");
        let retained = "---\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n";
        assert_eq!(std::fs::read_to_string(&change_path)?, retained);
        assert_eq!(committed(&ws.path, ".changes/shared.md")?, retained);

        let coverage = release(&ws.path, &["lib-b", "--dry-run"])?;
        assert!(
            coverage.status.success(),
            "retained no-release intent should continue to cover lib-b\n{}",
            describe(&coverage)
        );
        assert!(
            stdout(&coverage).contains("nothing to release"),
            "{}",
            describe(&coverage)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn reviewed_changes_require_repository_wide_coverage_by_default() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-default-change-coverage")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("unstructured commit subject")?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--dry-run"])?;
        let combined = combined(&output);
        assert!(!output.status.success(), "{}", combined);
        assert!(combined.contains("missing change files"), "{}", combined);
        assert!(!combined.contains("not a conventional commit"), "{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_requires_the_reviewed_change_entry_to_be_committed() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-apply-bound-change-entry")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        let origin = publish_to_origin(&ws)?;
        let initial = head(&ws.path)?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/untracked-reviewed.md");
        std::fs::write(
            &change_path,
            "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch from a committed change entry.\n",
        )?;

        let refused = release(&ws.path, &["lib-a", "--bump", "auto", "--yes"])?;
        let output = combined(&refused);
        assert_eq!(refused.status.code(), Some(2), "{output}");
        assert!(
            output.contains("a release starts from a clean worktree") && output.contains(".changes/"),
            "an untracked change entry must be committed before it can release\n{output}"
        );
        assert!(change_path.exists());
        assert_eq!(head(&ws.path)?, initial);
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );

        commit_and_push(&ws, "Review the patch")?;
        let output = release(&ws.path, &["lib-a", "--bump", "auto", "--yes"])?;
        assert!(
            output.status.success(),
            "the committed change entry should release\n{}",
            describe(&output)
        );
        assert!(!change_path.exists());
        assert!(!committed_exists(&ws.path, ".changes/untracked-reviewed.md"));
        let manifest = committed(&ws.path, "crates/lib-a/Cargo.toml")?;
        assert!(manifest.contains("version = \"0.1.1\""), "{}", manifest);
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.path)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_apply_rejects_unrelated_dirt_before_the_first_write() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-apply-unrelated-dirt")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/reviewed.md");
        std::fs::write(&change_path, "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch.\n")?;
        let origin = publish_to_origin(&ws)?;
        std::fs::write(ws.path.join("UNRELATED.md"), "unbound operator dirt\n")?;
        let initial = head(&ws.path)?;

        let output = release(&ws.path, &["lib-a", "--bump", "auto", "--yes"])?;
        let combined = combined(&output);
        assert!(!output.status.success(), "{}", combined);
        assert!(
            combined.contains("a release starts from a clean worktree"),
            "{}",
            combined
        );
        assert!(combined.contains("UNRELATED.md"), "{}", combined);
        assert!(change_path.exists());
        assert_eq!(head(&ws.path)?, initial);
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        assert!(!local_tags(&ws.path)?.contains(&"lib-a-v0.1.1".to_owned()));

        let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        assert!(manifest.contains("version = \"0.1.0\""), "{}", manifest);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_uses_reviewed_bumps_per_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-bump")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "1.2.3", &[])?;
        commit_with_lockfile(&ws, "Add release crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "1.2.3")?;

        write_test_change_levels(&ws.path, &[("lib-a", "minor"), ("lib-b", "patch")])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "auto"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("0.1.0 -> 0.2.0"),
            "reviewed minor intent should produce a minor bump\nstdout:\n{}",
            text
        );
        assert!(
            text.contains("1.2.3 -> 1.2.4"),
            "reviewed patch intent should produce a patch bump\nstdout:\n{}",
            text
        );
        assert!(
            text.contains("auto: reviewed change files"),
            "plan should explain auto bump source\nstdout:\n{}",
            text
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_honors_pre_1_major_policy() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-pre1-major")?;
        write_release_config(&ws, "pre_1_breaking_bump = \"major\"")?;

        ws.add_crate("lib-a", "0.3.1", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.3.1")?;
        write_test_change_levels(&ws.path, &[("lib-a", "major")])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("0.3.1 -> 1.0.0"),
            "pre_1_breaking_bump = major should graduate to 1.0.0\nstdout:\n{}",
            text
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- API checks -------------------------------------------------------------------------------

/// Run cargo-rail with a `cargo` shim that answers `cargo semver-checks check-release` with
/// `check_release_script` and logs every checker invocation. Other Cargo commands run Cargo.
///
/// Returns the output and the checker log.
#[cfg(unix)]
fn run_with_semver_shim(
    ws: &TestWorkspace,
    registry: &EmptyRegistry,
    check_release_script: &str,
    args: &[&str],
) -> Result<(Output, String)> {
    use std::os::unix::fs::PermissionsExt;

    let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
    let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
    let shim_dir = tempfile::TempDir::new()?;
    let log = shim_dir.path().join("semver-checks.log");
    let shim = shim_dir.path().join("cargo");
    std::fs::write(
        &shim,
        format!(
            r#"#!/bin/sh
if [ "$1" = "semver-checks" ]; then
  printf '%s\n' "$*" >> "{}"
fi
if [ "$1" = "semver-checks" ] && [ "$2" = "--version" ]; then
  echo "cargo-semver-checks 0.99.0"
  exit 0
fi
if [ "$1" = "semver-checks" ] && [ "$2" = "check-release" ]; then
  {}
fi
exec "{}" "$@"
"#,
            log.display(),
            check_release_script,
            real_cargo
        ),
    )?;
    let mut perms = std::fs::metadata(&shim)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms)?;

    let path = format!(
        "{}:{}",
        shim_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut arguments = vec!["rail", "release"];
    arguments.extend_from_slice(args);
    let output = cargo_rail_command(&ws.path)?
        .env("PATH", path)
        .envs(registry.environment())
        .args(arguments)
        .output()?;
    let log = std::fs::read_to_string(&log).unwrap_or_default();
    Ok((output, log))
}

/// A published library crate whose release goes to the loopback registry, so API checks apply.
#[cfg(unix)]
fn semver_shim_workspace(name: &str, policy: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named(name)?;
    ws.write_release_config(&format!(
        "tag_format = \"{{crate}}-v{{version}}\"\nsemver_check = \"{policy}\"\npublish = \"{}\"\n",
        EmptyRegistry::NAME
    ))?;

    ws.add_crate("lib-a", "1.2.3", &[])?;
    commit_with_lockfile(&ws, "Add lib-a")?;
    ws.tag("lib-a-v1.2.3", "Initial release")?;
    ws.modify_file("lib-a", "src/lib.rs", "pub fn doc_only_bump_signal() {}\n")?;
    ws.commit("docs: update public API notes")?;
    Ok(ws)
}

#[cfg(unix)]
const BREAKING_CHECK: &str = r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#;

#[cfg(unix)]
#[test]
fn release_plan_blocks_when_semver_checks_exceeds_reviewed_intent() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-auto-semver-checks", "warn")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"minor\"\n---\n\nReviewed a non-breaking API change.\n",
        )?;
        let origin = publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let (output, log) = run_with_semver_shim(
            &ws,
            &registry,
            BREAKING_CHECK,
            &["lib-a", "--dry-run", "--bump", "auto"],
        )?;
        let combined = combined(&output);
        assert_eq!(output.status.code(), Some(2), "{}", combined);
        assert!(log.contains("check-release"), "the API check did not run: {log}");
        assert!(combined.contains("requires a major release"), "{}", combined);
        assert!(combined.contains("revise the reviewed change entry"), "{}", combined);
        assert!(!combined.contains("1.2.3 -> 2.0.0"), "{}", combined);
        assert!(!local_tags(&ws.path)?.contains(&"lib-a-v1.3.0".to_owned()));
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.path)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_accepts_semver_breakage_covered_by_reviewed_major_intent() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-semver-reviewed-major", "warn")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"major\"\n---\n\nReviewed breaking API change.\n",
        )?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let (output, log) = run_with_semver_shim(&ws, &registry, BREAKING_CHECK, &["lib-a", "--dry-run"])?;
        let text = stdout(&output);
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert!(log.contains("check-release"), "the API check did not run: {log}");
        assert!(text.contains("1.2.3 -> 2.0.0"), "{}", text);
        assert!(text.contains("reviewed change files -> major"), "{}", text);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_api_evidence_binds_baseline_and_blocks_required_unavailability() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-api-evidence", "warn")?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;
        let baseline = rev_parse(&ws.path, "lib-a-v1.2.3^{}")?;
        let args = ["lib-a", "--bump", "patch", "--dry-run"];
        let script = format!(
            r#"
[ "$5" = "--baseline-rev" ] && [ "$6" = "{baseline}" ] && [ "$7" = "--default-features" ] && [ "$8" = "--target" ] && [ -n "$9" ] || {{ echo 'wrong comparison scope' >&2; exit 1; }}
echo 'fixture API evidence is unavailable' >&2
exit 1
"#
        );

        // Unavailable evidence does not block under the warn policy.
        let (optional, log) = run_with_semver_shim(&ws, &registry, &script, &args)?;
        assert_eq!(optional.status.code(), Some(0), "{}", describe(&optional));
        assert!(
            log.contains(&format!("--baseline-rev {baseline} --default-features --target ")),
            "the API check must compare the previous release tag's commit with default features: {log}"
        );

        ws.write_release_config(&format!(
            "tag_format = \"{{crate}}-v{{version}}\"\nsemver_check = \"deny\"\npublish = \"{}\"\n",
            EmptyRegistry::NAME
        ))?;
        commit_and_push(&ws, "Require API evidence")?;
        let (required, _) = run_with_semver_shim(&ws, &registry, &script, &args)?;
        assert_eq!(required.status.code(), Some(2), "{}", describe(&required));
        assert!(
            combined(&required).contains("fixture API evidence is unavailable"),
            "{}",
            describe(&required)
        );
        let (missing, _) = run_with_semver_shim(
            &ws,
            &registry,
            "echo 'no such command: semver-checks' >&2; exit 101",
            &args,
        )?;
        assert_eq!(missing.status.code(), Some(2), "{}", describe(&missing));
        assert!(combined(&missing).contains("no such command: semver-checks"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_auto_ignores_inconclusive_semver_checks() {
    let result: Result<()> = (|| {
        // A non-zero exit without the breaking-summary marker is an operational
        // failure (first release: no baseline on crates.io) — never an escalation.
        let ws = semver_shim_workspace("release-auto-semver-inconclusive", "warn")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/no-release.md"),
            "---\n\"lib-a\" = \"none\"\n---\n\nReviewed documentation-only change.\n",
        )?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let (output, _) = run_with_semver_shim(
            &ws,
            &registry,
            r#"echo "error: the crate lib-a has no published versions to use as a baseline" >&2
  exit 1"#,
            &["lib-a", "--dry-run", "--bump", "auto", "--format", "json"],
        )?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert!(
            !stdout(&output).contains("2.0.0"),
            "inconclusive semver-checks must not escalate the bump\n{}",
            describe(&output)
        );
        let report: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["result"], "nothing_to_release");
        assert!(
            skipped_reason(&report, "lib-a").is_some(),
            "docs-only crate should be skipped with a trace reason\n{report}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_auto_skips_semver_checks_for_unpublishable_crates() {
    let result: Result<()> = (|| {
        // publish = false crates have no registry baseline; the API check must
        // not run for them even when the checker would report breakage.
        let ws = semver_shim_workspace("release-auto-semver-unpublishable", "warn")?;
        ws.write_release_config(&format!(
            r#"tag_format = "{{crate}}-v{{version}}"
semver_check = "warn"
publish = "{}"

[crates.lib-a.release]
publish = false
"#,
            EmptyRegistry::NAME
        ))?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let (output, log) = run_with_semver_shim(
            &ws,
            &registry,
            BREAKING_CHECK,
            &["lib-a", "--dry-run", "--bump", "auto", "--format", "json"],
        )?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert!(
            !log.contains("check-release"),
            "unpublishable crates must never run API checks: {log}"
        );
        let report: Value = serde_json::from_slice(&output.stdout)?;
        let planned = crate_report(&report, "lib-a");
        assert_eq!(planned["version"], "1.2.4", "{report}");
        assert_eq!(planned["registry"], "skipped", "{report}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Skipped crates ---------------------------------------------------------------------------

#[test]
fn release_plan_auto_reports_skipped_crates_with_reason() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-skip-trace")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn only_a_changed() {}\n")?;
        ws.commit("feat: extend lib-a")?;
        write_test_change_levels(&ws.path, &[("lib-a", "minor")])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "auto"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("unchanged: lib-b"),
            "plan should list skipped crates\nstdout:\n{}",
            text
        );

        let (output, report) = release_json(&ws.path, &["--dry-run", "--bump", "auto"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(crate_names(&report), ["lib-a"]);
        let reason = skipped_reason(&report, "lib-b").unwrap_or_default();
        assert!(
            reason.contains("auto: no release-worthy changes since lib-b-v0.1.0"),
            "skip trace should name the crate and the range\n{report}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_noops_when_all_crates_are_skipped() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-noop")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        write_test_change_levels(&ws.path, &[("lib-a", "none")])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "auto"])?;
        let text = stdout(&output);
        assert!(
            output.status.success(),
            "a dry run should succeed when nothing is released\n{}",
            describe(&output)
        );
        assert!(
            text.contains("nothing to release"),
            "no-op output should explain that nothing will be released\nstdout:\n{}",
            text
        );
        assert!(
            !text.contains("every check passed"),
            "no-op output must not report a planned release\nstdout:\n{}",
            text
        );

        let (output, json) = release_json(&ws.path, &["--dry-run", "--bump", "auto"])?;
        assert!(
            output.status.success(),
            "json no-op dry run should succeed\n{}",
            describe(&output)
        );
        assert_eq!(json["result"], "nothing_to_release");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["crates"], serde_json::json!([]));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Auxiliary Cargo lockfiles ---------------------------------------------------------------

fn add_auxiliary_cargo_workspace(ws: &TestWorkspace, name: &str, dependency: &str) -> Result<PathBuf> {
    add_auxiliary_cargo_workspace_with_dependencies(ws, name, &[(dependency, "..")])
}

fn add_auxiliary_cargo_workspace_with_dependencies(
    ws: &TestWorkspace,
    name: &str,
    dependencies: &[(&str, &str)],
) -> Result<PathBuf> {
    let root = ws.path.join(name);
    std::fs::create_dir_all(root.join("src"))?;
    let dependencies = dependencies
        .iter()
        .map(|(dependency, path)| format!("{dependency} = {{ path = {} }}\n", toml_edit::Value::from(*path)))
        .collect::<String>();
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
{dependencies}
"#,
        ),
    )?;
    std::fs::write(root.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
    generate_lockfile(&root)?;
    Ok(root)
}

const AUXILIARY_RELEASE_CONFIG: &str = r#"tag_format = "v{version}"
publish = "none"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#;

fn configured_auxiliary_release(crate_name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    // `aux` is a reserved DOS device name; keep this shared fixture portable.
    add_auxiliary_cargo_workspace(&ws, "auxiliary", crate_name)?;
    ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
    write_test_change(&ws.path, &[crate_name])?;
    commit_with_lockfile(&ws, "Configure auxiliary Cargo release projection")?;
    ws.with_origin()?;
    Ok(ws)
}

fn check_auxiliary_release(ws: &TestWorkspace) -> Result<Output> {
    release(&ws.path, &["--dry-run", "--bump", "patch"])
}

/// Assert that a release refused a worktree whose `path` differs from HEAD, before any effect.
fn assert_refused_dirty_release(ws: &TestWorkspace, output: &Output, path: &str, initial_head: &str) -> Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::ensure!(
        !output.status.success(),
        "release accepted a dirty {path}\n{}",
        describe(output)
    );
    anyhow::ensure!(
        stderr.contains("a release starts from a clean worktree") && stderr.contains(path),
        "release must name the differing path {path}\n{stderr}"
    );
    anyhow::ensure!(
        head(&ws.path)? == initial_head,
        "release moved HEAD from {initial_head}"
    );
    anyhow::ensure!(
        std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""),
        "release changed the package version"
    );
    Ok(())
}

fn assert_only_crlf(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.windows(2).any(|window| window == b"\r\n"),
        "{} was not CRLF",
        path.display()
    );
    anyhow::ensure!(
        bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte != b'\n' || index > 0 && bytes[index - 1] == b'\r'),
        "{} contains a non-CRLF newline",
        path.display()
    );
    Ok(())
}

fn assert_external_auxiliary_dependency_rejected(absolute: bool) -> Result<()> {
    let crate_name = if absolute { "aux-absolute" } else { "aux-escaping" };
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    let external = tempfile::TempDir::new_in(
        ws.path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test workspace has no parent"))?,
    )?;
    std::fs::create_dir_all(external.path().join("src"))?;
    std::fs::write(
        external.path().join("Cargo.toml"),
        r#"[package]
name = "external-path-package"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    std::fs::write(external.path().join("src/lib.rs"), "pub fn external() {}\n")?;
    let dependency_path = if absolute {
        external.path().to_path_buf()
    } else {
        PathBuf::from("../..").join(
            external
                .path()
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("external package has no file name"))?,
        )
    };
    let dependency_path = dependency_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("external package path is not UTF-8"))?;
    add_auxiliary_cargo_workspace_with_dependencies(&ws, "auxiliary", &[("external-path-package", dependency_path)])?;
    ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
    write_test_change(&ws.path, &[crate_name])?;
    publish_to_origin(&ws)?;
    let initial_head = head(&ws.path)?;

    let check = check_auxiliary_release(&ws)?;
    anyhow::ensure!(
        !check.status.success(),
        "release dry run unexpectedly accepted an external auxiliary dependency\n{}",
        describe(&check)
    );
    let stderr = String::from_utf8_lossy(&check.stderr);
    if absolute {
        anyhow::ensure!(
            stderr.contains("outside the captured source") && stderr.contains(&external.path().display().to_string()),
            "{stderr}"
        );
    } else {
        anyhow::ensure!(stderr.contains("cargo metadata --locked failed"), "{stderr}");
    }
    let final_head = head(&ws.path)?;
    anyhow::ensure!(
        final_head == initial_head,
        "release dry run moved HEAD from {initial_head} to {final_head}"
    );
    Ok(())
}

#[test]
fn release_plans_and_commits_exact_auxiliary_cargo_lockfiles() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-release", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux-one", "aux-release")?;
        add_auxiliary_cargo_workspace(&ws, "aux-two", "aux-release")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
publish = "none"
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-release"])?;
        let origin = publish_to_origin(&ws)?;
        let before = ["aux-one", "aux-two"]
            .map(|name| std::fs::read(ws.path.join(name).join("Cargo.lock")))
            .into_iter()
            .collect::<std::io::Result<Vec<_>>>()?;

        let check = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert!(check.status.success(), "{}", describe(&check));
        for (name, before) in ["aux-one", "aux-two"].into_iter().zip(&before) {
            assert_eq!(
                &std::fs::read(ws.path.join(name).join("Cargo.lock"))?,
                before,
                "a dry run changed {name}/Cargo.lock"
            );
        }

        let apply = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(apply.status.success(), "release apply failed\n{}", describe(&apply));
        for (name, before) in ["aux-one", "aux-two"].into_iter().zip(&before) {
            let lockfile = std::fs::read(ws.path.join(name).join("Cargo.lock"))?;
            assert_ne!(&lockfile, before, "{name} lockfile was not projected");
            let committed = git(&ws.path, &["show", &format!("HEAD:{name}/Cargo.lock")])?;
            assert_eq!(committed.stdout, lockfile, "{name} lockfile was not committed exactly");
            let text = String::from_utf8(lockfile)?;
            assert!(text.contains("name = \"aux-release\"\nversion = \"0.1.1\""));
        }
        let status = git(&ws.path, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "release left unstaged paths");
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.path)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_updates_all_packages_in_one_auxiliary_cargo_invocation() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new()?;
        ws.add_crate("dual-release-one", "0.1.0", &[])?;
        ws.add_crate("dual-release-two", "0.1.0", &[])?;
        generate_lockfile(&ws.path)?;
        add_auxiliary_cargo_workspace_with_dependencies(
            &ws,
            "aux-dual",
            &[
                ("dual-release-one", "../crates/dual-release-one"),
                ("dual-release-two", "../crates/dual-release-two"),
            ],
        )?;
        ws.write_release_config(
            r#"tag_format = "{crate}-v{version}"
publish = "none"
auxiliary_cargo_manifests = ["aux-dual/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["dual-release-one", "dual-release-two"])?;
        publish_to_origin(&ws)?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let marker = wrapper_dir.path().join("update-called");
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = update ]; then\ncase \"$3\" in */aux-dual/Cargo.toml)\n  if [ -e \"{}\" ]; then exit 97; fi\n  printf 'one\\n' > \"{}\"\n;; esac\nfi\nexec \"{}\" \"$@\"\n",
                marker.display(),
                marker.display(),
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args(["rail", "release", "--dry-run", "--bump", "patch"])
            .output()?;
        assert!(check.status.success(), "{}", describe(&check));
        assert_eq!(std::fs::read_to_string(&marker)?, "one\n");

        std::fs::remove_file(&marker)?;
        let apply = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args(["rail", "release", "--bump", "patch", "--yes"])
            .output()?;
        assert!(apply.status.success(), "{}", describe(&apply));
        assert_eq!(std::fs::read_to_string(&marker)?, "one\n");
        let projected = committed(&ws.path, "aux-dual/Cargo.lock")?;
        assert!(projected.contains("name = \"dual-release-one\"\nversion = \"0.1.1\""));
        assert!(projected.contains("name = \"dual-release-two\"\nversion = \"0.1.1\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_accepts_git_clean_crlf_checkout() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-crlf", "0.1.0")?;
        git(&ws.path, &["config", "core.autocrlf", "true"])?;
        std::fs::write(
            ws.path.join(".gitattributes"),
            "**/Cargo.toml text eol=crlf\n**/Cargo.lock text eol=crlf\n",
        )?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-crlf")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-crlf"])?;
        commit_with_lockfile(&ws, "Configure a CRLF auxiliary Cargo projection")?;
        ws.with_origin()?;

        for path in ["auxiliary/Cargo.toml", "auxiliary/Cargo.lock"] {
            std::fs::remove_file(ws.path.join(path))?;
        }
        git(
            &ws.path,
            &[
                "checkout-index",
                "--force",
                "--",
                "auxiliary/Cargo.toml",
                "auxiliary/Cargo.lock",
            ],
        )?;
        git(&ws.path, &["add", "--", "auxiliary/Cargo.toml", "auxiliary/Cargo.lock"])?;
        for path in ["auxiliary/Cargo.toml", "auxiliary/Cargo.lock"] {
            assert_only_crlf(&ws.path.join(path))?;
        }
        let status = git(&ws.path, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "CRLF checkout is not Git-clean");

        let check = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert!(check.status.success(), "{}", describe(&check));
        let status = git(&ws.path, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "planning changed the CRLF checkout");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_accepts_git_clean_crlf_in_nested_workspace() {
    let result: Result<()> = (|| {
        let ws = NestedWorkspace::new("rust")?;
        git(&ws.git_root, &["config", "core.autocrlf", "true"])?;
        std::fs::write(
            ws.git_root.join(".gitattributes"),
            "rust/auxiliary/Cargo.toml text eol=crlf\nrust/auxiliary/Cargo.lock text eol=crlf\n",
        )?;
        ws.add_crate("nested-crlf", "0.1.0")?;
        let auxiliary = ws.workspace_root.join("auxiliary");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            r#"[package]
name = "nested-crlf-aux"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
nested-crlf = { path = "../crates/nested-crlf" }
"#,
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
        for workspace in [&ws.workspace_root, &auxiliary] {
            generate_lockfile(workspace)?;
        }
        std::fs::write(
            ws.workspace_root.join(".config/rail.toml"),
            format!("[release]\n{AUXILIARY_RELEASE_CONFIG}"),
        )?;
        write_test_change(&ws.workspace_root, &["nested-crlf"])?;
        ws.commit("Configure a nested CRLF auxiliary Cargo projection")?;
        publish_nested_to_origin(&ws)?;

        for path in ["rust/auxiliary/Cargo.toml", "rust/auxiliary/Cargo.lock"] {
            std::fs::remove_file(ws.git_root.join(path))?;
        }
        git(
            &ws.git_root,
            &[
                "checkout-index",
                "--force",
                "--",
                "rust/auxiliary/Cargo.toml",
                "rust/auxiliary/Cargo.lock",
            ],
        )?;
        git(
            &ws.git_root,
            &["add", "--", "rust/auxiliary/Cargo.toml", "rust/auxiliary/Cargo.lock"],
        )?;
        for path in ["rust/auxiliary/Cargo.toml", "rust/auxiliary/Cargo.lock"] {
            assert_only_crlf(&ws.git_root.join(path))?;
        }
        let status = git(&ws.git_root, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "nested CRLF checkout is not Git-clean");

        let check = release(&ws.workspace_root, &["--dry-run", "--bump", "patch"])?;
        assert!(check.status.success(), "{}", describe(&check));
        let status = git(&ws.git_root, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "planning changed the nested CRLF checkout");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-manifest")?;
        let initial = head(&ws.path)?;
        let manifest = ws.path.join("auxiliary/Cargo.toml");
        let mut changed = std::fs::read_to_string(&manifest)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&manifest, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert_refused_dirty_release(&ws, &check, "auxiliary/Cargo.toml", &initial)?;

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-lock")?;
        let initial = head(&ws.path)?;
        let lockfile = ws.path.join("auxiliary/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&lockfile, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert_refused_dirty_release(&ws, &check, "auxiliary/Cargo.lock", &initial)?;

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_index_only_changes() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-index-manifest")?;
        let initial = head(&ws.path)?;
        let manifest = ws.path.join("auxiliary/Cargo.toml");
        let original = std::fs::read(&manifest)?;
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n# staged only\n");
        std::fs::write(&manifest, changed)?;
        git(&ws.path, &["add", "--", "auxiliary/Cargo.toml"])?;
        std::fs::write(&manifest, original)?;

        let check = check_auxiliary_release(&ws)?;
        assert_refused_dirty_release(&ws, &check, "auxiliary/Cargo.toml", &initial)?;

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_intent_to_add() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-intent-manifest", "0.1.0")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-intent-manifest"])?;
        publish_to_origin(&ws)?;
        let initial = head(&ws.path)?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-intent-manifest")?;
        git(
            &ws.path,
            &[
                "add",
                "--intent-to-add",
                "--",
                "auxiliary/Cargo.toml",
                "auxiliary/Cargo.lock",
            ],
        )?;

        let check = check_auxiliary_release(&ws)?;
        assert_refused_dirty_release(&ws, &check, "auxiliary/Cargo.toml", &initial)?;

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_rejects_absolute_and_escaping_path_dependencies() {
    let result: Result<()> = (|| {
        assert_external_auxiliary_dependency_rejected(true)?;
        assert_external_auxiliary_dependency_rejected(false)?;
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plans_auxiliary_lockfile_from_nested_workspace_root() {
    let result: Result<()> = (|| {
        let ws = NestedWorkspace::new("rust")?;
        ws.add_crate("nested-release", "0.1.0")?;
        let auxiliary = ws.workspace_root.join("auxiliary");
        std::fs::create_dir_all(auxiliary.join("src"))?;
        std::fs::write(
            auxiliary.join("Cargo.toml"),
            r#"[package]
name = "nested-aux"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
nested-release = { path = "../crates/nested-release" }
"#,
        )?;
        std::fs::write(auxiliary.join("src/lib.rs"), "pub fn auxiliary() {}\n")?;
        for workspace in [&ws.workspace_root, &auxiliary] {
            generate_lockfile(workspace)?;
        }
        std::fs::write(
            ws.workspace_root.join(".config/rail.toml"),
            format!("[release]\n{AUXILIARY_RELEASE_CONFIG}"),
        )?;
        write_test_change(&ws.workspace_root, &["nested-release"])?;
        ws.commit("Configure nested auxiliary Cargo projection")?;
        let origin = publish_nested_to_origin(&ws)?;
        let before = std::fs::read(auxiliary.join("Cargo.lock"))?;

        let check = release(&ws.workspace_root, &["--dry-run", "--bump", "patch"])?;
        assert!(check.status.success(), "{}", describe(&check));
        assert_eq!(std::fs::read(auxiliary.join("Cargo.lock"))?, before);

        let applied = release(&ws.workspace_root, &["--bump", "patch", "--yes"])?;
        assert!(applied.status.success(), "{}", describe(&applied));
        assert!(
            std::fs::read_to_string(ws.workspace_root.join("crates/nested-release/Cargo.toml"))?
                .contains("version = \"0.1.1\"")
        );
        let projected = std::fs::read_to_string(auxiliary.join("Cargo.lock"))?;
        assert!(projected.contains("name = \"nested-release\"\nversion = \"0.1.1\""));
        assert_eq!(committed(&ws.git_root, "rust/auxiliary/Cargo.lock")?, projected);
        assert!(!ws.git_root.join("auxiliary").exists());
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.git_root)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_plan_rejects_drift_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-drift", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-drift")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-drift"])?;
        let origin = publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;
        let check = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert!(check.status.success(), "{}", describe(&check));

        // The lockfile drifts after the preview; the release itself refuses it.
        let lockfile = ws.path.join("auxiliary/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push('\n');
        std::fs::write(&lockfile, changed)?;

        let apply = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert_refused_dirty_release(&ws, &apply, "auxiliary/Cargo.lock", &initial_head)?;
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial_head.as_str())
        );
        assert!(local_tags(&ws.path)?.is_empty());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_cargo_failure_leaves_the_worktree_untouched() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-invalid", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join("auxiliary"))?;
        std::fs::write(ws.path.join("auxiliary/Cargo.toml"), "this is not Cargo TOML\n")?;
        std::fs::write(ws.path.join("auxiliary/Cargo.lock"), "version = 4\n")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-invalid"])?;
        publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let lockfile = std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?;

        let check = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert!(!check.status.success());
        assert!(
            String::from_utf8_lossy(&check.stderr).contains("cargo locate-project failed"),
            "{}",
            describe(&check)
        );
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?, lockfile);
        assert_eq!(head(&ws.path)?, initial_head);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_auxiliary_cargo_rejects_undeclared_command_mutation() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-command-boundary", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-command-boundary")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-command-boundary"])?;
        publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let lockfile = std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = update ]; then printf 'unexpected\\n' > undeclared-by-cargo; fi\nexec \"{}\" \"$@\"\n",
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args(["rail", "release", "--dry-run", "--bump", "patch"])
            .output()?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("created undeclared paths") && stderr.contains("undeclared-by-cargo"),
            "{stderr}"
        );
        assert!(!ws.path.join("undeclared-by-cargo").exists());
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?, lockfile);
        assert_eq!(head(&ws.path)?, initial_head);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_auxiliary_cargo_rejects_late_mutation_of_bound_lockfile() {
    use std::os::unix::fs::PermissionsExt as _;

    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-bound-candidate", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux-one", "aux-bound-candidate")?;
        add_auxiliary_cargo_workspace(&ws, "aux-two", "aux-bound-candidate")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
publish = "none"
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-bound-candidate"])?;
        publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let first_lockfile = std::fs::read(ws.path.join("aux-one/Cargo.lock"))?;
        let second_lockfile = std::fs::read(ws.path.join("aux-two/Cargo.lock"))?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\"{}\" \"$@\"\nstatus=$?\nif [ \"$status\" -eq 0 ] && [ \"$1\" = update ]; then\n  case \"$*\" in\n    *aux-two/Cargo.toml*) printf '\\n# late mutation\\n' >> aux-one/Cargo.lock ;;\n  esac\nfi\nexit \"$status\"\n",
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args(["rail", "release", "--dry-run", "--bump", "patch"])
            .output()?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("mutated planned path 'aux-one/Cargo.lock' after binding its candidate bytes"),
            "{stderr}"
        );
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("aux-one/Cargo.lock"))?, first_lockfile);
        assert_eq!(std::fs::read(ws.path.join("aux-two/Cargo.lock"))?, second_lockfile);
        assert_eq!(head(&ws.path)?, initial_head);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_is_restored_after_a_rejected_push() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-recovery", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-recovery")?;
        ws.write_release_config(AUXILIARY_RELEASE_CONFIG)?;
        write_test_change(&ws.path, &["aux-recovery"])?;
        let origin = publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;
        let before = std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?;

        install_pre_push_hook(&ws, "#!/bin/sh\necho 'fixture push policy rejected' >&2\nexit 1\n")?;
        let interrupted = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(!interrupted.status.success(), "{}", describe(&interrupted));
        assert!(
            combined(&interrupted).contains("nothing was released; the checkout is back at its starting commit"),
            "{}",
            describe(&interrupted)
        );
        assert_eq!(std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?, before);
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));
        assert_eq!(head(&ws.path)?, initial_head);
        assert!(local_tags(&ws.path)?.is_empty());
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial_head.as_str())
        );

        std::fs::remove_file(ws.path.join(".git/hooks/pre-push"))?;
        let rerun = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(rerun.status.success(), "{}", describe(&rerun));
        let after = std::fs::read_to_string(ws.path.join("auxiliary/Cargo.lock"))?;
        assert!(after.contains("name = \"aux-recovery\"\nversion = \"0.1.1\""));
        assert_eq!(
            git(&ws.path, &["rev-list", "--count", &format!("{initial_head}..HEAD")])?.stdout,
            b"1\n"
        );
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.path)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- History ----------------------------------------------------------------------------------

fn shallow_clone(origin: &Path, name: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let root = tempfile::TempDir::new()?;
    let clone_path = root.path().join(name);
    let output = git_command(root.path())
        .args([
            "clone",
            "--depth",
            "1",
            &file_url(origin),
            clone_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("shallow clone path is not UTF-8"))?,
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "shallow clone failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((root, clone_path))
}

#[test]
fn release_plan_uses_reviewed_intent_in_a_shallow_clone() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-shallow-guard")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "1.2.3", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "1.2.3")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        write_test_change(&ws.path, &["lib-a"])?;
        ws.commit("Record reviewed lib-a change")?;
        let origin = publish_to_origin(&ws)?;

        let (_root, clone_path) = shallow_clone(&origin, "shallow")?;
        assert_eq!(
            String::from_utf8(git(&clone_path, &["rev-parse", "--is-shallow-repository"])?.stdout)?.trim(),
            "true"
        );

        let output = release(&clone_path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        let combined = combined(&output);
        assert_eq!(
            output.status.code(),
            Some(0),
            "reviewed intent should produce a normal dry run\n{}",
            combined
        );
        assert!(
            combined.contains("1.2.3 -> 1.2.4") && combined.contains("auto: reviewed change files -> patch"),
            "output:\n{}",
            combined
        );
        assert!(!combined.contains("complete history"), "output:\n{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_names_no_previous_tag_full_history() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-no-previous-tag")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;

        let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let reason = crate_report(&report, "lib-a")["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("no previous tag: full history"),
            "the reason should name the first-release history range\n{report}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Version groups and the dependent closure -------------------------------------------------

#[test]
fn version_group_propagates_max_auto_bump_and_surfaces_in_json() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-version-group-max")?;
        write_release_config(
            &ws,
            r#"
[release.version_groups]
core = ["lib-a", "lib-b", "lib-c"]
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.add_crate("lib-c", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add grouped crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn patch_signal() {}\n")?;
        ws.commit("fix: patch lib-a")?;
        ws.modify_file("lib-b", "src/lib.rs", "pub fn minor_signal() {}\n")?;
        ws.commit("feat: extend lib-b")?;
        write_test_change_levels(&ws.path, &[("lib-a", "patch"), ("lib-b", "minor"), ("lib-c", "none")])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "auto"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert_eq!(
            text.matches("0.1.0 -> 0.2.0").count(),
            3,
            "all group members should receive the max minor bump\n{}",
            text
        );
        assert!(
            text.contains("lib-c") && text.contains("version group core -> minor"),
            "group-only member should be planned with a group reason\n{}",
            text
        );

        let (output, json) = release_json(&ws.path, &["--dry-run", "--bump", "auto"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        for crate_name in ["lib-a", "lib-b", "lib-c"] {
            let crate_plan = crate_report(&json, crate_name);
            assert_eq!(crate_plan["version"], "0.2.0", "{crate_name}: {json}");
            assert!(
                crate_plan["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("version group core -> minor")),
                "{crate_name}: {json}"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn version_group_partial_selection_expands_to_the_whole_group() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-version-group-partial")?;
        write_release_config(
            &ws,
            r#"
[release.version_groups]
core = ["lib-a", "lib-b"]
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add grouped crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn minor_signal() {}\n")?;
        ws.commit("feat: extend lib-a")?;
        write_test_change_levels(&ws.path, &[("lib-a", "minor"), ("lib-b", "none")])?;
        publish_to_origin(&ws)?;

        let expanded = release(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        let text = stdout(&expanded);
        assert!(
            expanded.status.success(),
            "expanded plan should succeed\n{}",
            describe(&expanded)
        );
        assert!(
            text.contains("lib-a") && text.contains("lib-b") && text.contains("version group core -> minor"),
            "naming one member releases the whole group\n{}",
            text
        );

        let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(crate_names(&report), ["lib-a", "lib-b"]);
        assert_eq!(crate_report(&report, "lib-b")["version"], "0.2.0");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_partial_change_file_consumption() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-partial-change-file")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;

        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/shared-change.md"),
            "---\n\"lib-a\" = \"minor\"\n\"lib-b\" = \"patch\"\n---\n\nShared behavior change.\n",
        )?;
        ws.commit("Add change file naming both crates")?;
        publish_to_origin(&ws)?;

        // Releasing only lib-a would consume the file and silently destroy
        // lib-b's pending intent — the plan must refuse.
        let output = release(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        let combined = combined(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "partial change-file consumption must be an error\n{}",
            combined
        );
        assert!(
            combined.contains("shared-change.md") && combined.contains("lib-b"),
            "error should name the file and the missing crate\noutput:\n{}",
            combined
        );

        // Releasing both crates together consumes the file cleanly.
        let output = release(&ws.path, &["--bump", "auto", "--yes"])?;
        assert!(
            output.status.success(),
            "full release should consume the change file\n{}",
            describe(&output)
        );
        assert!(
            !ws.path.join(".changes/shared-change.md").exists(),
            "change file should be consumed by the release"
        );
        assert!(!committed_exists(&ws.path, ".changes/shared-change.md"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- `cargo rail change` ----------------------------------------------------------------------

#[test]
fn change_add_and_status_support_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-json-output")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "minor",
                "--message",
                "Added a user-facing thing.",
                "--format",
                "json",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change add should succeed\n{}", stdout);
        let json: Value = serde_json::from_str(&stdout)?;
        assert_eq!(json["command"], "change");
        assert_eq!(json["mode"], "add");
        assert_eq!(json["crates"][0], "lib-a");
        assert_eq!(json["bump"], "minor");
        let created = json["path"].as_str().expect("path in payload");
        let normalized_created = created.replace('\\', "/");
        assert!(normalized_created.contains(".changes/"));
        assert!(
            created.ends_with(".md") && !created.contains("2026"),
            "created change file should use deterministic slug-hash naming: {}",
            created
        );

        let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "json"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change status should succeed\n{}", stdout);
        let json: Value = serde_json::from_str(&stdout)?;
        assert_eq!(json["command"], "change");
        assert_eq!(json["count"], 1);
        assert_eq!(json["crates"][0]["crate_name"], "lib-a");
        assert_eq!(json["crates"][0]["bump"], "minor");
        assert_eq!(json["files"][0]["intents"][0]["crate"], "lib-a");
        assert_eq!(json["files"][0]["intents"][0]["bump"], "minor");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_names_only_is_empty_without_pending_files() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-names-only-empty")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "names-only"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change status should succeed\n{}", stdout);
        assert_eq!(stdout, "", "names-only should be empty when no change files exist");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_names_only_lists_pending_change_paths() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-names-only-pending")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "minor",
                "--message",
                "Added names-only change status.",
            ],
        )?;
        assert!(
            output.status.success(),
            "change add should succeed\n{}",
            String::from_utf8_lossy(&output.stdout)
        );

        let output = run_cargo_rail(&ws.path, &["rail", "change", "status", "--format", "names-only"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change status should succeed\n{}", stdout);
        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(lines.len(), 1, "one pending file should be listed\n{}", stdout);
        assert!(
            lines[0].starts_with(".changes/"),
            "path should be workspace-relative: {}",
            lines[0]
        );
        assert!(
            lines[0].ends_with(".md"),
            "path should name a markdown change file: {}",
            lines[0]
        );
        assert!(
            !stdout.contains("no pending change files"),
            "names-only should not include human status text"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_check_fails_when_changed_crate_lacks_change_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-check-missing")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        git(&ws.path, &["branch", "origin/main"])?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
        ws.commit("Change lib-a source")?;

        let output = run_cargo_rail(&ws.path, &["rail", "change", "check", "--merge-base"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "missing change file should fail as a check result\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(stdout.contains("missing change files"), "stdout:\n{}", stdout);
        assert!(stdout.contains("lib-a"), "stdout:\n{}", stdout);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_check_passes_when_changed_crate_has_change_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-check-covered")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        git(&ws.path, &["branch", "origin/main"])?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() -> bool { true }")?;
        ws.commit("Change lib-a source")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "patch",
                "--message",
                "Documented the source change.",
            ],
        )?;
        assert!(
            output.status.success(),
            "change add should succeed\n{}",
            String::from_utf8_lossy(&output.stdout)
        );

        let output = run_cargo_rail(&ws.path, &["rail", "change", "check", "--since", "origin/main"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change check should pass\n{}", stdout);
        assert!(stdout.contains("change files: ok"), "stdout:\n{}", stdout);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_uses_stable_slug_hash_names_and_rejects_duplicate_intent() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-stable-filenames")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let args = [
            "rail",
            "change",
            "add",
            "lib-a",
            "--bump",
            "minor",
            "--message",
            "Added deterministic filenames for reviewed release intent.",
            "--format",
            "names-only",
        ];
        let output = run_cargo_rail(&ws.path, &args)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change add should succeed\n{}", stdout);
        let first_path = std::path::PathBuf::from(stdout.trim());
        let first_name = first_path.file_name().and_then(|name| name.to_str()).unwrap();
        assert!(
            first_name.starts_with("added-deterministic-filenames-") && first_name.ends_with(".md"),
            "filename should be slug-hash, got {}",
            first_name
        );
        let slug = first_name
            .trim_end_matches(".md")
            .rsplit_once('-')
            .map(|(slug, _)| slug)
            .unwrap();
        assert!(slug.len() <= 32, "slug should be capped at 32 chars: {}", first_name);
        assert!(
            first_name
                .trim_end_matches(".md")
                .rsplit_once('-')
                .is_some_and(|(_, hash)| hash.len() == 4 && hash.chars().all(|c| c.is_ascii_hexdigit())),
            "filename should end in a 4-hex hash: {}",
            first_name
        );

        let duplicate = run_cargo_rail(&ws.path, &args)?;
        let combined = combined(&duplicate);
        assert_eq!(
            duplicate.status.code(),
            Some(2),
            "duplicate intent should fail\n{}",
            combined
        );
        assert!(combined.contains("change file already exists"), "output:\n{}", combined);

        std::fs::remove_file(&first_path)?;
        let output = run_cargo_rail(&ws.path, &args)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let second_path = std::path::PathBuf::from(stdout.trim());
        assert_eq!(
            second_path.file_name(),
            first_path.file_name(),
            "same content should produce the same filename"
        );

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "patch",
                "--message",
                "Patched another thing.",
                "--name",
                "custom-name",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let custom_path = std::path::PathBuf::from(stdout.trim());
        assert!(
            custom_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("custom-name-")),
            "custom --name should override slug: {}",
            custom_path.display()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_rejects_change_dir_that_escapes_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-dir-escape")?;
        write_release_config(&ws, "change_dir = \"../outside\"")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "patch",
                "--message",
                "Should not write outside the workspace.",
            ],
        )?;
        let combined = combined(&output);
        assert_eq!(output.status.code(), Some(2), "change add should fail\n{}", combined);
        assert!(
            combined.contains("invalid configuration for 'release.change_dir'")
                && combined.contains("change_dir must be a workspace-relative path"),
            "output:\n{}",
            combined
        );
        assert!(!ws.path.parent().unwrap().join("outside").exists());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_dir_override_round_trips_through_release_consumption() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-dir-override")?;
        write_release_config(&ws, "change_dir = \"changes\"")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "minor",
                "--message",
                "Added configurable change directory.",
                "--format",
                "names-only",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "change add should succeed\n{}", stdout);
        let change_path = std::path::PathBuf::from(stdout.trim());
        assert_eq!(
            change_path
                .parent()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str()),
            Some("changes"),
            "path: {}",
            change_path.display()
        );
        assert!(change_path.exists(), "path: {}", change_path.display());

        let status = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
        let status_stdout = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_stdout.contains("lib-a: minor"),
            "status should read configured change_dir\n{}",
            status_stdout
        );
        publish_to_origin(&ws)?;

        let plan = release(&ws.path, &["lib-a", "--dry-run", "--bump", "auto"])?;
        let plan_stdout = String::from_utf8_lossy(&plan.stdout);
        assert!(
            plan_stdout.contains("0.1.0 -> 0.2.0"),
            "plan should read configured change_dir\n{}",
            describe(&plan)
        );

        let release = release(&ws.path, &["lib-a", "--bump", "auto", "--yes"])?;
        assert!(
            release.status.success(),
            "release should consume change file from configured dir\n{}",
            describe(&release)
        );
        assert!(
            !change_path.exists(),
            "release should consume {}",
            change_path.display()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_status_reports_max_bump_per_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-status-max-bump")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "patch",
                "--message",
                "Fixed first thing.",
            ],
        )?;
        run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "minor",
                "--message",
                "Added second thing.",
            ],
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("resulting bumps:") && stdout.contains("lib-a: minor (2 files)"),
            "status should report max bump across files\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_add_without_message_errors_in_non_tty() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("change-non-tty-message")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;

        let output = run_cargo_rail(&ws.path, &["rail", "change", "add", "lib-a", "--bump", "patch"])?;
        let combined = combined(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "non-tty authoring should fail\n{}",
            combined
        );
        assert!(
            combined.contains("requires --message in non-interactive mode"),
            "{}",
            combined
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn change_file_drives_auto_bump_and_is_consumed_on_release() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-change-file-auto")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;

        let add_output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "change",
                "add",
                "lib-a",
                "--bump",
                "minor",
                "--message",
                "Added reviewed release intent.",
                "--format",
                "names-only",
            ],
        )?;
        let add_stdout = String::from_utf8_lossy(&add_output.stdout);
        assert!(add_output.status.success(), "change add failed:\n{}", add_stdout);
        let change_path = std::path::PathBuf::from(add_stdout.trim());
        assert!(
            change_path.exists(),
            "change file should exist at {}",
            change_path.display()
        );

        let status_output = run_cargo_rail(&ws.path, &["rail", "change", "status"])?;
        let status_stdout = String::from_utf8_lossy(&status_output.stdout);
        assert!(status_stdout.contains("lib-a: minor"), "status:\n{}", status_stdout);
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--bump", "auto", "--yes"])?;
        assert!(
            output.status.success(),
            "release should succeed from change file\n{}",
            describe(&output)
        );
        assert!(
            !change_path.exists(),
            "release should consume {}",
            change_path.display()
        );

        let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        assert!(manifest.contains("version = \"0.2.0\""), "manifest:\n{}", manifest);
        let changelog = std::fs::read_to_string(ws.path.join("crates/lib-a/CHANGELOG.md"))?;
        assert!(
            changelog.contains("Added reviewed release intent."),
            "changelog:\n{}",
            changelog
        );
        assert_eq!(committed(&ws.path, "crates/lib-a/CHANGELOG.md")?, changelog);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_enforces_required_change_file_coverage() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-change-file-gate")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("fix: change lib-a")?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--dry-run"])?;
        let combined = combined(&output);
        assert!(
            !output.status.success(),
            "a release should fail without the required change file\n{}",
            combined
        );
        assert!(combined.contains("missing change files"), "output:\n{}", combined);
        assert!(combined.contains("lib-a"), "output:\n{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_respects_changelog_skip() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-skip-require")?;
        write_release_config(&ws, "\n[crates.internal.changelog]\nskip = true")?;

        // Crate with changes
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn change() {}")?;
        ws.commit("fix: update lib-a")?;

        // Crate with no changes
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-b")?;
        ws.tag("lib-b-v0.1.0", "Initial lib-b")?;

        // Crate marked as skip (no changelog expected)
        ws.add_crate("internal", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add internal crate")?;
        ws.tag("internal-v0.1.0", "Initial internal crate")?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(output.status.success(), "{}", describe(&output));

        assert!(
            !ws.path.join("crates/internal/CHANGELOG.md").exists(),
            "internal crate changelog should be skipped"
        );
        assert!(!committed_exists(&ws.path, "crates/internal/CHANGELOG.md"));
        assert!(local_tags(&ws.path)?.contains(&"internal-v0.1.1".to_owned()));
        for crate_name in ["lib-a", "lib-b"] {
            assert!(
                committed_exists(&ws.path, &format!("crates/{crate_name}/CHANGELOG.md")),
                "{crate_name} should keep its changelog"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Push and hooks ---------------------------------------------------------------------------

#[test]
fn release_pushes_the_commit_and_tag_atomically_with_hook_context() {
    let result: Result<()> = (|| {
        let (ws, origin) = push_release_workspace("push-release")?;

        let hook_counter = ws.path.join(".git/pre-push-count");
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
count_file="$(dirname "$0")/../pre-push-count"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "$CARGO_RAIL_TEST_INHERITED" != "from-caller" ]; then
  echo "missing inherited caller environment" >&2
  exit 1
fi
if [ "$CARGO_RAIL_RELEASE_PUSH" != "1" ]; then
  echo "missing CARGO_RAIL_RELEASE_PUSH" >&2
  exit 1
fi
if [ "$CARGO_RAIL_OPERATION" != "release" ]; then
  echo "missing CARGO_RAIL_OPERATION" >&2
  exit 1
fi
echo "release hook context accepted"
"#,
        )?;

        let trace_dir = tempfile::TempDir::new()?;
        let trace_path = trace_dir.path().join("git-trace.log");
        let output = cargo_rail_command(&ws.path)?
            .env("CARGO_RAIL_TEST_INHERITED", "from-caller")
            .env("GIT_DIR", ws.path.join("ambient-wrong-repository"))
            .env("GIT_TRACE", &trace_path)
            .args(["rail", "release", "--bump", "patch", "--yes"])
            .output()?;
        let stdout = stdout(&output);
        assert!(
            output.status.success(),
            "release should push commit and tag\n{}",
            describe(&output)
        );

        let released = head(&ws.path)?;
        assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), Some(released.clone()));
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(released));
        assert!(
            !stdout.contains("git push origin"),
            "owned push should not print manual push follow-up"
        );
        assert!(
            stdout.contains("release hook context accepted"),
            "successful hook diagnostics should stream to stdout\n{}",
            describe(&output)
        );
        let hook_runs = std::fs::read_to_string(&hook_counter)?;
        assert_eq!(
            hook_runs.trim(),
            "1",
            "one atomic push carries the branch and its tags, and preflight must not run hooks"
        );
        let trace = std::fs::read_to_string(&trace_path)?;
        assert!(
            trace.contains("push --atomic"),
            "release must retain its atomic push\n{}",
            trace
        );
        assert!(
            !trace.contains("--no-verify"),
            "cargo-rail must never bypass repository hooks\n{}",
            trace
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_a_different_origin_push_repository_before_mutation() {
    let result: Result<()> = (|| {
        let (ws, _origin) = push_release_workspace("divergent-push")?;
        let push_remote = tempfile::TempDir::new()?;
        git(push_remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["config", "remote.origin.pushurl", push_remote.path().to_str().unwrap()],
        )?;
        let head_before = head(&ws.path)?;

        let output = release(&ws.path, &["--bump", "patch", "--yes"])?;
        let combined = combined(&output);
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(
            combined.contains("origin fetches from") && combined.contains("but pushes to"),
            "{combined}"
        );
        assert_eq!(head(&ws.path)?, head_before);
        assert!(local_tags(&ws.path)?.is_empty());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_multiple_origin_push_repositories() {
    let result: Result<()> = (|| {
        let (ws, _origin) = push_release_workspace("multiple-pushes")?;
        let first = tempfile::TempDir::new()?;
        let second = tempfile::TempDir::new()?;
        for remote in [&first, &second] {
            git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
            git(
                &ws.path,
                &[
                    "config",
                    "--add",
                    "remote.origin.pushurl",
                    remote.path().to_str().unwrap(),
                ],
            )?;
        }
        let head_before = head(&ws.path)?;

        let output = release(&ws.path, &["--bump", "patch", "--yes"])?;
        let combined = combined(&output);
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(combined.contains("2 effective push URLs"), "{combined}");
        assert_eq!(head(&ws.path)?, head_before);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_hook_failure_streams_and_preserves_both_output_streams() {
    let result: Result<()> = (|| {
        let (ws, _origin) = push_release_workspace("push-hook-diagnostics")?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
echo "hook stdout: release intent was rejected"
echo "hook stderr: policy details" >&2
exit 1
"#,
        )?;

        let output = release(&ws.path, &["--bump", "patch", "--yes"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(2), "rejected release push must fail");
        assert!(
            stdout.contains("hook stdout: release intent was rejected"),
            "hook stdout should stream while Git runs\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            stderr.contains("hook stdout: release intent was rejected")
                && stderr.contains("hook stderr: policy details"),
            "the final Git error must preserve both streams\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_hook_failure_json_captures_structured_diagnostics() {
    let result: Result<()> = (|| {
        let (ws, _origin) = push_release_workspace("push-hook-json")?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
echo "hook stdout: machine-readable release rejection"
echo "hook stderr: machine-readable policy details" >&2
exit 1
"#,
        )?;

        let output = release(&ws.path, &["--bump", "patch", "--yes", "--json"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|error| panic!("release failure must remain valid JSON: {}\n{}", error, stdout));

        assert_eq!(output.status.code(), Some(2), "rejected release push must fail");
        let message = json["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("stdout:\nhook stdout: machine-readable release rejection"),
            "JSON errors must retain and label Git stdout\n{}",
            stdout
        );
        assert!(
            message.contains("stderr:\nhook stderr: machine-readable policy details"),
            "JSON errors must retain and label Git stderr\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
fn run_with_lost_push_acknowledgment(cwd: &Path, args: &[&str]) -> Result<Output> {
    use std::os::unix::fs::PermissionsExt;
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success(), "cannot locate Git");
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_owned();
    let dir = tempfile::tempdir()?;
    let wrapper = dir.path().join("git");
    let completed = dir.path().join("completed");
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
case " $* " in
  *" push "*)
    "{real_git}" "$@" || exit $?
    : > "{}"
    echo 'fixture Git transport lost acknowledgment after successful operation' >&2
    exit 1
    ;;
esac
exec "{real_git}" "$@"
"#,
            completed.display()
        ),
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(paths)?;
    let output = cargo_rail_command(cwd)?.env("PATH", path).args(args).output()?;
    anyhow::ensure!(
        completed.is_file(),
        "release did not reach the completed Git push: {output:?}"
    );
    Ok(output)
}

#[cfg(windows)]
fn run_with_lost_push_acknowledgment(cwd: &Path, args: &[&str]) -> Result<Output> {
    let located = Command::new("where.exe").arg("git.exe").output()?;
    anyhow::ensure!(located.status.success(), "cannot locate Git");
    let locations = String::from_utf8(located.stdout)?;
    let real_git = locations
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Git path is empty"))?
        .trim();
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("git_proxy.rs");
    let wrapper = dir.path().join("git.exe");
    let completed = dir.path().join("completed");
    std::fs::write(
        &source,
        format!(
            r#"
fn main() {{
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let effect = args.iter().any(|arg| arg == "push");
    let status = std::process::Command::new({real_git:?}).args(&args).status().unwrap();
    if status.success() && effect {{
        std::fs::write({completed:?}, b"").unwrap();
        eprintln!("fixture Git transport lost acknowledgment after successful operation");
        std::process::exit(1);
    }}
    std::process::exit(status.code().unwrap_or(1));
}}
"#
        ),
    )?;
    let compiled = Command::new("rustc")
        .arg(&source)
        .arg("--edition=2024")
        .arg("-o")
        .arg(&wrapper)
        .output()?;
    anyhow::ensure!(
        compiled.status.success(),
        "Git proxy compilation failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let output = cargo_rail_command(cwd)?
        .env("PATH", std::env::join_paths(paths)?)
        .args(args)
        .output()?;
    anyhow::ensure!(
        completed.is_file(),
        "release did not reach the completed Git push: {output:?}"
    );
    Ok(output)
}

#[test]
fn release_accepts_a_push_whose_acknowledgment_was_lost() {
    let result: Result<()> = (|| {
        let (ws, origin) = push_release_workspace("push-ack")?;
        let initial = head(&ws.path)?;

        let output = run_with_lost_push_acknowledgment(&ws.path, &["rail", "release", "--bump", "patch", "--yes"])?;
        assert!(
            output.status.success(),
            "origin shows the complete release, so the lost acknowledgment is success\n{}",
            describe(&output)
        );
        let released = head(&ws.path)?;
        assert_eq!(rev_parse(&ws.path, "HEAD^")?, initial);
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(released.clone()));
        assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), Some(released.clone()));

        let rerun = release(&ws.path, &["--yes"])?;
        assert!(rerun.status.success(), "{}", describe(&rerun));
        assert!(stdout(&rerun).contains("nothing to release"), "{}", describe(&rerun));
        assert_eq!(head(&ws.path)?, released);
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(released));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_restores_the_checkout_when_a_local_hook_rejects_the_push() {
    let result: Result<()> = (|| {
        let (ws, origin) = push_release_workspace("push-abort")?;
        let initial = head(&ws.path)?;
        install_pre_push_hook(&ws, "#!/bin/sh\nexit 1\n")?;

        let interrupted = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(
            !interrupted.status.success(),
            "local pre-push hook should reject release push"
        );
        assert!(
            combined(&interrupted).contains("nothing was released"),
            "{}",
            describe(&interrupted)
        );
        assert_eq!(head(&ws.path)?, initial);
        assert!(git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout.is_empty());
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));
        assert!(ws.path.join(".changes/release-test.md").exists());
        assert!(git(&ws.path, &["status", "--porcelain"])?.stdout.is_empty());
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), None);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_push_rejects_a_changed_captured_branch_tip() {
    let result: Result<()> = (|| {
        for during_push in [false, true] {
            let (ws, origin) = push_release_workspace("branch-lease")?;
            let prior = head(&ws.path)?;
            std::fs::write(
                ws.path.join("README.md"),
                "# branch-lease\n\nReviewed release request.\n",
            )?;
            let initial = commit_and_push(&ws, "Review the complete release request")?;
            if during_push {
                install_pre_push_hook(
                    &ws,
                    r#"#!/bin/sh
git --git-dir="$FIXTURE_REMOTE" update-ref refs/heads/main "$FIXTURE_PRIOR" "$FIXTURE_INITIAL"
"#,
                )?;
            } else {
                git(&origin, &["update-ref", "refs/heads/main", &prior, &initial])?;
            }
            let rejected = cargo_rail_command(&ws.path)?
                .env("FIXTURE_REMOTE", &origin)
                .env("FIXTURE_PRIOR", &prior)
                .env("FIXTURE_INITIAL", &initial)
                .args(["rail", "release", "--bump", "patch", "--yes"])
                .output()?;
            assert!(
                !rejected.status.success(),
                "a changed remote tip cannot authorize a release push\n{}",
                describe(&rejected)
            );
            if during_push {
                assert!(
                    combined(&rejected).contains("origin rejected the release push"),
                    "{}",
                    describe(&rejected)
                );
            } else {
                assert!(
                    combined(&rejected).contains("branch 'main'") && combined(&rejected).contains("origin/main"),
                    "{}",
                    describe(&rejected)
                );
                assert_eq!(head(&ws.path)?, initial);
            }
            assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(prior));
            assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), None);
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_an_existing_local_tag_for_the_new_version() {
    let result: Result<()> = (|| {
        let (ws, origin) = push_release_workspace("resume-tag-owner")?;
        let initial = head(&ws.path)?;
        git(
            &ws.path,
            &["tag", "-a", "v0.1.1", &initial, "-m", "Independent tag replacement"],
        )?;
        let replacement = rev_parse(&ws.path, "refs/tags/v0.1.1")?;

        let rejected = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(!rejected.status.success(), "{}", describe(&rejected));
        assert!(
            combined(&rejected).contains("tag v0.1.1 already exists"),
            "{}",
            describe(&rejected)
        );
        assert_eq!(rev_parse(&ws.path, "refs/tags/v0.1.1")?, replacement);
        assert_eq!(head(&ws.path)?, initial);
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), None);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_commit_records_its_crates_in_a_trailer() {
    let result: Result<()> = (|| {
        let (ws, origin) = push_release_workspace("release-transaction")?;
        let released = release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(released.status.success(), "{}", describe(&released));

        let message = String::from_utf8(git(&ws.path, &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(
            message.starts_with("chore(release): release-transaction v0.1.1\n"),
            "{message}"
        );
        let trailer = String::from_utf8(
            git(
                &ws.path,
                &["log", "-1", "--format=%(trailers:key=Rail-Release-Crates,valueonly)"],
            )?
            .stdout,
        )?;
        assert_eq!(
            trailer.trim(),
            "release-transaction@0.1.1",
            "the release commit must name its crates and versions"
        );
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&ws.path)?));

        let rerun = release(&ws.path, &["--yes"])?;
        assert!(rerun.status.success(), "{}", describe(&rerun));
        assert!(stdout(&rerun).contains("nothing to release"), "{}", describe(&rerun));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- JSON, configuration, and publishability -------------------------------------------------

#[test]
fn test_release_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;
        ws.write_release_config("publish = \"none\"\n")?;
        write_test_change(&ws.path, &["json-release"])?;
        publish_to_origin(&ws)?;

        let (output, json) = release_json(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "a dry run exits 0 when every check passes\n{}",
            describe(&output)
        );
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["command"], "release");
        assert_eq!(json["mode"], "dry_run");
        assert_eq!(json["result"], "planned");
        assert_eq!(json["state"], "planned");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["branch"], "main");
        assert!(
            json.get("commit").is_none(),
            "a dry run reports no release commit: {json}"
        );
        let planned = crate_report(&json, "json-release");
        assert_eq!(planned["previous_version"], "0.1.0");
        assert_eq!(planned["version"], "0.1.1");
        assert_eq!(planned["tag"], "json-release-v0.1.1");
        assert_eq!(planned["reason"], "explicit");
        assert!(
            planned["notes"]
                .as_str()
                .is_some_and(|notes| notes.contains("Exercise the current release contract.")),
            "{json}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_uses_the_config_override() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-config-override", "0.1.0")?;
        ws.write_release_config("tag_format = \"custom-v{version}\"\npublish = \"none\"\n")?;
        let config = ws.path.join("custom-rail.toml");
        std::fs::rename(ws.path.join(".config/rail.toml"), &config)?;
        write_test_change(&ws.path, &["release-config-override"])?;
        publish_to_origin(&ws)?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--config",
                "custom-rail.toml",
                "release",
                "--dry-run",
                "--format",
                "json",
                "--bump",
                "patch",
            ],
        )?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            crate_report(&json, "release-config-override")["tag"],
            "custom-v0.1.1",
            "the release must use the --config override: {json}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_with_publish_none_plans_no_registry_or_github_effects() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("local-release-authority", "0.1.0")?;
        ws.write_release_config("publish = \"none\"\n")?;
        write_test_change(&ws.path, &["local-release-authority"])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "patch"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(text.contains("not published"), "{text}");
        assert!(text.contains("pushes main with the tags"), "{text}");
        assert!(
            !text.contains("publishes to"),
            "a release without a registry publishes nothing: {text}"
        );
        assert!(
            !text.contains("GitHub release"),
            "a local origin creates no GitHub release: {text}"
        );

        let (output, json) = release_json(&ws.path, &["--dry-run", "--bump", "patch"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let planned = crate_report(&json, "local-release-authority");
        assert_eq!(planned["registry"], "skipped");
        assert_eq!(planned["github_release"], "skipped");
        assert!(json.get("registry").is_none(), "{json}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn cargo_publish_authority_cannot_be_widened_by_rail_config() {
    let result: Result<()> = (|| {
        for (name, publish_line) in [
            ("manifest-private", "publish = false"),
            ("registry-private", "publish = [\"private\"]"),
        ] {
            let ws = TestWorkspace::new_single_crate(name, "0.1.0")?;
            let manifest_path = ws.path.join("Cargo.toml");
            let manifest = std::fs::read_to_string(&manifest_path)?;
            let manifest = manifest.replacen("edition = \"2021\"", &format!("edition = \"2021\"\n{publish_line}"), 1);
            anyhow::ensure!(manifest.contains(publish_line));
            std::fs::write(&manifest_path, manifest)?;
            // Crates.io is the default registry; rail.toml asks to publish anyway.
            ws.write_release_config(&format!(
                "semver_check = \"off\"\n\n[crates.{name}.release]\npublish = true\n"
            ))?;
            std::fs::create_dir_all(ws.path.join(".changes"))?;
            std::fs::write(
                ws.path.join(".changes/publish-authority.md"),
                format!("---\n\"{name}\" = \"patch\"\n---\n\nExercise Cargo publication authority.\n"),
            )?;
            publish_to_origin(&ws)?;

            let (output, json) = release_json(&ws.path, &[name, "--dry-run", "--bump", "patch"])?;
            assert_eq!(output.status.code(), Some(0), "{name}: {}", describe(&output));
            assert_eq!(crate_names(&json), [name]);
            assert_eq!(json["registry"], "crates-io");
            assert_eq!(
                crate_report(&json, name)["registry"],
                "skipped",
                "Cargo registry authority was widened for {name}: {json}"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test explicit version bump (e.g., "1.2.3" instead of "patch")
#[test]
fn test_release_explicit_version() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("explicit-ver", "0.1.0")?;
        ws.write_release_config("publish = \"none\"\n")?;
        write_test_change(&ws.path, &["explicit-ver"])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run", "--bump", "2.0.0"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("0.1.0 -> 2.0.0"),
            "Should show explicit version in plan.\nOutput:\n{}",
            text
        );
        let (_, json) = release_json(&ws.path, &["--dry-run", "--bump", "2.0.0"])?;
        assert_eq!(crate_report(&json, "explicit-ver")["version"], "2.0.0");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Changelog paths and shapes ---------------------------------------------------------------

/// A workspace with lib-a released at 0.1.0, a feature since, and reviewed patch intent.
fn changelog_workspace(name: &str, changelog_config: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named(name)?;
    write_release_config(&ws, changelog_config)?;
    ws.add_crate("lib-a", "0.1.0", &[])?;
    commit_with_lockfile(&ws, "Add lib-a")?;
    ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
    ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
    ws.commit("feat: add v2 function")?;
    write_test_change(&ws.path, &["lib-a"])?;
    Ok(ws)
}

/// Test default changelog relative_to behavior (crate-relative)
#[test]
fn test_changelog_relative_to_crate_default() {
    let result: Result<()> = (|| {
        // Don't set relative_to - should default to "crate"
        let ws = changelog_workspace("changelog-crate-rel", "[release.changelog]\npath = \"CHANGELOG.md\"\n")?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(output.status.success(), "release should succeed\n{}", describe(&output));

        // Changelog should be at crates/lib-a/CHANGELOG.md (crate-relative)
        let crate_changelog = ws.path.join("crates/lib-a/CHANGELOG.md");
        let workspace_changelog = ws.path.join("CHANGELOG.md");

        assert!(
            crate_changelog.exists(),
            "Changelog should exist at crate-relative path: {}",
            crate_changelog.display()
        );
        assert!(
            !workspace_changelog.exists(),
            "Changelog should NOT exist at workspace root when using crate-relative"
        );
        assert!(committed(&ws.path, "crates/lib-a/CHANGELOG.md")?.contains("## [0.1.1]"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Publish `ws` to a bare `origin` that Git and Cargo-Rail identify as `git@github.com:org/repo.git`.
///
/// A fixture SSH command serves the GitHub URL from the local bare repository.
#[cfg(unix)]
fn publish_to_github_origin(ws: &TestWorkspace) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    commit_with_lockfile(ws, "Prepare the release fixture")?;
    let origin = ws.path.join("target/origin.git");
    std::fs::create_dir_all(&origin)?;
    git(&origin, &["init", "--bare", "--initial-branch=main"])?;
    let ssh = ws.path.join("target/ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\ncase \"$*\" in *git-receive-pack*) exec git-receive-pack '{}' ;; *git-upload-pack*) exec git-upload-pack '{}' ;; esac\nexit 1\n",
            origin.display(),
            origin.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))?;
    ws.set_remote("git@github.com:org/repo.git")?;
    git(
        &ws.path,
        &[
            "config",
            "core.sshCommand",
            ssh.to_str().context("SSH path is not UTF-8")?,
        ],
    )?;
    git(
        &ws.path,
        &["push", "--quiet", "--set-upstream", "origin", "HEAD:main", "--tags"],
    )?;
    Ok(origin)
}

#[cfg(unix)]
#[test]
fn compare_linked_changelog_round_trips_through_preview_and_apply() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("compare-linked-changelog")?;
        // Origin is on GitHub; this release creates no GitHub release and waits for no checks.
        write_release_config(&ws, "github_release = false\nchecks = \"none\"")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        let changelog_path = ws.path.join("crates/lib-a/CHANGELOG.md");
        std::fs::write(
            &changelog_path,
            "# Changelog\n\n## [0.1.0](https://github.com/org/repo/compare/lib-a-v0.0.0...lib-a-v0.1.0) - 2026-01-01\n\n- Initial release.\n",
        )?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        write_test_change(&ws.path, &["lib-a"])?;
        let origin = publish_to_github_origin(&ws)?;

        let preview = release(&ws.path, &["lib-a", "--bump", "patch", "--dry-run"])?;
        assert!(preview.status.success(), "{}", describe(&preview));

        let apply = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(apply.status.success(), "release apply failed\n{}", describe(&apply));
        let expected = "## [0.1.1](https://github.com/org/repo/compare/lib-a-v0.1.0...lib-a-v0.1.1) - ";
        let applied = std::fs::read_to_string(changelog_path)?;
        assert!(applied.contains(expected), "applied changelog:\n{applied}");
        assert!(applied.contains("## [0.1.0](https://github.com/org/repo/compare/"));
        assert_eq!(committed(&ws.path, "crates/lib-a/CHANGELOG.md")?, applied);
        assert_eq!(ref_commit(&origin, "refs/tags/lib-a-v0.1.1"), Some(head(&ws.path)?));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "workspace" creates changelog at workspace root
#[test]
fn test_changelog_relative_to_workspace() {
    let result: Result<()> = (|| {
        // Explicitly set relative_to = "workspace"
        let ws = changelog_workspace(
            "changelog-ws-rel",
            "[release.changelog]\npath = \"CHANGELOG.md\"\nrelative_to = \"workspace\"\n",
        )?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(output.status.success(), "release should succeed\n{}", describe(&output));

        // Changelog should be at workspace root (workspace-relative)
        let workspace_changelog = ws.path.join("CHANGELOG.md");
        let crate_changelog = ws.path.join("crates/lib-a/CHANGELOG.md");

        assert!(
            workspace_changelog.exists(),
            "Changelog should exist at workspace root: {}",
            workspace_changelog.display()
        );
        assert!(
            !crate_changelog.exists(),
            "Changelog should NOT exist at crate directory when using workspace-relative"
        );

        // Verify changelog content
        let content = std::fs::read_to_string(&workspace_changelog)?;
        assert!(
            content.contains("## [0.1.1]"),
            "Changelog should contain release info. Content:\n{}",
            content
        );
        assert_eq!(committed(&ws.path, "CHANGELOG.md")?, content);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_an_absolute_changelog_path_outside_the_workspace() {
    let result: Result<()> = (|| {
        let outside = tempfile::TempDir::new()?;
        let outside_path = outside.path().join("CHANGELOG.md");
        let ws = changelog_workspace(
            "changelog-outside-workspace",
            &format!(
                "[release.changelog]\npath = \"{}\"\nrelative_to = \"workspace\"\n",
                outside_path.display().to_string().replace('\\', "\\\\")
            ),
        )?;
        let origin = publish_to_origin(&ws)?;
        let initial = head(&ws.path)?;

        for args in [
            &["lib-a", "--bump", "patch", "--dry-run"][..],
            &["lib-a", "--bump", "patch", "--yes"][..],
        ] {
            let output = release(&ws.path, args)?;
            assert!(!output.status.success(), "{args:?}: {}", describe(&output));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("outside the repository"),
                "outside changelog path should fail before mutation\nstderr:\n{}",
                stderr
            );
            assert!(!outside_path.exists());
            assert_eq!(head(&ws.path)?, initial);
        }
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_rejects_a_symlink_changelog_path() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::symlink;

        let ws = TestWorkspace::new_named("changelog-symlink")?;
        write_release_config(
            &ws,
            r#"
[release.changelog]
path = "CHANGELOG.md"
relative_to = "workspace"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
        let outside = tempfile::TempDir::new()?;
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "outside\n")?;
        symlink(&victim, ws.path.join("CHANGELOG.md"))?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;
        let origin = publish_to_origin(&ws)?;
        let initial = head(&ws.path)?;

        for args in [
            &["lib-a", "--bump", "patch", "--dry-run"][..],
            &["lib-a", "--bump", "patch", "--yes"][..],
        ] {
            let output = release(&ws.path, args)?;
            assert!(!output.status.success(), "{args:?}: {}", describe(&output));
            assert_eq!(std::fs::read_to_string(&victim)?, "outside\n");
            assert!(
                std::fs::symlink_metadata(ws.path.join("CHANGELOG.md"))?
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(head(&ws.path)?, initial);
        }
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that parent directories are auto-created for changelog paths
#[test]
fn test_changelog_parent_directories_auto_created() {
    let result: Result<()> = (|| {
        // Use a nested path that doesn't exist
        let ws = changelog_workspace(
            "changelog-auto-mkdir",
            "[release.changelog]\npath = \"docs/changelogs/CHANGELOG.md\"\nrelative_to = \"workspace\"\n",
        )?;
        publish_to_origin(&ws)?;

        // docs/changelogs/ doesn't exist yet - should be auto-created
        assert!(!ws.path.join("docs/changelogs").exists());

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(
            output.status.success(),
            "release should succeed with auto-created directories\n{}",
            describe(&output)
        );

        // Verify directory and changelog were created
        let changelog_path = ws.path.join("docs/changelogs/CHANGELOG.md");
        assert!(
            changelog_path.exists(),
            "Changelog should exist at nested path: {}",
            changelog_path.display()
        );
        assert!(
            ws.path.join("docs/changelogs").is_dir(),
            "Parent directories should be auto-created"
        );
        assert!(committed_exists(&ws.path, "docs/changelogs/CHANGELOG.md"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "crate" with custom path creates in crate subdir
#[test]
fn test_changelog_relative_to_crate_custom_path() {
    let result: Result<()> = (|| {
        // Use custom path with crate-relative
        let ws = changelog_workspace(
            "changelog-crate-custom",
            "[release.changelog]\npath = \"docs/CHANGES.md\"\nrelative_to = \"crate\"\n",
        )?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(output.status.success(), "release should succeed\n{}", describe(&output));

        // Changelog should be at crates/lib-a/docs/CHANGES.md
        let changelog_path = ws.path.join("crates/lib-a/docs/CHANGES.md");
        assert!(
            changelog_path.exists(),
            "Changelog should exist at custom crate-relative path: {}",
            changelog_path.display()
        );
        assert!(committed_exists(&ws.path, "crates/lib-a/docs/CHANGES.md"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Bump modes --------------------------------------------------------------------------------

/// Plan `--bump BUMP` for a single crate at `version` and return the planned version.
fn planned_single_crate_version(name: &str, version: &str, bump: &str) -> Result<(String, String)> {
    let ws = TestWorkspace::new_single_crate(name, version)?;
    ws.write_release_config("publish = \"none\"\n")?;
    write_test_change(&ws.path, &[name])?;
    publish_to_origin(&ws)?;

    let output = release(&ws.path, &["--dry-run", "--bump", bump])?;
    anyhow::ensure!(output.status.success(), "{}", describe(&output));
    let (_, json) = release_json(&ws.path, &["--dry-run", "--bump", bump])?;
    let planned = crate_report(&json, name)["version"]
        .as_str()
        .context("planned version")?
        .to_owned();
    Ok((stdout(&output), planned))
}

/// Test --bump prerelease from stable version
#[test]
fn test_bump_prerelease_from_stable() {
    let result: Result<()> = (|| {
        let (text, version) = planned_single_crate_version("prerelease-test", "1.0.0", "prerelease")?;
        // Should show 1.0.0 -> 1.0.0-rc.1
        assert_eq!(
            version, "1.0.0-rc.1",
            "Should bump to rc.1 prerelease.\nOutput:\n{text}"
        );
        assert!(text.contains("1.0.0 -> 1.0.0-rc.1"), "{text}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test --bump prerelease increments existing prerelease
#[test]
fn test_bump_prerelease_increment() {
    let result: Result<()> = (|| {
        let (text, version) = planned_single_crate_version("prerelease-inc", "2.0.0-rc.1", "prerelease")?;
        // Should show 2.0.0-rc.1 -> 2.0.0-rc.2
        assert_eq!(version, "2.0.0-rc.2", "Should increment to rc.2.\nOutput:\n{text}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test --bump release strips prerelease suffix
#[test]
fn test_bump_release_strips_prerelease() {
    let result: Result<()> = (|| {
        let (text, version) = planned_single_crate_version("release-strip", "1.5.0-beta.3", "release")?;
        // Should show 1.5.0-beta.3 -> 1.5.0
        assert_eq!(version, "1.5.0", "Should strip prerelease to 1.5.0.\nOutput:\n{text}");
        assert!(text.contains("1.5.0-beta.3 -> 1.5.0"), "{text}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_unsafe_tag_names_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("unsafe-release-tag", "0.1.0")?;
        ws.write_release_config("tag_format = \"-{version}\"\npublish = \"none\"\n")?;
        commit_with_lockfile(&ws, "Configure unsafe release tag")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}")?;
        ws.commit("Change unsafe release tag crate")?;
        write_test_change(&ws.path, &["unsafe-release-tag"])?;
        let origin = publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;

        let output = release(&ws.path, &["--bump", "patch", "--yes"])?;
        let combined = combined(&output);
        assert!(!output.status.success(), "{combined}");
        assert!(
            combined.contains("release.tag_format") && combined.contains("valid Git tag name"),
            "{combined}"
        );
        assert_eq!(head(&ws.path)?, initial_head);
        assert!(git(&ws.path, &["tag", "--list"])?.stdout.is_empty());
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial_head.as_str())
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Confirmation and branch preconditions ----------------------------------------------------

/// A workspace with lib-a released at 0.1.0, a feature since, and reviewed patch intent on origin.
fn gated_release_workspace(name: &str) -> Result<(TestWorkspace, PathBuf)> {
    let ws = TestWorkspace::new_named(name)?;
    write_release_config(&ws, "")?;
    ws.add_crate("lib-a", "0.1.0", &[])?;
    commit_with_lockfile(&ws, "Add lib-a")?;
    ws.tag("lib-a-v0.1.0", "Initial release")?;
    ws.modify_file("lib-a", "src/lib.rs", "pub fn gate() {}")?;
    ws.commit("feat: add release-gated change")?;
    write_test_change(&ws.path, &["lib-a"])?;
    let origin = publish_to_origin(&ws)?;
    Ok((ws, origin))
}

/// Test that a release requires explicit confirmation in non-interactive mode
#[test]
fn test_release_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
        let (ws, origin) = gated_release_workspace("release-confirmation-gate")?;
        let initial = head(&ws.path)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch"])?;
        assert!(
            !output.status.success(),
            "release should fail without --yes in non-interactive mode"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("a release asks for confirmation, but no terminal is attached")
                && stderr.contains("--yes")
                && stderr.contains("--dry-run"),
            "safety gate message missing expected guidance.\nstderr:\n{}",
            stderr
        );
        assert_eq!(head(&ws.path)?, initial);
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        assert!(!local_tags(&ws.path)?.contains(&"lib-a-v0.1.1".to_owned()));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_confirmation_pty_accepts_only_yes_and_keeps_the_prompt_on_stderr() {
    fn run_in_pty(ws: &TestWorkspace, answer: &[u8]) -> Result<(String, String)> {
        use std::ffi::OsStr;
        use std::io::Write as _;
        use std::os::unix::ffi::OsStrExt as _;
        use std::process::Stdio;
        use std::thread;
        use std::time::{Duration, Instant};

        use rustix::fs::{Mode, OFlags, open};
        use rustix::io::{FdFlags, fcntl_setfd};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

        let stdout = ws.path.join("target/pty-stdout");
        let stderr = ws.path.join("target/pty-stderr");
        std::fs::create_dir_all(ws.path.join("target"))?;
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
        let mut master = std::fs::File::from(master);
        let mut child = cargo_rail_command(&ws.path)?
            .args(["rail", "release", "lib-a", "--bump", "patch"])
            .stdin(Stdio::from(std::fs::File::from(slave)))
            .stdout(Stdio::from(std::fs::File::create(&stdout)?))
            .stderr(Stdio::from(std::fs::File::create(&stderr)?))
            .spawn()?;

        let outcome = (|| -> Result<()> {
            let prompt_deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if Instant::now() >= prompt_deadline {
                    anyhow::bail!("command did not reach its confirmation prompt");
                }
                if std::fs::read(&stderr)?
                    .windows(b"Proceed? [y/N] ".len())
                    .any(|window| window == b"Proceed? [y/N] ")
                {
                    break;
                }
                if let Some(status) = child.try_wait()? {
                    anyhow::bail!("command exited before prompting: {status}");
                }
                thread::sleep(Duration::from_millis(20));
            }

            master.write_all(answer)?;
            let exit_deadline = Instant::now() + Duration::from_secs(60);
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= exit_deadline {
                    anyhow::bail!("command did not exit after confirmation");
                }
                thread::sleep(Duration::from_millis(20));
            };
            anyhow::ensure!(status.success(), "command failed after prompting: {status}");
            Ok(())
        })();
        if let Err(error) = outcome {
            drop(child.kill());
            drop(child.wait());
            return Err(error);
        }
        Ok((std::fs::read_to_string(stdout)?, std::fs::read_to_string(stderr)?))
    }

    let result: Result<()> = (|| {
        for (name, answer) in [
            ("lower-y", &b"y\r"[..]),
            ("upper-y", &b"Y\r"[..]),
            ("lower-yes", &b"yes\r"[..]),
            ("mixed-yes", &b"YeS\r"[..]),
        ] {
            let (accepted, origin) = gated_release_workspace(&format!("release-confirmation-pty-{name}"))?;
            let (stdout, stderr) = run_in_pty(&accepted, answer)?;
            assert!(
                stderr.contains("Proceed? [y/N] "),
                "prompt missing from stderr for {name}: {stderr}"
            );
            assert!(
                !stdout.contains("Proceed? [y/N] "),
                "prompt leaked to stdout for {name}: {stdout}"
            );
            assert!(
                std::fs::read_to_string(accepted.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.1\""),
                "accepted answer {name} did not authorize the release"
            );
            assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(head(&accepted.path)?));
        }

        for (name, answer) in [
            ("enter", &b"\r"[..]),
            ("n", &b"n\r"[..]),
            ("no", &b"no\r"[..]),
            ("leading-space", &b" y\r"[..]),
            ("trailing-space", &b"yes \r"[..]),
            ("other", &b"anything\r"[..]),
            ("eof", &b"\x04"[..]),
        ] {
            let (rejected, origin) = gated_release_workspace(&format!("release-confirmation-pty-{name}"))?;
            let head_before = head(&rejected.path)?;
            let (stdout, stderr) = run_in_pty(&rejected, answer)?;
            assert!(
                stderr.contains("Proceed? [y/N] "),
                "prompt missing from stderr for {name}: {stderr}"
            );
            assert!(
                !stdout.contains("Proceed? [y/N] "),
                "prompt leaked to stdout for {name}: {stdout}"
            );
            assert!(stdout.contains("cancelled; nothing changed"), "{name}: {stdout}");
            assert_eq!(
                head(&rejected.path)?,
                head_before,
                "rejected answer {name} changed HEAD"
            );
            assert!(
                std::fs::read_to_string(rejected.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.0\""),
                "rejected answer {name} mutated the manifest"
            );
            assert_eq!(
                ref_commit(&origin, "refs/heads/main").as_deref(),
                Some(head_before.as_str())
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that release fails from detached HEAD
#[test]
fn test_release_detached_head_fails() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-detached")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;
        let commit_sha = head(&ws.path)?;

        // Checkout detached HEAD
        git(&ws.path, &["checkout", &commit_sha])?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "Release from detached HEAD should fail");
        assert!(
            stderr.contains("HEAD is detached"),
            "Error should mention detached HEAD.\nstderr:\n{}",
            stderr
        );
        assert!(local_tags(&ws.path)?.is_empty());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// A release starts only from a branch that exists on origin.
#[test]
fn release_requires_the_branch_on_origin() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-branch")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;

        // Create and switch to a feature branch that origin does not have.
        git(&ws.path, &["checkout", "-b", "feature-branch"])?;
        let initial = head(&ws.path)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "Release from a branch origin lacks should fail.\nstderr:\n{}",
            stderr
        );
        assert!(
            stderr.contains("branch 'feature-branch' does not exist on origin")
                && stderr.contains("git push -u origin feature-branch"),
            "Error should name the branch and how to publish it.\nstderr:\n{}",
            stderr
        );
        assert_eq!(head(&ws.path)?, initial);
        assert!(local_tags(&ws.path)?.is_empty());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Any branch on origin may release; the release stays on that branch.
#[test]
fn release_from_a_non_default_branch_on_origin() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-branch-yes")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        let origin = publish_to_origin(&ws)?;
        let main = head(&ws.path)?;

        // Create and switch to a maintenance branch
        git(&ws.path, &["checkout", "-b", "hotfix-1.0"])?;

        // Make a change for the release
        ws.modify_file("lib-a", "src/lib.rs", "pub fn hotfix() {}")?;
        ws.commit("feat: add hotfix function")?;
        write_test_change(&ws.path, &["lib-a"])?;
        ws.commit("Review the hotfix")?;
        git(&ws.path, &["push", "--quiet", "-u", "origin", "hotfix-1.0"])?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(
            output.status.success(),
            "Release with --yes should succeed.\n{}",
            describe(&output)
        );

        let branch = git(&ws.path, &["branch", "--show-current"])?;
        assert!(
            String::from_utf8_lossy(&branch.stdout).trim() == "hotfix-1.0",
            "release should remain on its branch"
        );
        let released = head(&ws.path)?;
        assert_eq!(
            rev_parse(&ws.path, "lib-a-v0.1.1^{commit}")?,
            released,
            "release tag should target the release commit"
        );
        assert_eq!(ref_commit(&origin, "refs/heads/hotfix-1.0"), Some(released.clone()));
        assert_eq!(ref_commit(&origin, "refs/tags/lib-a-v0.1.1"), Some(released));
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(main));
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("git push origin"),
            "a release pushes itself.\n{}",
            describe(&output)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Unpublishable crates ---------------------------------------------------------------------

#[test]
fn release_dry_run_plans_the_same_release_for_an_unpublishable_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-local-unpublishable")?;
        // Crates.io is the default registry; an unpublishable workspace never reaches it.
        ws.write_release_config("semver_check = \"off\"\n")?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        commit_with_lockfile(&ws, "feat: add internal release")?;
        write_test_change(&ws.path, &["internal"])?;
        publish_to_origin(&ws)?;

        let (first, check) = release_json(&ws.path, &["--dry-run"])?;
        let (second, run) = release_json(&ws.path, &["--dry-run"])?;
        assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
        assert_eq!(second.status.code(), Some(0), "{}", describe(&second));
        assert_eq!(check, run);
        assert_eq!(check["result"], "planned");
        let planned = crate_report(&check, "internal");
        assert_eq!(planned["tag"], "internal-v0.1.1");
        assert_eq!(planned["registry"], "skipped");
        assert_eq!(planned["github_release"], "skipped");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_dry_run_reports_unpublishable_crates_as_registry_skipped() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-publication-unpublishable")?;
        ws.write_release_config("semver_check = \"off\"\n")?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        commit_with_lockfile(&ws, "feat: add internal release")?;
        write_test_change(&ws.path, &["internal"])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["--dry-run"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(text.contains("not published"), "{text}");
        assert!(!text.contains("publishes to"), "{text}");

        let (output, json) = release_json(&ws.path, &["--dry-run"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        assert_eq!(json["result"], "planned");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["registry"], "crates-io");
        assert_eq!(crate_names(&json), ["internal"]);
        assert_eq!(crate_report(&json, "internal")["registry"], "skipped");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Configure the loopback registry for a workspace of publishable and unpublishable crates.
fn write_registry_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
    ws.write_release_config(&format!(
        "tag_format = \"{{crate}}-v{{version}}\"\nsemver_check = \"off\"\npublish = \"{}\"\n{extras}",
        EmptyRegistry::NAME
    ))
}

/// Test that a release publishes only crates whose Cargo.toml allows it
#[test]
fn test_release_all_skips_unpublishable_cargo_toml() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-skip-unpub")?;
        write_registry_release_config(&ws, "")?;

        // Add a publishable crate
        ws.add_crate("lib-pub", "0.1.0", &[])?;

        // Add an unpublishable crate (publish = false in Cargo.toml)
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;

        commit_with_lockfile(&ws, "Add crates")?;
        write_test_change(&ws.path, &["lib-pub", "lib-internal"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let output = registry.release(&ws.path, &["--dry-run"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));

        let section = |name: &str| {
            text.split("\n\n")
                .find(|section| section.trim_start().starts_with(&format!("{name} ")))
                .unwrap_or_default()
                .to_owned()
        };
        // lib-pub is ready to publish; the plan keeps lib-internal but never publishes it.
        assert!(
            section("lib-pub").contains("publish to fixture"),
            "Should report lib-pub as publishing.\nstdout:\n{text}"
        );
        assert!(
            section("lib-internal").contains("not published"),
            "Should report lib-internal as not publishable.\nstdout:\n{text}"
        );
        assert!(text.contains("fixture: will publish"), "{text}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that path-only deps are allowed for crates with publish = false
#[test]
fn test_release_path_deps_allowed_for_unpublishable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("path-dep-unpub")?;
        write_registry_release_config(&ws, "")?;

        // Add a publishable crate
        ws.add_crate("lib-core", "0.1.0", &[])?;

        // Add an unpublishable crate with a path-only dep
        add_crate_with_path_dep(&ws, "wasm-bindings", "0.1.0", "lib-core", false)?;

        commit_with_lockfile(&ws, "Add crates")?;
        write_test_change(&ws.path, &["lib-core", "wasm-bindings"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let output = registry.release(&ws.path, &["--dry-run", "--format", "json"])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

        // Should NOT contain the path-only dependency error
        assert!(
            !stderr.contains("path-only dependency"),
            "Should not complain about path-only deps for unpublishable crates.\nstderr:\n{}",
            stderr
        );
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(crate_report(&json, "lib-core")["registry"], "pending");
        assert_eq!(crate_report(&json, "wasm-bindings")["registry"], "skipped");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that explicitly naming an unpublishable crate reports its status
#[test]
fn test_release_explicit_unpublishable_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("explicit-unpub")?;
        ws.write_release_config("semver_check = \"off\"\n")?;

        // Add an unpublishable crate
        add_unpublishable_crate(&ws, "internal-tool", "0.1.0")?;
        commit_with_lockfile(&ws, "Add crates")?;
        write_test_change(&ws.path, &["internal-tool"])?;
        publish_to_origin(&ws)?;

        // Release the specific crate
        let output = release(&ws.path, &["internal-tool", "--dry-run"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(
            text.contains("not published"),
            "Should report crate as not publishable.\nstdout:\n{}",
            text
        );
        let (_, json) = release_json(&ws.path, &["internal-tool", "--dry-run"])?;
        assert_eq!(crate_report(&json, "internal-tool")["registry"], "skipped");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test JSON output marks unpublishable crates as skipped for the registry
#[test]
fn test_release_json_marks_unpublishable_crates_skipped() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("json-skipped")?;
        write_registry_release_config(&ws, "")?;

        // Add publishable and unpublishable crates
        ws.add_crate("lib-pub", "0.1.0", &[])?;
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;
        commit_with_lockfile(&ws, "Add crates")?;
        write_test_change(&ws.path, &["lib-pub", "lib-internal"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let output = registry.release(&ws.path, &["--dry-run", "--json"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let json: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("Should be valid JSON.\n{}", describe(&output)));
        assert_eq!(json["registry"], EmptyRegistry::NAME);
        assert_eq!(crate_report(&json, "lib-pub")["registry"], "pending");
        assert_eq!(
            crate_report(&json, "lib-internal")["registry"],
            "skipped",
            "lib-internal must not publish.\nJSON:\n{json}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that rail.toml publish = false is respected
#[test]
fn test_release_respects_rail_toml_publish_false() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("rail-toml-unpub")?;

        // Add crates (both publishable in Cargo.toml)
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add crates")?;

        // Configure lib-b as non-publishable in rail.toml
        write_registry_release_config(&ws, "\n[crates.lib-b.release]\npublish = false\n")?;
        write_test_change(&ws.path, &["lib-a", "lib-b"])?;
        publish_to_origin(&ws)?;
        let registry = EmptyRegistry::start()?;

        let output = registry.release(&ws.path, &["--dry-run", "--format", "json"])?;
        assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
        let json: Value = serde_json::from_slice(&output.stdout)?;

        // lib-a is ready to publish; lib-b stays in the release without publishing.
        assert_eq!(crate_names(&json), ["lib-a", "lib-b"]);
        assert_eq!(crate_report(&json, "lib-a")["registry"], "pending", "{json}");
        assert_eq!(crate_report(&json, "lib-b")["registry"], "skipped", "{json}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_failed_package_validation_before_any_effect() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("registry-shadow", "0.1.0")?;
        ws.write_release_config(&format!(
            "tag_format = \"v{{version}}\"\nsemver_check = \"off\"\npublish = \"{}\"\n",
            EmptyRegistry::NAME
        ))?;
        commit_with_lockfile(&ws, "Configure releases")?;
        ws.tag("v0.1.0", "Release registry-shadow 0.1.0")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn invalid( {\n")?;
        ws.commit("Introduce an invalid package for the validation boundary")?;
        write_test_change(&ws.path, &["registry-shadow"])?;
        let origin = publish_to_origin(&ws)?;
        let initial = head(&ws.path)?;
        let registry = EmptyRegistry::start()?;

        let interrupted = registry.release(&ws.path, &["--bump", "patch", "--yes"])?;
        assert!(!interrupted.status.success(), "{}", describe(&interrupted));
        assert!(
            combined(&interrupted).contains("`cargo package` failed for the release commit"),
            "{}",
            describe(&interrupted)
        );
        assert_eq!(head(&ws.path)?, initial);
        assert!(!local_tags(&ws.path)?.contains(&"v0.1.1".to_owned()));
        assert_eq!(
            ref_commit(&origin, "refs/heads/main").as_deref(),
            Some(initial.as_str())
        );
        assert_eq!(ref_commit(&origin, "refs/tags/v0.1.1"), None);
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_package_excludes_finder_metadata() {
    let result: Result<()> = (|| {
        let root = crate::helpers::manifest_directory();
        let manifest: toml_edit::DocumentMut = std::fs::read_to_string(root.join("Cargo.toml"))?.parse()?;
        let include = manifest["package"]["include"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("package.include must be an array"))?;

        assert!(
            include.iter().any(|value| value.as_str() == Some("!**/.DS_Store")),
            "package.include must exclude Finder metadata even when tests are included"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

// ----- Dependent closure -------------------------------------------------------------------------

/// lib-c depends on lib-b, which depends on lib-a; all three are released and published.
fn closure_workspace(name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named(name)?;
    write_release_config(&ws, "")?;

    ws.add_crate("lib-a", "0.1.0", &[])?;
    ws.add_crate(
        "lib-b",
        "0.1.0",
        &[("lib-a", "{ version = \"^0.1.0\", path = \"../lib-a\" }")],
    )?;
    ws.add_crate(
        "lib-c",
        "0.1.0",
        &[("lib-b", "{ version = \"^0.1.0\", path = \"../lib-b\" }")],
    )?;
    commit_with_lockfile(&ws, "Add release closure crates")?;
    tag_release(&ws, "lib-a", "0.1.0")?;
    tag_release(&ws, "lib-b", "0.1.0")?;
    tag_release(&ws, "lib-c", "0.1.0")?;
    ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
    ws.commit("feat: change lib-a")?;
    write_test_change(&ws.path, &["lib-a"])?;
    publish_to_origin(&ws)?;
    Ok(ws)
}

#[test]
fn named_crate_release_expands_to_the_dependent_closure() {
    let result: Result<()> = (|| {
        let ws = closure_workspace("release-partial-closure")?;

        let (output, report) = release_json(&ws.path, &["lib-a", "--dry-run"])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "naming lib-a releases the dependents it needs\n{}",
            describe(&output)
        );
        assert_eq!(crate_names(&report), ["lib-a", "lib-b", "lib-c"]);
        assert_eq!(
            crate_report(&report, "lib-a")["reason"],
            "auto: reviewed change files -> patch"
        );
        for dependent in ["lib-b", "lib-c"] {
            assert_eq!(
                crate_report(&report, dependent)["reason"],
                "auto: dependent closure -> patch",
                "{report}"
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_dependent_closure_follows_dependency_order() {
    let result: Result<()> = (|| {
        let ws = closure_workspace("release-include-dependents")?;

        let output = release(&ws.path, &["lib-a", "--dry-run"])?;
        let text = stdout(&output);
        assert!(output.status.success(), "{}", describe(&output));
        let position = |name: &str| {
            text.find(&format!("\n  {name} 0.1.0 -> "))
                .unwrap_or_else(|| panic!("expected {name} in release plan\nstdout:\n{text}"))
        };
        assert!(
            position("lib-a") < position("lib-b") && position("lib-b") < position("lib-c"),
            "dependent closure should be released in dependency order.\nstdout:\n{}",
            text
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_subset_release_only_mutates_selected_closure_tags_and_changelogs() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-subset-apply")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate(
            "lib-b",
            "0.1.0",
            &[("lib-a", "{ version = \"^0.1.0\", path = \"../lib-a\" }")],
        )?;
        ws.add_crate("lib-c", "0.1.0", &[])?;
        commit_with_lockfile(&ws, "Add release subset crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;
        let origin = publish_to_origin(&ws)?;
        let initial_head = head(&ws.path)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(
            output.status.success(),
            "subset release should succeed.\n{}",
            describe(&output)
        );

        let released = head(&ws.path)?;
        let count = git(&ws.path, &["rev-list", "--count", &format!("{initial_head}..HEAD")])?;
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
        for tag in ["lib-a-v0.1.1", "lib-b-v0.1.1"] {
            assert_eq!(rev_parse(&ws.path, &format!("{tag}^{{commit}}"))?, released);
            assert_eq!(ref_commit(&origin, &format!("refs/tags/{tag}")), Some(released.clone()));
        }
        assert_eq!(ref_commit(&origin, "refs/heads/main"), Some(released));

        let lib_a_manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        let lib_b_manifest = std::fs::read_to_string(ws.path.join("crates/lib-b/Cargo.toml"))?;
        let lib_c_manifest = std::fs::read_to_string(ws.path.join("crates/lib-c/Cargo.toml"))?;
        assert!(lib_a_manifest.contains("version = \"0.1.1\""));
        assert!(lib_b_manifest.contains("version = \"0.1.1\""));
        assert!(lib_b_manifest.contains("^0.1.1"));
        assert!(lib_c_manifest.contains("version = \"0.1.0\""));

        let tags = local_tags(&ws.path)?;
        assert!(
            tags.contains(&"lib-a-v0.1.1".to_owned()),
            "missing lib-a tag.\ntags:\n{tags:?}"
        );
        assert!(
            tags.contains(&"lib-b-v0.1.1".to_owned()),
            "missing lib-b tag.\ntags:\n{tags:?}"
        );
        assert!(
            !tags.contains(&"lib-c-v0.1.1".to_owned()),
            "unrelated crate should not be tagged.\ntags:\n{tags:?}"
        );
        assert_eq!(ref_commit(&origin, "refs/tags/lib-c-v0.1.1"), None);

        assert!(ws.path.join("crates/lib-a/CHANGELOG.md").exists());
        assert!(ws.path.join("crates/lib-b/CHANGELOG.md").exists());
        assert!(
            !ws.path.join("crates/lib-c/CHANGELOG.md").exists(),
            "unrelated crate should not get a changelog"
        );
        assert!(git(&ws.path, &["status", "--porcelain"])?.stdout.is_empty());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_apply_supports_publish_false_from_rail_toml() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-run-publish-false")?;
        ws.add_crate("internal-tool", "0.1.0", &[])?;
        // Crates.io is the default registry; rail.toml keeps this crate off it.
        ws.write_release_config(
            r#"tag_format = "{crate}-v{version}"
semver_check = "off"

[crates.internal-tool.release]
publish = false
"#,
        )?;
        commit_with_lockfile(&ws, "Add internal-tool")?;
        tag_release(&ws, "internal-tool", "0.1.0")?;
        ws.modify_file("internal-tool", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: update internal-tool")?;
        write_test_change(&ws.path, &["internal-tool"])?;
        let origin = publish_to_origin(&ws)?;

        let (output, report) = release_json(&ws.path, &["internal-tool", "--bump", "patch", "--yes"])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "publish = false release should succeed without crates.io publish.\n{}",
            describe(&output)
        );
        assert_eq!(report["result"], "released");
        assert_eq!(
            crate_report(&report, "internal-tool")["registry"],
            "skipped",
            "expected a skipped registry effect.\n{report}"
        );
        assert!(ws.path.join("crates/internal-tool/CHANGELOG.md").exists());
        let tags = local_tags(&ws.path)?;
        assert!(tags.contains(&"internal-tool-v0.1.1".to_owned()), "{tags:?}");
        assert_eq!(
            ref_commit(&origin, "refs/tags/internal-tool-v0.1.1"),
            Some(head(&ws.path)?)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_subset_release_updates_shared_workspace_dependency_versions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-workspace-deps")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        add_workspace_dependency(&ws, "lib-a", "0.1.0")?;
        ws.add_crate("lib-b", "0.1.0", &[("lib-a", "{ workspace = true }")])?;
        commit_with_lockfile(&ws, "Add workspace dependency crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;
        publish_to_origin(&ws)?;

        let output = release(&ws.path, &["lib-a", "--bump", "patch", "--yes"])?;
        assert!(
            output.status.success(),
            "subset release with workspace dependencies should succeed.\n{}",
            describe(&output)
        );

        let root_manifest = std::fs::read_to_string(ws.path.join("Cargo.toml"))?;
        let lib_b_manifest = std::fs::read_to_string(ws.path.join("crates/lib-b/Cargo.toml"))?;
        assert!(
            root_manifest.contains("lib-a = { version = \"0.1.1\", path = \"crates/lib-a\" }"),
            "workspace dependency should be bumped.\nCargo.toml:\n{}",
            root_manifest
        );
        assert!(
            lib_b_manifest.contains("version = \"0.1.1\""),
            "dependent crate should be version bumped as part of the closure.\nCargo.toml:\n{}",
            lib_b_manifest
        );
        assert!(
            lib_b_manifest.contains("lib-a = { workspace = true }"),
            "workspace dependency declaration should remain workspace-based.\nCargo.toml:\n{}",
            lib_b_manifest
        );
        assert_eq!(committed(&ws.path, "Cargo.toml")?, root_manifest);

        Ok(())
    })();
    super::helpers::finish_test(result);
}
