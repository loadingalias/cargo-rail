//! Integration tests for release + changelog generation
//!
//! Covers:
//! - Tag pattern detection ({crate}-v*)
//! - Compare URLs with GitHub remote
//! - reviewed change-file bump and prose projection
//! - per-crate changelog paths and skips

#[cfg(unix)]
use crate::helpers::isolated_cargo_rail_command;
use crate::helpers::{
    NestedWorkspace, TestWorkspace, cargo_command, cargo_rail_command, file_url, git, git_command, run_cargo_rail,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn generate_lockfile(workspace: &Path) -> Result<()> {
    let output = cargo_command(workspace)
        .args(["generate-lockfile", "--manifest-path", "Cargo.toml"])
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

#[test]
fn current_configuration_preserves_release_sources_and_presentation() {
    let result: Result<()> = (|| {
        for source in ["commits", "both"] {
            {
                let ws = TestWorkspace::new_named("release-config-presentation")?;
                ws.add_crate("lib-a", "1.2.3", &[])?;
                let authority = "remote_effects = 'none'";
                write_release_config(
                    &ws,
                    &format!(
                        "source = '{source}'\nrequire_change_files = false\nrequire_release_notes = true\n{authority}\n[release.changelog]\nemoji = false\nentry_format = '- CUSTOM: {{description}}'\n[release.changelog.filters]\nskip_types = ['docs']\n"
                    ),
                )?;
                ws.commit("fixture")?;
                tag_release(&ws, "lib-a", "1.2.3")?;
                ws.modify_file("lib-a", "src/lib.rs", "pub fn added() {}\n")?;
                ws.commit("feat: add the API")?;
                if source == "both" {
                    std::fs::create_dir_all(ws.path.join(".changes"))?;
                    std::fs::write(
                        ws.path.join(".changes/reviewed.md"),
                        "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed addition.\n",
                    )?;
                }
                let config_path = ws.path.join(".config/rail.toml");
                let original = std::fs::read(&config_path)?;
                let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--format", "json"])?;
                assert_eq!(output.status.code(), Some(1), "{source}: {output:?}");
                let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                let plan = &value["release_plan"];
                assert_eq!(plan["source"], source);
                assert_eq!(plan["crates"][0]["new_version"], "1.3.0");
                let body = plan["crates"][0]["changelog_body"].as_str().unwrap();
                assert!(body.contains("CUSTOM: add the API"), "{body}");
                if source == "both" {
                    assert!(body.contains("Reviewed addition."), "{body}");
                }
                assert_eq!(std::fs::read(config_path)?, original);
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

fn write_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
    ws.write_release_config(&format!(
        r#"tag_prefix = "v"
tag_format = "{{crate}}-v{{version}}"
semver_check = "off"
{}
"#,
        extras
    ))?;
    Ok(())
}

fn write_publication_release_config(ws: &TestWorkspace, extras: &str) -> Result<()> {
    write_release_config(
        ws,
        &format!("remote_effects = \"push\"\nregistry_publication = \"crates-io\"\n{extras}"),
    )
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

fn shallow_clone(ws: &TestWorkspace, name: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let root = tempfile::TempDir::new()?;
    let clone_path = root.path().join(name);
    let output = git_command(root.path())
        .args([
            "clone",
            "--depth",
            "1",
            &file_url(&ws.path),
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

#[cfg(unix)]
fn seed_legacy_release_lease(remote: &Path, status: &str) -> Result<String> {
    let fixture = tempfile::tempdir()?;
    git(fixture.path(), &["init", "--initial-branch=main"])?;
    git(fixture.path(), &["config", "user.name", "Release fixture"])?;
    git(fixture.path(), &["config", "user.email", "release@example.invalid"])?;
    std::fs::write(
        fixture.path().join("record.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 9,
            "transaction_id": "release-legacy-v9",
            "status": status,
            "require_changelog_entries": false
        }))?,
    )?;
    git(fixture.path(), &["add", "record.json"])?;
    git(fixture.path(), &["commit", "-m", "Retain legacy release"])?;
    let head = git(fixture.path(), &["rev-parse", "HEAD"])?;
    let remote = remote
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("release fixture remote is not UTF-8"))?;
    git(
        fixture.path(),
        &[
            "push",
            "--atomic",
            remote,
            "HEAD:refs/notes/cargo-rail/release-legacy-v9",
            "HEAD:refs/notes/cargo-rail/active",
        ],
    )?;
    Ok(String::from_utf8(head.stdout)?.trim().to_owned())
}

fn run_with_rejected_commit(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    let hook = cwd.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'fixture commit policy rejected' >&2\nexit 1\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    let output = run_cargo_rail(cwd, args);
    std::fs::remove_file(hook)?;
    let output = output?;
    anyhow::ensure!(
        String::from_utf8_lossy(&output.stderr).contains("fixture commit policy rejected"),
        "release did not reach the rejecting commit hook: {output:?}"
    );
    Ok(output)
}

#[cfg(unix)]
fn run_with_lost_git_acknowledgment(cwd: &Path, args: &[&str], operation: &str) -> Result<std::process::Output> {
    run_with_lost_git_acknowledgment_and_path_prefix(cwd, args, operation, None)
}

#[cfg(unix)]
fn run_with_lost_git_acknowledgment_and_path_prefix(
    cwd: &Path,
    args: &[&str],
    operation: &str,
    path_prefix: Option<&Path>,
) -> Result<std::process::Output> {
    use std::os::unix::fs::PermissionsExt;
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    anyhow::ensure!(real_git.status.success(), "cannot locate Git");
    let real_git = String::from_utf8(real_git.stdout)?.trim().to_owned();
    let pattern = match operation {
        "commit" => "*\" commit -m \"*",
        "tag" => "*\" tag -a \"*|*\" tag -s \"*",
        "push" => "*\" push \"*",
        "release-push" => "*\" push --atomic --force-with-lease=refs/heads/\"*",
        "record-push" => "*\" push --atomic --force-with-lease=refs/notes/cargo-rail/release-\"*",
        _ => anyhow::bail!("unsupported Git operation"),
    };
    let dir = tempfile::tempdir()?;
    let wrapper = dir.path().join("git");
    let completed = dir.path().join("completed");
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
case " $* " in
  {pattern})
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
    paths.extend(path_prefix.map(Path::to_path_buf));
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(paths)?;
    let output = cargo_rail_command(cwd)?.env("PATH", path).args(args).output()?;
    anyhow::ensure!(
        completed.is_file(),
        "release did not reach the completed Git operation: {output:?}"
    );
    Ok(output)
}

#[cfg(windows)]
fn run_with_lost_git_acknowledgment(cwd: &Path, args: &[&str], operation: &str) -> Result<std::process::Output> {
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
    std::fs::write(
        &source,
        format!(
            r#"
fn main() {{
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let effect = match {operation:?} {{
        "commit" => args.windows(2).any(|pair| pair[0] == "commit" && pair[1] == "-m"),
        "tag" => args.windows(2).any(|pair| pair[0] == "tag" && (pair[1] == "-a" || pair[1] == "-s")),
        "push" => args.iter().any(|arg| arg == "push"),
        "release-push" => args.iter().any(|arg| arg.to_string_lossy().starts_with("--force-with-lease=refs/heads/")),
        "record-push" => args
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--force-with-lease=refs/notes/cargo-rail/release-")),
        _ => panic!("unsupported Git operation"),
    }};
    let status = std::process::Command::new({real_git:?}).args(&args).status().unwrap();
    if status.success() && effect {{
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
        String::from_utf8_lossy(&output.stderr).contains("fixture Git transport lost acknowledgment"),
        "release did not reach the completed Git operation: {output:?}"
    );
    Ok(output)
}

fn only_release_state(workspace: &Path) -> Result<PathBuf> {
    std::fs::read_dir(workspace.join("target/cargo-rail/releases"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().is_some_and(|extension| extension == "json"))
        .ok_or_else(|| anyhow::anyhow!("missing release state"))
}

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

fn configured_auxiliary_release(crate_name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    // `aux` is a reserved DOS device name; keep this shared fixture portable.
    add_auxiliary_cargo_workspace(&ws, "auxiliary", crate_name)?;
    ws.write_release_config(
        r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
    )?;
    write_test_change(&ws.path, &[crate_name])?;
    ws.commit("Configure auxiliary Cargo release projection")?;
    Ok(ws)
}

fn check_auxiliary_release(ws: &TestWorkspace) -> Result<std::process::Output> {
    run_cargo_rail(
        &ws.path,
        &["rail", "release", "check", "--all", "--bump", "patch", "--skip-tag"],
    )
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
    let ws = TestWorkspace::new_single_crate(if absolute { "aux-absolute" } else { "aux-escaping" }, "0.1.0")?;
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
    ws.write_release_config(
        r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
    )?;
    write_test_change(&ws.path, &[if absolute { "aux-absolute" } else { "aux-escaping" }])?;
    let initial_head = ws.commit("Configure external auxiliary path dependency")?;

    let check = check_auxiliary_release(&ws)?;
    anyhow::ensure!(
        !check.status.success(),
        "release check unexpectedly accepted an external auxiliary dependency\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
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
    let final_head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
    anyhow::ensure!(
        final_head == format!("{initial_head}\n").as_bytes(),
        "release check moved HEAD from {initial_head} to {}",
        String::from_utf8_lossy(&final_head).trim()
    );
    Ok(())
}

fn push_release_workspace(crate_name: &str) -> Result<(TestWorkspace, tempfile::TempDir)> {
    let ws = TestWorkspace::new_single_crate(crate_name, "0.1.0")?;
    let remote = tempfile::TempDir::new()?;
    git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
    ws.set_remote(
        remote
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("bare release remote path is not UTF-8"))?,
    )?;
    git(&ws.path, &["push", "-u", "origin", "main"])?;
    ws.write_release_config(
        r#"tag_format = "v{version}"
remote_effects = "push"
"#,
    )?;
    write_test_change(&ws.path, &[crate_name])?;
    Ok((ws, remote))
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

#[test]
fn release_plan_works_on_single_crate_repo() {
    let result: Result<()> = (|| {
        // Test that release plan works on a split repo (single-crate, non-workspace)
        let ws = TestWorkspace::new_single_crate("private-tool", "0.1.0")?;

        // Add release config (what a split repo would have)
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "release-{crate}-{prefix}{version}"
semver_check = "off"
"#,
        )?;
        ws.commit("Configure custom release tags")?;
        ws.tag("release-private-tool-v0.1.0", "Initial release")?;
        write_test_change(&ws.path, &["private-tool"])?;

        let explained = run_cargo_rail(
            &ws.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(explained.status.success(), "config explain failed: {explained:?}");
        let explained: serde_json::Value = serde_json::from_slice(&explained.stdout)?;
        assert_eq!(
            explained["fields"][0]["configured"],
            "release-{crate}-{prefix}{version}"
        );
        assert_eq!(explained["fields"][0]["effective"], "release-{crate}-{prefix}{version}");

        // Run release plan
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "patch"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Should show the crate in the plan
        assert!(
            stdout.contains("private-tool"),
            "Plan should include private-tool. Output:\n{}",
            stdout
        );
        assert!(
            stdout.contains("0.1.0 → 0.1.1") || stdout.contains("0.1.0") && stdout.contains("0.1.1"),
            "Plan should show version bump. Output:\n{}",
            stdout
        );
        assert!(
            !stdout.contains("0 crate(s)"),
            "Plan should not show 0 crates. Output:\n{}",
            stdout
        );
        assert!(
            stdout.contains("Tag: release-private-tool-v0.1.1"),
            "explicit tag format must control the single-package plan. Output:\n{stdout}"
        );

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--format", "json", "--bump", "patch"],
        )?;
        assert_eq!(output.status.code(), Some(1), "release check: {output:?}");
        let output: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            output["release_plan"]["crates"][0]["tag_name"],
            "release-private-tool-v0.1.1"
        );
        assert_eq!(
            output["release_plan"]["crates"][0]["previous_tag"],
            "release-private-tool-v0.1.0"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_tag_defaults_align_across_init_explain_and_planning() {
    let result: Result<()> = (|| {
        let single = TestWorkspace::new_single_crate("single", "0.1.0")?;
        single.write_release_config("semver_check = \"off\"\n")?;
        single.commit("Configure release checks")?;
        let single_init = run_cargo_rail(&single.path, &["rail", "release", "init", "--dry-run"])?;
        assert!(single_init.status.success(), "release init failed: {single_init:?}");
        let single_init = String::from_utf8(single_init.stdout)?;
        assert!(
            single_init.contains("tag_format = \"{crate}-{prefix}{version}\""),
            "single-package initialization used a different default: {single_init}"
        );
        let single_explain = run_cargo_rail(
            &single.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(
            single_explain.status.success(),
            "config explain failed: {single_explain:?}"
        );
        let single_explain: serde_json::Value = serde_json::from_slice(&single_explain.stdout)?;
        assert_eq!(single_explain["fields"][0]["default"], "{crate}-{prefix}{version}");
        assert_eq!(single_explain["fields"][0]["effective"], "{crate}-{prefix}{version}");
        write_test_change(&single.path, &["single"])?;
        let single_plan = run_cargo_rail(&single.path, &["rail", "release", "check", "--bump", "patch"])?;
        let single_plan = String::from_utf8(single_plan.stdout)?;
        assert!(single_plan.contains("Tag: single-v0.1.1"), "single plan: {single_plan}");

        let multi = TestWorkspace::new_named("release-default-tag-multi")?;
        multi.add_crate("crate-a", "0.1.0", &[])?;
        multi.add_crate("crate-b", "0.1.0", &[])?;
        multi.write_release_config("semver_check = \"off\"\n")?;
        multi.commit("Add workspace packages")?;
        let multi_init = run_cargo_rail(&multi.path, &["rail", "release", "init", "--dry-run"])?;
        assert!(multi_init.status.success(), "release init failed: {multi_init:?}");
        let multi_init = String::from_utf8(multi_init.stdout)?;
        assert!(
            multi_init.contains("tag_format = \"{crate}-{prefix}{version}\""),
            "multi-package initialization used a different default: {multi_init}"
        );
        let multi_explain = run_cargo_rail(
            &multi.path,
            &["rail", "config", "explain", "release.tag_format", "-f", "json"],
        )?;
        assert!(
            multi_explain.status.success(),
            "config explain failed: {multi_explain:?}"
        );
        let multi_explain: serde_json::Value = serde_json::from_slice(&multi_explain.stdout)?;
        assert_eq!(multi_explain["fields"][0]["default"], "{crate}-{prefix}{version}");
        assert_eq!(multi_explain["fields"][0]["effective"], "{crate}-{prefix}{version}");
        write_test_change(&multi.path, &["crate-a"])?;
        let multi_plan = run_cargo_rail(&multi.path, &["rail", "release", "check", "crate-a", "--bump", "patch"])?;
        let multi_plan = String::from_utf8(multi_plan.stdout)?;
        assert!(multi_plan.contains("Tag: crate-a-v0.1.1"), "multi plan: {multi_plan}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn reviewed_changes_are_the_only_release_source() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-source-changes-default")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "1.2.3", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "1.2.3")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn redesigned() {}\n")?;
        ws.commit("feat!: conventional history must not control this release")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch intent.\n",
        )?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--format", "json"])?;
        assert_eq!(output.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let plan = &json["release_plan"];
        assert_eq!(plan["crates"][0]["new_version"], serde_json::json!("1.2.4"));
        assert_eq!(plan["source"], "changes");
        assert!(
            plan["crates"][0]["commits"]
                .as_array()
                .is_some_and(|commits| commits.is_empty())
        );
        assert!(
            plan["crates"][0]["changelog_body"]
                .as_str()
                .unwrap()
                .contains("Reviewed patch intent.")
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("conventional history must not control"),
            "changes mode leaked commit prose: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn no_release_change_intent_satisfies_default_coverage_without_a_bump() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-no-release-intent")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
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

        let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
        assert!(
            check.status.success(),
            "reviewed no-release intent should satisfy coverage\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );

        let plan = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        assert!(plan.status.success());
        let stdout = String::from_utf8_lossy(&plan.stdout);
        assert!(stdout.contains("No release-worthy changes detected."), "{}", stdout);
        assert!(
            stdout.contains("no reviewed release intent or dependency updates"),
            "{}",
            stdout
        );
        assert!(!stdout.contains("Internal-only refactor"), "{}", stdout);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_retains_unconsumed_no_release_intent_from_a_shared_file() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-retain-no-release-intent")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.commit("Add crates")?;
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

        let preview = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--format", "json"])?;
        assert_eq!(
            preview.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&preview.stdout),
            String::from_utf8_lossy(&preview.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        assert_eq!(json["release_plan"]["change_files_to_delete"], serde_json::json!([]));
        let retained = &json["release_plan"]["change_files_to_update"][0];
        let retained_path = retained["path"]
            .as_str()
            .expect("retained change-file path should be a JSON string");
        assert_eq!(
            std::fs::canonicalize(retained_path)?,
            std::fs::canonicalize(&change_path)?,
            "release plan should retain the same change file"
        );
        assert_eq!(
            retained["content"],
            serde_json::json!("---\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n")
        );

        let apply = run_cargo_rail(&ws.path, &["rail", "release", "run", "lib-a", "--yes"])?;
        assert!(
            apply.status.success(),
            "release should retain lib-b coverage\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&change_path)?,
            "---\n\"lib-b\" = \"none\"\n---\n\nShared internal work with one released fix.\n"
        );

        let coverage = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-b"])?;
        assert!(
            coverage.status.success(),
            "retained no-release intent should continue to cover lib-b\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&coverage.stdout),
            String::from_utf8_lossy(&coverage.stderr)
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn reviewed_changes_require_repository_wide_coverage_by_default() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-default-change-coverage")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("unstructured commit subject")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{}", combined);
        assert!(combined.contains("missing change files"), "{}", combined);
        assert!(!combined.contains("not a conventional commit"), "{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_apply_accepts_the_untracked_change_entry_bound_by_its_plan() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-apply-bound-change-entry")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/untracked-reviewed.md");
        std::fs::write(
            &change_path,
            "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch from an untracked plan input.\n",
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "auto", "--yes"],
        )?;
        assert!(
            output.status.success(),
            "bound dirty input should be accepted\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!change_path.exists());
        let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        assert!(manifest.contains("version = \"0.1.1\""), "{}", manifest);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_abort_restores_untracked_reviewed_input_after_a_local_fault() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-restore-untracked-intent")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn reviewed_change() {}\n")?;
        let initial_head = ws.commit("Implement reviewed change")?;

        let content = "---\n\"lib-a\" = \"patch\"\n---\n\nPreserve this reviewed intent across recovery.\n";
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/recover.md");
        std::fs::write(&change_path, content)?;

        let interrupted = run_with_rejected_commit(&ws.path, &["rail", "release", "run", "lib-a", "--yes"])?;
        assert!(!interrupted.status.success());
        assert_eq!(
            std::fs::read_to_string(&change_path)?,
            content,
            "a pre-commit failure must immediately restore untracked reviewed input"
        );

        let state_path = only_release_state(&ws.path)?;
        let aborted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "abort",
                state_path.file_stem().unwrap().to_str().unwrap(),
                "--yes",
            ],
        )?;
        assert!(
            aborted.status.success(),
            "abort should restore journaled local inputs\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&aborted.stdout),
            String::from_utf8_lossy(&aborted.stderr)
        );
        assert_eq!(std::fs::read_to_string(&change_path)?, content);
        assert_eq!(
            String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout).trim(),
            initial_head
        );
        assert!(std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.0\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_apply_rejects_unrelated_dirt_before_the_first_write() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-apply-unrelated-dirt")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "off"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        let change_path = ws.path.join(".changes/reviewed.md");
        std::fs::write(&change_path, "---\n\"lib-a\" = \"patch\"\n---\n\nReviewed patch.\n")?;
        std::fs::write(ws.path.join("UNRELATED.md"), "unbound operator dirt\n")?;
        let head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "auto", "--yes"],
        )?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{}", combined);
        assert!(combined.contains("unplanned worktree changes"), "{}", combined);
        assert!(combined.contains("UNRELATED.md"), "{}", combined);
        assert!(change_path.exists());
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head);

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
        ws.commit("Add release crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "1.2.3")?;

        write_test_change_levels(&ws.path, &[("lib-a", "minor"), ("lib-b", "patch")])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "--check should report pending release changes\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            stdout.contains("0.1.0 → 0.2.0"),
            "reviewed minor intent should produce a minor bump\nstdout:\n{}",
            stdout
        );
        assert!(
            stdout.contains("1.2.3 → 1.2.4"),
            "reviewed patch intent should produce a patch bump\nstdout:\n{}",
            stdout
        );
        assert!(
            stdout.contains("auto: reviewed change files"),
            "plan should explain auto bump source\nstdout:\n{}",
            stdout
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
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.3.1")?;
        write_test_change_levels(&ws.path, &[("lib-a", "major")])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("0.3.1 → 1.0.0"),
            "pre_1_breaking_bump = major should graduate to 1.0.0\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
fn run_with_semver_shim(ws: &TestWorkspace, check_release_script: &str, args: &[&str]) -> Result<std::process::Output> {
    use std::os::unix::fs::PermissionsExt;

    let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
    let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
    let shim_dir = tempfile::TempDir::new()?;
    let shim = shim_dir.path().join("cargo");
    std::fs::write(
        &shim,
        format!(
            r#"#!/bin/sh
if [ "$1" = "semver-checks" ] && [ "$2" = "--version" ]; then
  echo "cargo-semver-checks 0.99.0"
  exit 0
fi
if [ "$1" = "semver-checks" ] && [ "$2" = "check-release" ]; then
  {}
fi
exec "{}" "$@"
"#,
            check_release_script, real_cargo
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
    Ok(cargo_rail_command(&ws.path)?.env("PATH", path).args(args).output()?)
}

#[cfg(unix)]
fn run_with_path_prefix(ws: &TestWorkspace, prefix: &Path, args: &[&str]) -> Result<std::process::Output> {
    let path = format!("{}:{}", prefix.display(), std::env::var("PATH").unwrap_or_default());
    let mut command = if matches!(args.get(1), Some(&"cache" | &"clean")) {
        isolated_cargo_rail_command(&ws.path)?
    } else {
        cargo_rail_command(&ws.path)?
    };
    command.env("PATH", path).args(args).output().map_err(Into::into)
}

#[cfg(unix)]
fn publication_boundary_shim(log_path: &Path, published_path: &Path) -> Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    let real_cargo = Command::new("sh").args(["-c", "command -v cargo"]).output()?;
    let real_cargo = String::from_utf8_lossy(&real_cargo.stdout).trim().to_string();
    let real_git = Command::new("sh").args(["-c", "command -v git"]).output()?;
    let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("cargo");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
echo "$*" >> "{}"

if [ "$1" = "search" ]; then
  exit 0
fi

if [ "$1" = "publish" ]; then
  touch "{}"
  echo "unexpected publication in preparation fixture" >&2
  exit 101
fi

exec "{}" "$@"
"#,
            log_path.display(),
            published_path.display(),
            real_cargo
        ),
    )?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms)?;

    let git_path = dir.path().join("git");
    let remote_head = dir.path().join("remote-head");
    let remote_tags = dir.path().join("remote-tags");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
case " $* " in
  *" remote get-url "*)
    echo "https://github.com/loadingalias/registry-shadow.git"
    exit 0
    ;;
  *" ls-remote "*)
    ref=""
    for argument in "$@"; do ref="$argument"; done
    case "$ref" in
      refs/heads/*)
        [ -f "{}" ] && printf '%s\t%s\n' "$(cat "{}")" "$ref"
        ;;
      refs/tags/*)
        [ -f "{}" ] && printf '%s\t%s\n' "$(cat "{}")" "$ref"
        ;;
    esac
    exit 0
    ;;
  *" push "*)
    repository=.
    previous=""
    for argument in "$@"; do
      [ "$previous" = "-C" ] && repository="$argument"
      previous="$argument"
    done
    head=$("{}" -C "$repository" rev-parse HEAD)
    case " $* " in
      *" refs/tags/"*)
        tag=""
        for argument in "$@"; do
          case "$argument" in refs/tags/*) tag="$argument" ;; esac
        done
        "{}" -C "$repository" rev-parse "$tag" > "{}"
        ;;
      *) printf '%s\n' "$head" > "{}" ;;
    esac
    exit 0
    ;;
esac
exec "{}" "$@"
"#,
            remote_head.display(),
            remote_head.display(),
            remote_tags.display(),
            remote_tags.display(),
            real_git,
            real_git,
            remote_tags.display(),
            remote_head.display(),
            real_git,
        ),
    )?;
    let mut perms = std::fs::metadata(&git_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&git_path, perms)?;

    let gh_path = dir.path().join("gh");
    std::fs::write(
        &gh_path,
        format!("#!/bin/sh\n{}\nexit 0\n", github_validation_response()),
    )?;
    let mut perms = std::fs::metadata(&gh_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gh_path, perms)?;
    Ok(dir)
}

#[cfg(unix)]
#[test]
fn release_rejects_failed_package_validation_before_remote_effects() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("registry-shadow", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
semver_check = "off"
sign_tags = false
remote_effects = "push"
validation = { ".github/workflows/ci.yml" = ["tests"] }
registry_publication = "crates-io"
"#,
        )?;
        ws.set_remote("https://github.com/loadingalias/registry-shadow.git")?;
        ws.commit("Configure releases")?;
        ws.tag("v0.1.0", "Release registry-shadow 0.1.0")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn invalid( {\n")?;
        ws.commit("Introduce an invalid package for the validation boundary")?;
        write_test_change(&ws.path, &["registry-shadow"])?;

        let shim_state = tempfile::TempDir::new()?;
        let log_path = shim_state.path().join("cargo.log");
        let published_path = shim_state.path().join("published");
        let shim = publication_boundary_shim(&log_path, &published_path)?;
        let path = format!(
            "{}:{}",
            shim.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let rejection = PathBuf::from(format!("{}.deny", published_path.display()));
        std::fs::write(&rejection, "reject this request")?;
        let interrupted = cargo_rail_command(&ws.path)?
            .env("PATH", &path)
            .args([
                "rail",
                "release",
                "run",
                "registry-shadow",
                "--bump",
                "patch",
                "--publish",
                "--yes",
            ])
            .output()?;
        assert!(!interrupted.status.success());
        assert!(
            !published_path.exists(),
            "a rejected registry request must not mark the version published"
        );
        anyhow::ensure!(
            ws.path.join("target/cargo-rail/releases").is_dir(),
            "release failed before journal creation\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&interrupted.stdout),
            String::from_utf8_lossy(&interrupted.stderr)
        );
        std::fs::remove_file(rejection)?;
        let state_path = only_release_state(&ws.path)?;
        let pending: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(pending["crates"][0]["publication"]["status"], "pending");
        assert!(pending["package_seal"].is_null());
        assert_eq!(pending["commit_push"]["status"], "pending");
        assert_eq!(pending["crates"][0]["tag"]["status"], "pending");
        let log = std::fs::read_to_string(&log_path)?;
        assert!(
            String::from_utf8_lossy(&interrupted.stderr)
                .contains("Cargo could not validate the complete release package set"),
            "{}",
            String::from_utf8_lossy(&interrupted.stderr)
        );
        assert!(
            !log.lines()
                .any(|line| line.starts_with("publish ") || line.starts_with("info ")),
            "{log}"
        );
        assert_eq!(pending["intent"]["publish_registry"], "crates-io");
        assert!(!published_path.exists());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_package_excludes_finder_metadata() {
    let result: Result<()> = (|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
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

#[cfg(unix)]
fn github_validation_response() -> &'static str {
    r#"
if [ "$1" = "api" ]; then
  for endpoint in "$@"; do :; done
  repository=${endpoint#repos/}
  repository=${repository%%/actions/*}
  sha=$(git rev-parse HEAD)
  run="{\"id\":42,\"run_attempt\":3,\"workflow_id\":7,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"repository\":{\"full_name\":\"$repository\"},\"head_repository\":{\"full_name\":\"$repository\"},\"event\":\"workflow_dispatch\",\"status\":\"${fixture_status:-completed}\",\"conclusion\":\"success\"}"
  case "$endpoint" in
    */actions/workflows/ci.yml) printf '%s\n' '{"id":7,"path":".github/workflows/ci.yml","state":"active"}' ;;
    */actions/workflows/7/runs*) printf '%s\n' "{\"total_count\":1,\"workflow_runs\":[$run]}" ;;
    */actions/runs/42/attempts/3/jobs*) printf '%s\n' "{\"total_count\":1,\"jobs\":[{\"id\":17,\"name\":\"tests\",\"run_id\":42,\"run_attempt\":3,\"head_sha\":\"$sha\",\"status\":\"completed\",\"conclusion\":\"success\"}]}" ;;
    */actions/runs/42|*/actions/runs/42/attempts/3) printf '%s\n' "$run" ;;
    *) echo "unexpected validation endpoint: $endpoint" >&2; exit 1 ;;
  esac
  exit 0
fi
"#
}

#[cfg(unix)]
fn gh_shim(log_path: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("gh");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
echo "$@" >> "{}"
if [ "$1" = "--version" ]; then
  echo "gh version 0.0.0"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  exit 0
fi
{}
echo "unexpected gh args: $@" >&2
exit 1
"#,
            log_path.display(),
            github_validation_response()
        ),
    )?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms)?;
    Ok((dir, path))
}

#[cfg(unix)]
fn glab_shim(log_path: &Path) -> Result<(tempfile::TempDir, PathBuf)> {
    glab_shim_with_status(log_path, "success")
}

#[cfg(unix)]
fn glab_shim_with_status(log_path: &Path, pipeline_status: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("glab");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
echo "$@" >> "{}"
if [ "$1" = "--version" ]; then
  echo "glab version 0.0.0"
  exit 0
fi
if [ "$1" = "release" ] && [ "$2" = "view" ]; then
  exit 1
fi
if [ "$1" = "release" ] && [ "$2" = "create" ]; then
  exit 0
fi
if [ "$1" = "api" ]; then
  case "$2" in
    projects/:id/pipelines\?sha=*)
      if git show-ref --verify --quiet refs/tags/v0.1.1; then
        echo "release tag existed before exact-SHA readiness" >&2
        exit 1
      fi
      echo '[{{"status":"{}"}}]'
      exit 0
      ;;
  esac
fi
echo "unexpected glab args: $@" >&2
exit 1
"#,
            log_path.display(),
            pipeline_status
        ),
    )?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms)?;
    Ok((dir, path))
}

#[cfg(unix)]
fn run_with_minimal_path_without_forge(ws: &TestWorkspace, args: &[&str]) -> Result<std::process::Output> {
    use std::os::unix::fs::symlink;

    let dir = tempfile::TempDir::new()?;
    for binary in ["cargo", "git", "rustc", "rustdoc"] {
        let output = Command::new("sh")
            .args(["-c", &format!("command -v {binary}")])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "could not locate required test binary {}",
            binary
        );
        let real = String::from_utf8_lossy(&output.stdout).trim().to_string();
        symlink(real, dir.path().join(binary))?;
    }

    cargo_rail_command(&ws.path)?
        .env("PATH", dir.path())
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .args(args)
        .output()
        .map_err(Into::into)
}

#[cfg(unix)]
fn semver_shim_workspace(name: &str) -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named(name)?;
    ws.write_release_config(
        r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "warn"
"#,
    )?;

    ws.add_crate("lib-a", "1.2.3", &[])?;
    ws.commit("Add lib-a")?;
    ws.tag("lib-a-v1.2.3", "Initial release")?;
    ws.modify_file("lib-a", "src/lib.rs", "pub fn doc_only_bump_signal() {}\n")?;
    ws.commit("docs: update public API notes")?;
    Ok(ws)
}

#[cfg(unix)]
#[test]
fn release_plan_blocks_when_semver_checks_exceeds_reviewed_intent() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-auto-semver-checks")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"minor\"\n---\n\nReviewed a non-breaking API change.\n",
        )?;

        let output = run_with_semver_shim(
            &ws,
            r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
            &["rail", "release", "check", "lib-a", "--bump", "auto"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let combined = format!("{}\n{}", stdout, stderr);
        assert_eq!(output.status.code(), Some(2), "{}", combined);
        assert!(combined.contains("requires a major release"), "{}", combined);
        assert!(combined.contains("revise the reviewed change entry"), "{}", combined);
        assert!(!combined.contains("1.2.3 → 2.0.0"), "{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_accepts_semver_breakage_covered_by_reviewed_major_intent() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-semver-reviewed-major")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/reviewed.md"),
            "---\n\"lib-a\" = \"major\"\n---\n\nReviewed breaking API change.\n",
        )?;

        let output = run_with_semver_shim(
            &ws,
            r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
            &["rail", "release", "check", "lib-a"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(1), "{}", stdout);
        assert!(stdout.contains("1.2.3 → 2.0.0"), "{}", stdout);
        assert!(stdout.contains("reviewed change files -> major"), "{}", stdout);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_api_evidence_binds_baseline_and_blocks_required_unavailability() {
    let result: Result<()> = (|| {
        let ws = semver_shim_workspace("release-api-evidence")?;
        write_test_change(&ws.path, &["lib-a"])?;
        let baseline = String::from_utf8(git(&ws.path, &["rev-parse", "lib-a-v1.2.3^{}"])?.stdout)?
            .trim()
            .to_owned();
        let args = [
            "rail", "release", "check", "lib-a", "--bump", "patch", "--format", "json",
        ];
        let script = format!(
            r#"
[ "$5" = "--baseline-rev" ] && [ "$6" = "{baseline}" ] && [ "$7" = "--default-features" ] && [ "$8" = "--target" ] && [ -n "$9" ] || {{ echo 'wrong comparison scope' >&2; exit 1; }}
echo 'fixture API evidence is unavailable' >&2
exit 1
"#
        );
        let optional = run_with_semver_shim(&ws, &script, &args)?;
        assert_eq!(optional.status.code(), Some(1), "{optional:?}");
        let document: serde_json::Value = serde_json::from_slice(&optional.stdout)?;
        let evidence = &document["release_plan"]["crates"][0]["api_evidence"];
        assert_eq!(evidence["outcome"], "unavailable");
        assert_eq!(evidence["baseline"], baseline);
        assert_eq!(evidence["required"], false);
        assert_eq!(evidence["detail"], "fixture API evidence is unavailable");
        ws.write_release_config(
            r#"tag_format = "{crate}-v{version}"
semver_check = "deny"
"#,
        )?;
        let required = run_with_semver_shim(&ws, &script, &args)?;
        assert_eq!(required.status.code(), Some(2), "{required:?}");
        assert!(
            String::from_utf8_lossy(&required.stdout).contains("fixture API evidence is unavailable"),
            "{required:?}"
        );
        let missing = run_with_semver_shim(&ws, "echo 'no such command: semver-checks' >&2; exit 101", &args)?;
        assert_eq!(missing.status.code(), Some(2), "{missing:?}");
        assert!(String::from_utf8_lossy(&missing.stdout).contains("no such command: semver-checks"));
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
        let ws = semver_shim_workspace("release-auto-semver-inconclusive")?;
        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/no-release.md"),
            "---\n\"lib-a\" = \"none\"\n---\n\nReviewed documentation-only change.\n",
        )?;

        let output = run_with_semver_shim(
            &ws,
            r#"echo "error: the crate lib-a has no published versions to use as a baseline" >&2
  exit 1"#,
            &["rail", "release", "check", "lib-a", "--bump", "auto"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("2.0.0"),
            "inconclusive semver-checks must not escalate the bump\nstdout:\n{}",
            stdout
        );
        assert!(
            stdout.contains("Skipped:") && stdout.contains("lib-a"),
            "docs-only crate should be skipped with a trace reason\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_plan_auto_skips_semver_checks_for_unpublishable_crates() {
    let result: Result<()> = (|| {
        // publish = false crates have no crates.io baseline; the API check must
        // not run for them even when the checker would report breakage.
        let ws = semver_shim_workspace("release-auto-semver-unpublishable")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"
semver_check = "warn"

[crates.lib-a.release]
publish = false
"#,
        )?;
        ws.commit("Disable publish for lib-a")?;

        let output = run_with_semver_shim(
            &ws,
            r#"echo "Summary semver requires new major version: 1 major check failed" >&2
  exit 1"#,
            &["rail", "release", "check", "lib-a", "--bump", "auto"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("2.0.0"),
            "unpublishable crates must never be semver-escalated\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_auto_reports_skipped_crates_with_reason() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-skip-trace")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.commit("Add crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn only_a_changed() {}\n")?;
        ws.commit("feat: extend lib-a")?;
        write_test_change_levels(&ws.path, &[("lib-a", "minor")])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            stdout.contains("Skipped:"),
            "plan should list skipped crates\nstdout:\n{}",
            stdout
        );
        assert!(
            stdout.contains("lib-b — auto: no release-worthy changes since lib-b-v0.1.0"),
            "skip trace should name the crate and the range\nstdout:\n{}",
            stdout
        );
        assert!(
            stdout.contains("1 skipped"),
            "summary should count skipped crates\nstdout:\n{}",
            stdout
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
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        write_test_change_levels(&ws.path, &[("lib-a", "none")])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "check mode should succeed when there are no planned release mutations\nstdout:\n{}\nstderr:\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("No release-worthy changes detected."),
            "no-op check output should explain that nothing will be applied\nstdout:\n{}",
            stdout
        );
        assert!(
            !stdout.contains("Changes detected."),
            "no-op check output must not report pending changes\nstdout:\n{}",
            stdout
        );

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "check", "--all", "--bump", "auto", "--format", "json",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;

        assert!(output.status.success(), "json no-op check should succeed\n{}", stdout);
        assert_eq!(json["result"], serde_json::json!("no_changes"));
        assert_eq!(json["exit_code"], serde_json::json!(0));
        assert_eq!(json["mutation_plan"]["actions"], serde_json::json!([]));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plans_and_commits_exact_auxiliary_cargo_lockfiles() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-release", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "aux-one", "aux-release")?;
        add_auxiliary_cargo_workspace(&ws, "aux-two", "aux-release")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-release"])?;
        ws.commit("Add auxiliary Cargo release projections")?;

        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(check["release_plan"]["plan_contract_version"], 9);
        let projections = check["release_plan"]["auxiliary_lockfiles"]
            .as_array()
            .expect("auxiliary lockfile projections");
        assert_eq!(projections.len(), 2);
        for (index, name) in ["aux-one", "aux-two"].into_iter().enumerate() {
            assert_eq!(projections[index]["manifest_path"], format!("{name}/Cargo.toml"));
            assert_eq!(projections[index]["lockfile_path"], format!("{name}/Cargo.lock"));
            assert!(
                projections[index]["before_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            );
            assert!(
                projections[index]["after_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            );
            assert_ne!(projections[index]["before_digest"], projections[index]["after_digest"]);
        }
        let auxiliary_actions = check["mutation_plan"]["actions"]
            .as_array()
            .expect("mutation actions")
            .iter()
            .filter(|action| action["code"] == "UPDATE_AUXILIARY_LOCKFILE")
            .collect::<Vec<_>>();
        assert_eq!(auxiliary_actions.len(), 2);
        assert_eq!(
            auxiliary_actions[0]["expected_mutations"][0]["path"],
            "aux-one/Cargo.lock"
        );

        let before = std::fs::read(ws.path.join("aux-one/Cargo.lock"))?;
        let apply = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(
            apply.status.success(),
            "release apply failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );
        for name in ["aux-one", "aux-two"] {
            let lockfile = std::fs::read(ws.path.join(name).join("Cargo.lock"))?;
            assert_ne!(lockfile, before, "{name} lockfile was not projected");
            let committed = git(&ws.path, &["show", &format!("HEAD:{name}/Cargo.lock")])?;
            assert_eq!(committed.stdout, lockfile, "{name} lockfile was not committed exactly");
            let text = String::from_utf8(lockfile)?;
            assert!(text.contains("name = \"aux-release\"\nversion = \"0.1.1\""));
        }
        let status = git(&ws.path, &["status", "--porcelain"])?;
        assert!(status.stdout.is_empty(), "release left unstaged paths");

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
auxiliary_cargo_manifests = ["aux-dual/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["dual-release-one", "dual-release-two"])?;
        ws.commit("Configure one auxiliary update for two releases")?;

        let wrapper_dir = tempfile::TempDir::new()?;
        let marker = wrapper_dir.path().join("update-called");
        let wrapper = wrapper_dir.path().join("cargo-wrapper");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = update ]; then\n  if [ -e \"{}\" ]; then exit 97; fi\n  printf 'one\\n' > \"{}\"\nfi\nexec \"{}\" \"$@\"\n",
                marker.display(),
                marker.display(),
                PathBuf::from(cargo).display()
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

        let check = cargo_rail_command(&ws.path)?
            .env("CARGO", &wrapper)
            .args([
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        assert_eq!(std::fs::read_to_string(&marker)?, "one\n");
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        let candidate = check["release_plan"]["auxiliary_lockfiles"][0]["content"]
            .as_str()
            .expect("planned Cargo.lock content");
        assert!(candidate.contains("name = \"dual-release-one\"\nversion = \"0.1.1\""));
        assert!(candidate.contains("name = \"dual-release-two\"\nversion = \"0.1.1\""));

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
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-crlf"])?;
        ws.commit("Configure a CRLF auxiliary Cargo projection")?;

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
        let status = git(&ws.path, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "CRLF checkout is not Git-clean");

        let check = cargo_rail_command(&ws.path)?
            .args([
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"].as_array().unwrap().len(),
            1
        );
        let status = git(&ws.path, &["status", "--porcelain", "--untracked-files=no"])?;
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
            r#"[release]
tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.workspace_root, &["nested-crlf"])?;
        ws.commit("Configure a nested CRLF auxiliary Cargo projection")?;

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
        let status = git(&ws.git_root, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "nested CRLF checkout is not Git-clean");

        let check = cargo_rail_command(&ws.workspace_root)?
            .args([
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ])
            .output()?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"].as_array().unwrap().len(),
            1
        );
        let status = git(&ws.git_root, &["status", "--porcelain", "--untracked-files=no"])?;
        assert!(status.stdout.is_empty(), "planning changed the nested CRLF checkout");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-manifest")?;
        let manifest = ws.path.join("auxiliary/Cargo.toml");
        let mut changed = std::fs::read_to_string(&manifest)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&manifest, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'auxiliary/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("filter-cleaned worktree content or executable mode differs"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_must_match_head() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-dirty-lock")?;
        let lockfile = ws.path.join("auxiliary/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push_str("\n# uncommitted\n");
        std::fs::write(&lockfile, changed)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo lockfile 'auxiliary/Cargo.lock' does not exactly match HEAD")
                && stderr.contains("filter-cleaned worktree content or executable mode differs"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_index_only_changes() {
    let result: Result<()> = (|| {
        let ws = configured_auxiliary_release("aux-index-manifest")?;
        let manifest = ws.path.join("auxiliary/Cargo.toml");
        let original = std::fs::read(&manifest)?;
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n# staged only\n");
        std::fs::write(&manifest, changed)?;
        git(&ws.path, &["add", "--", "auxiliary/Cargo.toml"])?;
        std::fs::write(&manifest, original)?;

        let check = check_auxiliary_release(&ws)?;
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'auxiliary/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("index entry differs from HEAD"),
            "{stderr}"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_manifest_rejects_intent_to_add() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-intent-manifest", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-intent-manifest"])?;
        ws.commit("Configure an unmaterialized auxiliary workspace")?;
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
        assert!(!check.status.success());
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(
            stderr.contains("auxiliary Cargo manifest 'auxiliary/Cargo.toml' does not exactly match HEAD")
                && stderr.contains("HEAD has no matching regular-file entry"),
            "{stderr}"
        );

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
            r#"[release]
tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.workspace_root, &["nested-release"])?;
        ws.commit("Configure nested auxiliary Cargo projection")?;

        let check = run_cargo_rail(
            &ws.workspace_root,
            &[
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(
            check.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"][0]["manifest_path"],
            "auxiliary/Cargo.toml"
        );
        assert_eq!(
            check["release_plan"]["auxiliary_lockfiles"][0]["lockfile_path"],
            "auxiliary/Cargo.lock"
        );

        let applied = run_cargo_rail(
            &ws.workspace_root,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(applied.status.success(), "{}", String::from_utf8_lossy(&applied.stderr));
        assert!(
            std::fs::read_to_string(ws.workspace_root.join("crates/nested-release/Cargo.toml"))?
                .contains("version = \"0.1.1\"")
        );
        assert!(
            std::fs::read_to_string(auxiliary.join("Cargo.lock"))?
                .contains("name = \"nested-release\"\nversion = \"0.1.1\"")
        );
        assert!(!ws.git_root.join("auxiliary").exists());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_plan_rejects_drift_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-drift", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-drift")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-drift"])?;
        let initial_head = ws.commit("Configure auxiliary Cargo projection")?;
        let check = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(check.status.code(), Some(1));
        let plan_dir = tempfile::TempDir::new()?;
        let plan_path = plan_dir.path().join("release-plan.json");
        std::fs::write(&plan_path, &check.stdout)?;
        let lockfile = ws.path.join("auxiliary/Cargo.lock");
        let mut changed = std::fs::read_to_string(&lockfile)?;
        changed.push('\n');
        std::fs::write(&lockfile, changed)?;

        let apply = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
                "--plan",
                plan_path.to_str().unwrap(),
            ],
        )?;
        assert!(!apply.status.success());
        let stderr = String::from_utf8_lossy(&apply.stderr);
        assert!(
            stderr.contains("auxiliary Cargo lockfile 'auxiliary/Cargo.lock' does not exactly match HEAD"),
            "{stderr}"
        );
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

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
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-invalid"])?;
        let initial_head = ws.commit("Add invalid auxiliary Cargo projection")?;
        let manifest = std::fs::read(ws.path.join("Cargo.toml"))?;
        let lockfile = std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?;

        let check = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--all", "--bump", "patch", "--skip-tag"],
        )?;
        assert!(!check.status.success());
        assert!(String::from_utf8_lossy(&check.stderr).contains("cargo locate-project failed"));
        assert_eq!(std::fs::read(ws.path.join("Cargo.toml"))?, manifest);
        assert_eq!(std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?, lockfile);
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

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
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-command-boundary"])?;
        let initial_head = ws.commit("Configure bounded auxiliary Cargo projection")?;
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
            .args(["rail", "release", "check", "--all", "--bump", "patch", "--skip-tag"])
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
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

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
auxiliary_cargo_manifests = ["aux-one/Cargo.toml", "aux-two/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-bound-candidate"])?;
        let initial_head = ws.commit("Configure exact auxiliary Cargo candidates")?;
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
            .args(["rail", "release", "check", "--all", "--bump", "patch", "--skip-tag"])
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
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial_head}\n").as_bytes()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_auxiliary_lockfile_recovers_before_the_first_commit() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("aux-recovery", "0.1.0")?;
        add_auxiliary_cargo_workspace(&ws, "auxiliary", "aux-recovery")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
auxiliary_cargo_manifests = ["auxiliary/Cargo.toml"]
"#,
        )?;
        write_test_change(&ws.path, &["aux-recovery"])?;
        let initial_head = ws.commit("Configure auxiliary Cargo recovery")?;
        let before = std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?;

        let interrupted = run_with_rejected_commit(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(!interrupted.status.success());
        assert_eq!(std::fs::read(ws.path.join("auxiliary/Cargo.lock"))?, before);
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(state["schema_version"], 10);
        assert_eq!(state["intent"]["plan"]["plan_contract_version"], 9);
        assert_eq!(
            state["intent"]["plan"]["auxiliary_lockfiles"].as_array().unwrap().len(),
            1
        );
        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let after = std::fs::read_to_string(ws.path.join("auxiliary/Cargo.lock"))?;
        assert!(after.contains("name = \"aux-recovery\"\nversion = \"0.1.1\""));
        assert_eq!(
            git(&ws.path, &["rev-list", "--count", &format!("{initial_head}..HEAD")])?.stdout,
            b"1\n"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_projects_exact_sha_checks_and_tags_last() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-plan-order", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
remote_effects = "gitlab"
registry_publication = "crates-io"
"#,
        )?;
        write_test_change(&ws.path, &["release-plan-order"])?;
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "check", "--all", "--bump", "patch", "--format", "json",
            ],
        )?;
        assert_eq!(output.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let codes = json["mutation_plan"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|action| action["code"].as_str())
            .collect::<Vec<_>>();
        let position = |code: &str| codes.iter().position(|candidate| *candidate == code).unwrap();
        assert!(position("COMMIT_RELEASE") < position("PUSH_RELEASE_COMMIT"));
        assert!(position("PUSH_RELEASE_COMMIT") < position("AWAIT_EXACT_SHA_CHECKS"));
        assert!(position("AWAIT_EXACT_SHA_CHECKS") < position("CREATE_TAG"));
        assert!(position("CREATE_TAG") < position("PUSH_RELEASE_TAGS"));
        assert!(position("PUSH_RELEASE_TAGS") < position("CREATE_FORGE_RELEASE"));
        assert!(!codes.contains(&"PUBLISH_CRATE"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_plan_uses_reviewed_intent_in_a_shallow_clone() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-auto-shallow-guard")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "1.2.3", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "1.2.3")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        write_test_change(&ws.path, &["lib-a"])?;
        ws.commit("Record reviewed lib-a change")?;

        let (_root, clone_path) = shallow_clone(&ws, "shallow")?;

        let output = run_cargo_rail(&clone_path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "reviewed intent should produce a normal check plan\n{}",
            combined
        );
        assert!(
            combined.contains("1.2.3 → 1.2.4") && combined.contains("auto: reviewed change files -> patch"),
            "output:\n{}",
            combined
        );
        assert!(
            !combined.contains("cannot run in a shallow clone"),
            "output:\n{}",
            combined
        );

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
        ws.commit("Add lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("no previous tag: full history"),
            "skip reason should name first-release history range\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

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
        ws.commit("Add grouped crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn patch_signal() {}\n")?;
        ws.commit("fix: patch lib-a")?;
        ws.modify_file("lib-b", "src/lib.rs", "pub fn minor_signal() {}\n")?;
        ws.commit("feat: extend lib-b")?;
        write_test_change_levels(&ws.path, &[("lib-a", "patch"), ("lib-b", "minor"), ("lib-c", "none")])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.code(),
            Some(1),
            "plan should have pending changes\n{}",
            stdout
        );
        assert_eq!(
            stdout.matches("0.1.0 → 0.2.0").count(),
            3,
            "all group members should receive the max minor bump\n{}",
            stdout
        );
        assert!(
            stdout.contains("lib-c") && stdout.contains("version group core -> minor"),
            "group-only member should be planned with a group reason\n{}",
            stdout
        );

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "check", "--all", "--bump", "auto", "--format", "json",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)?;
        assert_eq!(json["release_plan"]["plan_contract_version"], 9);
        assert!(
            json["release_plan"]["snapshot_id"]
                .as_str()
                .is_some_and(|snapshot| snapshot.starts_with("v1-sha256-"))
        );
        let crates = json["release_plan"]["crates"].as_array().expect("crates array");
        for crate_name in ["lib-a", "lib-b", "lib-c"] {
            let crate_plan = crates
                .iter()
                .find(|entry| entry["name"] == crate_name)
                .unwrap_or_else(|| panic!("missing {}", crate_name));
            assert_eq!(crate_plan["version_group"], "core");
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn version_group_partial_selection_rejects_or_expands_by_policy() {
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
        ws.commit("Add grouped crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn minor_signal() {}\n")?;
        ws.commit("feat: extend lib-a")?;
        write_test_change_levels(&ws.path, &[("lib-a", "minor"), ("lib-b", "none")])?;

        let rejected = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&rejected.stdout),
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert_eq!(
            rejected.status.code(),
            Some(2),
            "partial group release should fail\n{}",
            combined
        );
        assert!(
            combined.contains("version group 'core'") && combined.contains("lib-b"),
            "output:\n{}",
            combined
        );

        let expanded = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "lib-a",
                "--bump",
                "auto",
                "--include-dependents",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&expanded.stdout);
        assert_eq!(
            expanded.status.code(),
            Some(1),
            "expanded plan should succeed\n{}",
            stdout
        );
        assert!(
            stdout.contains("lib-a") && stdout.contains("lib-b") && stdout.contains("version group core -> minor"),
            "expanded plan should include the whole group\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_review_continues_the_original_transaction_only_on_its_exact_merged_tree() {
    reviewed_release_continuation(false);
}

#[cfg(unix)]
#[test]
fn release_hosted_review_rediscovers_a_merge_after_the_original_runner_is_lost() {
    reviewed_release_continuation(true);
}

#[cfg(unix)]
fn reviewed_release_continuation(hosted: bool) {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-pr-mode")?;
        write_release_config(
            &ws,
            &format!(
                r#"remote_effects = "push"
validation = {{ ".github/workflows/ci.yml" = ["tests"] }}
{}"#,
                if hosted {
                    "hosted_workflow = \".github/workflows/release.yml\""
                } else {
                    ""
                }
            ),
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;

        let remote_root = tempfile::TempDir::new()?;
        let remote = remote_root.path().join("origin.git");
        let output = Command::new("git")
            .args(["init", "--bare", remote.to_str().expect("UTF-8 fixture remote")])
            .output()?;
        assert!(output.status.success(), "bare remote init failed");
        let ssh = remote_root.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                r#"#!/bin/sh
case "$*" in
  *git-receive-pack*) exec git-receive-pack "{}" ;;
  *git-upload-pack*) exec git-upload-pack "{}" ;;
esac
exit 1
"#,
                remote.display(),
                remote.display()
            ),
        )?;
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&ssh)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&ssh, permissions)?;
        }
        ws.set_remote("git@github.com:org/repo.git")?;
        git(
            &ws.path,
            &[
                "config",
                "core.sshCommand",
                ssh.to_str().expect("UTF-8 fixture SSH path"),
            ],
        )?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
context_file="$(dirname "$0")/../release-pr-hook-context"
printf '%s:%s\n' "$CARGO_RAIL_RELEASE_PUSH" "$CARGO_RAIL_OPERATION" >> "$context_file"
if [ "$CARGO_RAIL_RELEASE_PUSH" != "1" ] || [ "$CARGO_RAIL_OPERATION" != "release" ]; then
  echo "release PR push did not provide cargo-rail hook context" >&2
  exit 1
fi
"#,
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
                "Added release PR mode.",
            ],
        )?;
        ws.commit("Add release intent")?;

        let gh_log_dir = tempfile::TempDir::new()?;
        let gh_log = gh_log_dir.path().join("gh.log");
        let (_gh_dir, gh_path) = gh_shim(&gh_log)?;
        let review_api = _gh_dir.path().join("review.py");
        std::fs::write(
            &review_api,
            r#"import json, pathlib, subprocess, sys
root = pathlib.Path(subprocess.check_output(['git','rev-parse','--git-dir'], text=True).strip())
p = root/'review.json'
a = sys.argv[1:]
if '--method' in a:
    body = json.loads(pathlib.Path(a[a.index('--input')+1]).read_text())
    sha = subprocess.check_output(['git','rev-parse','HEAD'], text=True).strip()
    p.write_text(json.dumps({'number':7,'head':{'sha':sha,'ref':body['head'],'repo':{'full_name':'org/repo'}},'base':{'ref':'main','repo':{'full_name':'org/repo'}},'merged':False,'state':'open'}))
if any('/pulls?' in x for x in a):
    print(json.dumps([json.loads(p.read_text())] if p.exists() else []))
else:
    print(p.read_text())
"#,
        )?;
        let script = std::fs::read_to_string(&gh_path)?;
        std::fs::write(&gh_path, script.replace("if [ \"$1\" = \"--version\" ]; then", &format!("case \"$*\" in *repos/org/repo/pulls*) exec python3 '{}' \"$@\" ;; esac\nif [ \"$1\" = \"--version\" ]; then", review_api.display())))?;
        let initial = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_owned();
        let event = gh_log_dir.path().join("event.json");
        std::fs::write(&event, "{}")?;
        let execute = |args: &[&str]| -> Result<std::process::Output> {
            let mut command = cargo_rail_command(&ws.path)?;
            command
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        gh_path.parent().expect("fixture GitHub executable directory").display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .args(args);
            if hosted {
                command
                    .env("GITHUB_ACTIONS", "true")
                    .env("GITHUB_REPOSITORY", "org/repo")
                    .env(
                        "GITHUB_WORKFLOW_REF",
                        "org/repo/.github/workflows/release.yml@refs/heads/main",
                    )
                    .env("GITHUB_EVENT_NAME", "workflow_dispatch")
                    .env("GITHUB_RUN_ID", "88")
                    .env("GITHUB_SHA", &initial)
                    .env("GITHUB_EVENT_PATH", &event)
                    .arg("--executor");
            }
            Ok(command.output()?)
        };
        let output = execute(&["rail", "release", "run", "lib-a", "--bump", "auto", "--pr", "--yes"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release PR mode should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        let branch = String::from_utf8_lossy(&git(&ws.path, &["branch", "--show-current"])?.stdout)
            .trim()
            .to_string();
        assert!(branch.starts_with("rail/release-"), "branch: {}", branch);
        assert!(
            String::from_utf8_lossy(&git(&ws.path, &["tag", "--list", "lib-a-v0.2.0"])?.stdout)
                .trim()
                .is_empty(),
            "PR mode must not create release tags"
        );
        assert!(!ws.path.join(".changes").exists() || std::fs::read_dir(ws.path.join(".changes"))?.next().is_none());
        assert!(std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.2.0\""));
        let gh_commands = std::fs::read_to_string(&gh_log)?;
        assert!(gh_commands.contains("--method POST repos/org/repo/pulls"));
        assert!(
            std::fs::read_to_string(ws.path.join(".git/release-pr-hook-context"))?
                .lines()
                .all(|line| line == "1:release")
        );
        let prepared_message =
            String::from_utf8_lossy(&git(&ws.path, &["log", "-1", "--format=%B"])?.stdout).to_string();
        let transaction = prepared_message
            .lines()
            .find_map(|line| line.strip_prefix("Rail-Release: "))
            .expect("prepared release transaction trailer")
            .to_string();

        let record_path = only_release_state(&ws.path)?;
        let original: serde_json::Value = serde_json::from_slice(&std::fs::read(&record_path)?)?;
        assert_eq!(original["phase"], "awaiting_review");
        std::fs::write(
            &event,
            serde_json::to_vec(&serde_json::json!({"inputs": {
                "transaction": transaction, "intent": original["intent"]["identity"], "source": initial
            }}))?,
        )?;
        git(&ws.path, &["checkout", "main"])?;
        git(&ws.path, &["merge", "--no-ff", &branch, "-m", "Merge release PR"])?;
        let merge_sha = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_string();
        git(
            &remote,
            &["fetch", ws.path.to_str().expect("UTF-8 fixture checkout"), &merge_sha],
        )?;
        git(&remote, &["update-ref", "refs/heads/main", &merge_sha])?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
while read -r _local_ref _local_sha remote_ref _remote_sha; do
  case "$remote_ref" in
    refs/heads/*)
      echo "protected branch update rejected" >&2
      exit 1
      ;;
  esac
done
"#,
        )?;

        let review_path = ws.path.join(".git/review.json");
        let mut pull: serde_json::Value = serde_json::from_slice(&std::fs::read(&review_path)?)?;
        pull["merged"] = true.into();
        pull["state"] = "closed".into();
        pull["merged_at"] = "2026-09-12T12:00:00Z".into();
        pull["merge_commit_sha"] = merge_sha.clone().into();
        std::fs::write(&review_path, serde_json::to_vec(&pull)?)?;
        std::fs::write(ws.path.join("later.txt"), "unrelated merge tree change")?;
        let later = ws.commit("Unrelated later change")?;
        pull["merge_commit_sha"] = later.into();
        std::fs::write(&review_path, serde_json::to_vec(&pull)?)?;
        let rejected = execute(&["rail", "release", "resume", &transaction])?;
        assert!(
            !rejected.status.success(),
            "changed merge tree was accepted: {rejected:?}"
        );
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("merged release tree differs"),
            "{rejected:?}"
        );
        git(&ws.path, &["reset", "--hard", &merge_sha])?;
        pull["merge_commit_sha"] = merge_sha.clone().into();
        std::fs::write(&review_path, serde_json::to_vec(&pull)?)?;
        if hosted {
            // No merge event ran: the retained record still names only the prepared PR commit.
            git(&ws.path, &["switch", &branch])?;
            assert!(original["review"]["merge"].is_null());
        }
        let output = execute(&["rail", "release", "resume", &transaction])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "review continuation should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        let tag_target = String::from_utf8_lossy(&git(&ws.path, &["rev-list", "-n", "1", "lib-a-v0.2.0"])?.stdout)
            .trim()
            .to_string();
        let completed_head = String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(
            tag_target, merge_sha,
            "review continuation should tag the merged commit covered by release evidence"
        );
        assert_eq!(
            completed_head, merge_sha,
            "review continuation must not manufacture a new commit"
        );
        let remote_head = String::from_utf8_lossy(&git(&remote, &["rev-parse", "refs/heads/main"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(
            remote_head, merge_sha,
            "review continuation must not push a protected branch update"
        );
        let remote_tag = String::from_utf8_lossy(&git(&remote, &["rev-list", "-n", "1", "lib-a-v0.2.0"])?.stdout)
            .trim()
            .to_string();
        assert_eq!(remote_tag, merge_sha, "the pushed tag must retain the proven commit");
        let gh_commands = std::fs::read_to_string(&gh_log)?;
        assert!(
            gh_commands.contains("api --hostname github.com repos/org/repo/actions/runs/42/attempts/3"),
            "GitHub readiness must target the bound host\n{}",
            gh_commands
        );
        let completed: serde_json::Value = serde_json::from_slice(&std::fs::read(&record_path)?)?;
        assert_eq!(completed["intent"], original["intent"]);
        assert_eq!(completed["preparation"], original["preparation"]);
        assert_eq!(completed["review"]["merge"]["commit"], merge_sha);
        assert_eq!(completed["status"], "complete");

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
        ws.commit("Add crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;

        std::fs::create_dir_all(ws.path.join(".changes"))?;
        std::fs::write(
            ws.path.join(".changes/shared-change.md"),
            "---\n\"lib-a\" = \"minor\"\n\"lib-b\" = \"patch\"\n---\n\nShared behavior change.\n",
        )?;
        ws.commit("Add change file naming both crates")?;

        // Releasing only lib-a would consume the file and silently destroy
        // lib-b's pending intent — the plan must refuse.
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);

        assert_eq!(
            output.status.code(),
            Some(2),
            "partial change-file consumption must be an error\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            combined.contains("shared-change.md") && combined.contains("lib-b"),
            "error should name the file and the missing crate\noutput:\n{}",
            combined
        );

        // Releasing both crates together consumes the file cleanly.
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--all", "--bump", "auto", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "full release should consume the change file\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            !ws.path.join(".changes/shared-change.md").exists(),
            "change file should be consumed by the release"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

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
        let json: serde_json::Value = serde_json::from_str(&stdout)?;
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
        let json: serde_json::Value = serde_json::from_str(&stdout)?;
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
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&duplicate.stdout),
            String::from_utf8_lossy(&duplicate.stderr)
        );
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
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
        ws.commit("Add lib-a")?;
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

        let plan = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "auto"])?;
        let plan_stdout = String::from_utf8_lossy(&plan.stdout);
        assert!(
            plan_stdout.contains("0.1.0 → 0.2.0"),
            "plan should read configured change_dir\n{}",
            plan_stdout
        );

        let release = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "auto", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&release.stdout);
        let stderr = String::from_utf8_lossy(&release.stderr);
        assert!(
            release.status.success(),
            "release should consume change file from configured dir\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
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
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
        ws.commit("Add lib-a")?;
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

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "auto", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release should succeed from change file\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
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

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_check_enforces_required_change_file_coverage() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-change-file-gate")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("fix: change lib-a")?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);
        assert!(
            !output.status.success(),
            "release check should fail without required change file\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(combined.contains("missing change files"), "output:\n{}", combined);
        assert!(combined.contains("lib-a"), "output:\n{}", combined);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_respects_skip_and_require_flags() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-skip-require")?;
        ws.set_remote("git@github.com:org/repo.git")?;
        write_release_config(&ws, "\n[crates.internal.changelog]\nskip = true")?;

        // Crate with changes
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn change() {}")?;
        ws.commit("fix: update lib-a")?;

        // Crate with no changes and no skip (should fail)
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.commit("Add lib-b")?;
        ws.tag("lib-b-v0.1.0", "Initial lib-b")?;

        // Crate marked as skip (no changelog expected)
        ws.add_crate("internal", "0.1.0", &[])?;
        ws.commit("Add internal crate")?;
        ws.tag("internal-v0.1.0", "Initial internal crate")?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "release should fail because lib-b has no changelog entries and stdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        // On failure, ensure skipped crate did not get a changelog
        assert!(
            !ws.path.join("crates/internal/CHANGELOG.md").exists(),
            "internal crate changelog should be skipped"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_preflight_requires_release_notes_by_default() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-require-notes-default")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("v0.1.0", "Initial lib-a")?;

        let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "patch"])?;
        let check_combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        assert_eq!(
            check.status.code(),
            Some(2),
            "release check should fail\n{check_combined}"
        );
        assert!(
            check_combined.contains("no release notes for lib-a v0.1.1"),
            "expected missing release notes error\n{check_combined}"
        );

        // No commits since last tag -> generated changelog entries are empty.
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "release should fail preflight when release notes are missing\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            stderr.contains("no release notes for lib-a v0.1.1")
                || stdout.contains("no release notes for lib-a v0.1.1"),
            "expected missing release notes error\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_creates_gitlab_release_with_glab() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("gitlab-release", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
remote_effects = "gitlab"
semver_check = "off"
"#,
        )?;
        ws.tag("v0.1.0", "Initial release")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
        ws.commit("fix: update gitlab release test crate")?;
        write_test_change(&ws.path, &["gitlab-release"])?;

        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;

        let glab_log_dir = tempfile::TempDir::new()?;
        let glab_log = glab_log_dir.path().join("glab.log");
        let (_glab_dir, glab_path) = glab_shim(&glab_log)?;
        let output = run_with_path_prefix(
            &ws,
            glab_path.parent().unwrap(),
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "GitLab release should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        let glab_log = std::fs::read_to_string(&glab_log)?;
        assert!(
            glab_log.contains("release view v0.1.1") && glab_log.contains("release create v0.1.1"),
            "glab should check then create the release\n{}",
            glab_log
        );
        assert!(
            glab_log.contains("--name gitlab-release v0.1.1") && glab_log.contains("--notes-file"),
            "glab release create args should include the title and notes file\n{}",
            glab_log
        );
        assert!(
            glab_log.contains("--repo "),
            "glab commands must target the bound repository\n{}",
            glab_log
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_errors_when_gitlab_forge_binary_missing() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("missing-glab", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "v{version}"
remote_effects = "gitlab"
semver_check = "off"
"#,
        )?;
        ws.tag("v0.1.0", "Initial release")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}\n")?;
        ws.commit("fix: update missing glab test crate")?;
        write_test_change(&ws.path, &["missing-glab"])?;

        let output =
            run_with_minimal_path_without_forge(&ws, &["rail", "release", "run", "--all", "--bump", "patch", "--yes"])?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "missing glab should fail before release mutation\n{}",
            combined
        );
        assert!(
            combined.contains("GitLab releases enabled but glab CLI was not found")
                && combined.contains("install glab or set release.remote_effects = \"push\""),
            "output:\n{}",
            combined
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn test_release_pushes_commit_and_tag_when_push_enabled() {
    let result: Result<()> = (|| {
        let (ws, _remote) = push_release_workspace("push-release")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
remote_effects = "gitlab"
"#,
        )?;
        let glab_log_dir = tempfile::TempDir::new()?;
        let glab_log = glab_log_dir.path().join("glab.log");
        let (_glab_dir, glab_path) = glab_shim(&glab_log)?;

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
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    glab_path.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("CARGO_RAIL_TEST_INHERITED", "from-caller")
            .env("GIT_DIR", ws.path.join("ambient-wrong-repository"))
            .env("GIT_TRACE", &trace_path)
            .args(["rail", "release", "run", "--all", "--bump", "patch", "--yes"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "release should push commit and tag\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        let remote_tags = git(&ws.path, &["ls-remote", "--tags", "origin", "v0.1.1"])?;
        assert!(
            !remote_tags.stdout.is_empty(),
            "remote should contain pushed release tag"
        );

        let remote_head = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;
        let local_head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(
            String::from_utf8_lossy(&remote_head.stdout).split_whitespace().next(),
            Some(String::from_utf8_lossy(&local_head.stdout).trim())
        );
        assert!(
            !stdout.contains("git push origin"),
            "owned push should not print manual push follow-up"
        );
        assert!(
            stdout.contains("release hook context accepted"),
            "successful hook diagnostics should stream to stdout\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        let hook_runs = std::fs::read_to_string(&hook_counter)?;
        assert_eq!(
            hook_runs.trim(),
            "2",
            "commit and tag pushes are separate Git transitions, and preflight must not run hooks"
        );
        let trace = std::fs::read_to_string(&trace_path)?;
        assert!(
            trace.contains("push --atomic"),
            "release must retain its atomic push\n{}",
            trace
        );
        let glab_log = std::fs::read_to_string(&glab_log)?;
        let readiness = glab_log.find("api projects/:id/pipelines?sha=").unwrap();
        let release = glab_log.find("release create v0.1.1").unwrap();
        assert!(
            readiness < release,
            "exact-SHA readiness must precede release creation\n{}",
            glab_log
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

#[cfg(unix)]
#[test]
fn release_hosted_request_survives_the_requester_and_runs_the_original_intent() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt;
        let ws = TestWorkspace::new_single_crate("hosted-fixture", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
semver_check = "off"
remote_effects = "push"
hosted_workflow = ".github/workflows/release.yml"
validation = { ".github/workflows/ci.yml" = ["tests"] }
"#,
        )?;
        write_test_change(&ws.path, &["hosted-fixture"])?;
        generate_lockfile(&ws.path)?;
        let initial = ws.commit("Review the complete hosted request")?;
        let transport = tempfile::tempdir()?;
        let remote = transport.path().join("origin.git");
        git(
            transport.path(),
            &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()],
        )?;
        let ssh = transport.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\ncase \"$*\" in *git-receive-pack*) exec git-receive-pack '{}' ;; *git-upload-pack*) exec git-upload-pack '{}' ;; esac\nexit 1\n",
                remote.display(),
                remote.display()
            ),
        )?;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))?;
        ws.set_remote("git@github.com:org/repo.git")?;
        git(&ws.path, &["config", "core.sshCommand", ssh.to_str().unwrap()])?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let (gh_dir, gh) = gh_shim(&transport.path().join("gh.log"))?;
        let api = gh_dir.path().join("dispatch.py");
        std::fs::write(
            &api,
            r#"import json, pathlib, subprocess, sys
args=sys.argv[1:]
root=pathlib.Path(__file__).parent
endpoint=next(x for x in args if x.startswith('repos/'))
sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
if endpoint.endswith('/dispatches'):
    body=json.loads(pathlib.Path(args[args.index('--input')+1]).read_text())
    workflow=7 if '/7/' in endpoint else 8
    (root/('dispatch-'+str(workflow)+'.json')).write_text(json.dumps(body))
    if (root/'lose-ack').exists():
        sys.exit(1)
    if workflow==7 and (root/'lose-ci-ack').exists():
        (root/'lose-ci-ack').unlink()
        sys.exit(1)
    print(json.dumps({'workflow_run_id':42 if workflow==7 else 88}))
elif endpoint.endswith('/release.yml'):
    print(json.dumps({'id':8,'path':'.github/workflows/release.yml','state':'active'}))
else:
    print(json.dumps({'id':88,'run_attempt':1,'workflow_id':8,'head_sha':sha,'event':'workflow_dispatch','repository':{'full_name':'org/repo'}}))
"#,
        )?;
        let script = std::fs::read_to_string(&gh)?;
        std::fs::write(&gh,script.replace("if [ \"$1\" = \"--version\" ]; then",&format!("case \"$*\" in */dispatches*|*/release.yml|*/runs/88) exec python3 '{}' \"$@\" ;; esac\nif [ \"$1\" = \"--version\" ]; then",api.display())))?;
        let script = std::fs::read_to_string(&gh)?;
        std::fs::write(&gh,script.replace("if [ \"$1\" = \"--version\" ]; then",&format!("case \"$*\" in */actions/workflows/7/runs*) if [ ! -f '{}/dispatch-7.json' ]; then echo '{{\"total_count\":0,\"workflow_runs\":[]}}'; exit 0; fi ;; esac\nif [ \"$1\" = \"--version\" ]; then",gh_dir.path().display())))?;
        let path = format!(
            "{}:{}",
            gh_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // The dispatch may have succeeded even when its acknowledgment is lost.
        std::fs::write(gh_dir.path().join("lose-ack"), "")?;
        let submitted = cargo_rail_command(&ws.path)?
            .env("PATH", &path)
            .args(["rail", "release", "run", "--all", "--yes"])
            .output()?;
        assert!(
            !submitted.status.success(),
            "lost acknowledgment was not reported: {submitted:?}"
        );
        assert!(
            String::from_utf8_lossy(&submitted.stderr).contains("workflow dispatch was not acknowledged"),
            "{submitted:?}"
        );
        let original: serde_json::Value = serde_json::from_slice(&std::fs::read(only_release_state(&ws.path)?)?)?;
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial}\n").as_bytes()
        );
        assert_eq!(original["phase"], "planned");
        assert!(original["intent"]["hosted"] == true);
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(gh_dir.path().join("dispatch-8.json"))?)?;
        assert_eq!(request["inputs"]["intent"], original["intent"]["identity"]);
        let transaction = original["transaction_id"].as_str().unwrap();
        std::fs::remove_file(gh_dir.path().join("lose-ack"))?;
        let runner = transport.path().join("runner");
        git(
            transport.path(),
            &["clone", remote.to_str().unwrap(), runner.to_str().unwrap()],
        )?;
        git(&runner, &["remote", "set-url", "origin", "git@github.com:org/repo.git"])?;
        git(&runner, &["config", "core.sshCommand", ssh.to_str().unwrap()])?;
        git(&runner, &["config", "user.name", "Release fixture"])?;
        git(&runner, &["config", "user.email", "release@example.invalid"])?;
        let event = transport.path().join("event.json");
        let mut payload = serde_json::json!({"repository":{"full_name":"org/repo"},"inputs":request["inputs"]});
        payload["inputs"]["intent"] = "sha256:wrong".into();
        std::fs::write(&event, serde_json::to_vec(&payload)?)?;
        let execute = || -> Result<std::process::Output> {
            Ok(cargo_rail_command(&runner)?
                .env("PATH", &path)
                .env("GITHUB_ACTIONS", "true")
                .env("GITHUB_REPOSITORY", "org/repo")
                .env("GITHUB_WORKSPACE", &runner)
                .env(
                    "GITHUB_WORKFLOW_REF",
                    "org/repo/.github/workflows/release.yml@refs/heads/main",
                )
                .env("GITHUB_RUN_ID", "88")
                .env("GITHUB_EVENT_NAME", "workflow_dispatch")
                .env("GITHUB_EVENT_PATH", &event)
                .args(["rail", "release", "resume", transaction, "--executor"])
                .output()?)
        };
        let rejected = execute()?;
        assert!(!rejected.status.success(), "wrong intent was executed: {rejected:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("does not authorize"),
            "{rejected:?}"
        );
        assert_eq!(
            git(&runner, &["rev-parse", "HEAD"])?.stdout,
            format!("{initial}\n").as_bytes()
        );
        payload["inputs"] = request["inputs"].clone();
        std::fs::write(&event, serde_json::to_vec(&payload)?)?;
        // Destroy the requester's entire checkout before continuation.
        std::fs::remove_dir_all(&ws.path)?;
        std::fs::write(gh_dir.path().join("lose-ci-ack"), "")?;
        let interrupted = execute()?;
        assert!(
            !interrupted.status.success(),
            "lost validation acknowledgment was not reported: {interrupted:?}"
        );
        assert!(
            String::from_utf8_lossy(&interrupted.stderr).contains("workflow dispatch was not acknowledged"),
            "{interrupted:?}"
        );
        let completed = execute()?;
        assert!(completed.status.success(), "hosted execution failed: {completed:?}");
        let retained: serde_json::Value = serde_json::from_slice(&std::fs::read(only_release_state(&runner)?)?)?;
        assert_eq!(retained["intent"], original["intent"]);
        assert_eq!(retained["status"], "complete");
        assert_eq!(retained["executor"], 88);
        assert_eq!(retained["validation_dispatches"][".github/workflows/ci.yml"], 42);
        assert_eq!(retained["validation"][0]["run_id"], 42);
        let prepared = retained["preparation"]["commit"].as_str().unwrap();
        assert_eq!(
            git(&remote, &["rev-parse", "refs/tags/v0.1.1^{commit}"])?.stdout,
            format!("{prepared}\n").as_bytes()
        );
        let ci: serde_json::Value = serde_json::from_slice(&std::fs::read(gh_dir.path().join("dispatch-7.json"))?)?;
        assert_eq!(ci["ref"], "main");
        let commands = std::fs::read_to_string(transport.path().join("gh.log"))?;
        assert_eq!(
            commands
                .lines()
                .filter(|line| line.contains("--method POST repos/org/repo/actions/workflows/7/dispatches"))
                .count(),
            1
        );
        let observed = cargo_rail_command(&runner)?
            .env("PATH", &path)
            .args(["rail", "release", "status", "--history", "--format", "json"])
            .output()?;
        assert!(observed.status.success(), "{observed:?}");
        assert!(String::from_utf8_lossy(&observed.stdout).contains("actions/runs/88"));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_a_different_origin_push_repository_before_mutation() {
    let result: Result<()> = (|| {
        let (ws, _fetch_remote) = push_release_workspace("divergent-push")?;
        let push_remote = tempfile::TempDir::new()?;
        git(push_remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["config", "remote.origin.pushurl", push_remote.path().to_str().unwrap()],
        )?;
        let head_before = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(
            combined.contains("origin fetches from") && combined.contains("but pushes to"),
            "{combined}"
        );
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head_before);
        assert!(!ws.path.join("target/cargo-rail/releases").exists());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_rejects_multiple_origin_push_repositories() {
    let result: Result<()> = (|| {
        let (ws, _fetch_remote) = push_release_workspace("multiple-pushes")?;
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

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(2), "{combined}");
        assert!(combined.contains("2 effective push URLs"), "{combined}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_gitlab_handoff_requires_original_records_in_a_second_checkout() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("cross-checkout", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
remote_effects = "gitlab"
"#,
        )?;
        ws.commit("Configure release reconstruction")?;
        ws.tag("v0.1.0", "Initial release")?;
        write_test_change(&ws.path, &["cross-checkout"])?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;

        let shim_state = tempfile::TempDir::new()?;
        let pending_log = shim_state.path().join("pending.log");
        let (_pending_dir, pending_glab) = glab_shim_with_status(&pending_log, "running")?;
        let interrupted = run_with_path_prefix(
            &ws,
            pending_glab.parent().unwrap(),
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        assert!(!interrupted.status.success());
        assert!(String::from_utf8_lossy(&interrupted.stderr).contains("awaiting exact-SHA checks"));
        assert!(
            git(&ws.path, &["ls-remote", "--tags", "origin", "v0.1.1"])?
                .stdout
                .is_empty()
        );
        let remote_head = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?.stdout;

        let clone_root = tempfile::TempDir::new()?;
        let clone = clone_root.path().join("checkout");
        let cloned = Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()])
            .output()?;
        assert!(cloned.status.success(), "{}", String::from_utf8_lossy(&cloned.stderr));
        git(&clone, &["config", "user.name", "Cargo-Rail Test"])?;
        git(&clone, &["config", "user.email", "cargo-rail@example.com"])?;

        let status = run_cargo_rail(&clone, &["rail", "release", "status", "--format", "json"])?;
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        let transaction = status["transactions"][0]["transaction_id"].as_str().unwrap();
        assert_eq!(status["transactions"][0]["recoverability"], "missing_record");
        assert_eq!(
            status["transactions"][0]["exact_sha"].as_str().unwrap().as_bytes(),
            remote_head.split(|b| *b == b'\t').next().unwrap()
        );

        let refused = run_cargo_rail(&clone, &["rail", "release", "resume", transaction])?;
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("original release record"));
        assert!(
            git(&clone, &["ls-remote", "--tags", "origin", "v0.1.1"])?
                .stdout
                .is_empty()
        );
        let bundle = clone_root.path().join("records");
        let exported = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "record",
                "export",
                transaction,
                bundle.to_str().unwrap(),
            ],
        )?;
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stdout)
        );
        let exported: serde_json::Value = serde_json::from_slice(&exported.stdout)?;
        let imported = run_cargo_rail(
            &clone,
            &[
                "rail",
                "release",
                "record",
                "import",
                bundle.to_str().unwrap(),
                "--intent",
                exported["intent"].as_str().unwrap(),
                "--source",
                exported["source"].as_str().unwrap(),
            ],
        )?;
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stdout)
        );

        let green_log = shim_state.path().join("green.log");
        let (_green_dir, green_glab) = glab_shim_with_status(&green_log, "success")?;
        let path = format!(
            "{}:{}",
            green_glab.parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let resumed = cargo_rail_command(&clone)?
            .env("PATH", path)
            .args(["rail", "release", "resume", transaction])
            .output()?;
        assert!(
            resumed.status.success(),
            "second-checkout resume failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert!(
            !git(&clone, &["ls-remote", "--tags", "origin", "v0.1.1"])?
                .stdout
                .is_empty()
        );
        assert_eq!(
            git(&clone, &["ls-remote", "origin", "refs/heads/main"])?.stdout,
            remote_head,
            "reconstruction must not create another release commit"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_hook_failure_streams_and_preserves_both_output_streams() {
    let result: Result<()> = (|| {
        let (ws, _remote) = push_release_workspace("push-hook-diagnostics")?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
echo "hook stdout: release intent was rejected"
echo "hook stderr: policy details" >&2
exit 1
"#,
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
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
        let (ws, _remote) = push_release_workspace("push-hook-json")?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
echo "hook stdout: machine-readable release rejection"
echo "hook stderr: machine-readable policy details" >&2
exit 1
"#,
        )?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
                "--json",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
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

#[test]
fn test_release_resume_reconciles_push_that_completed_before_failure() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("push-resume", "0.1.0")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
remote_effects = "push"
"#,
        )?;
        write_test_change(&ws.path, &["push-resume"])?;

        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
            "push",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let remote_before = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;

        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        assert!(
            resumed.status.success(),
            "resume stderr:\n{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let remote_after = git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?;
        assert_eq!(
            remote_before.stdout, remote_after.stdout,
            "resume should reconcile, not create another commit"
        );
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
        assert_eq!(state["status"], "complete");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_resume_rejects_remote_repository_drift() {
    let result: Result<()> = (|| {
        let (ws, _original_remote) = push_release_workspace("push-target-drift")?;
        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
            "push",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let replacement = tempfile::TempDir::new()?;
        git(replacement.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["remote", "set-url", "origin", replacement.path().to_str().unwrap()],
        )?;

        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&resumed.stdout),
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert_eq!(resumed.status.code(), Some(2), "{combined}");
        assert!(combined.contains("release repository changed from"), "{combined}");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_remains_local_before_a_push_after_origin_drift() {
    let result: Result<()> = (|| {
        let (ws, _original_remote) = push_release_workspace("abort-before-push-drift")?;
        let initial_head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
            "commit",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let replacement = tempfile::TempDir::new()?;
        git(replacement.path(), &["init", "--bare", "--initial-branch=main"])?;
        git(
            &ws.path,
            &["remote", "set-url", "origin", replacement.path().to_str().unwrap()],
        )?;

        let aborted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "abort",
                state_path.file_stem().unwrap().to_str().unwrap(),
                "--yes",
            ],
        )?;
        assert!(
            aborted.status.success(),
            "purely local abort must not depend on the current origin\n{}\n{}",
            String::from_utf8_lossy(&aborted.stdout),
            String::from_utf8_lossy(&aborted.stderr)
        );
        assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, initial_head);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_reconciles_push_rejected_by_local_hook() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("push-abort", "0.1.0")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let initial = git(&ws.path, &["rev-parse", "HEAD"])?;
        let initial = String::from_utf8_lossy(&initial.stdout).trim().to_string();

        let hook_path = ws.path.join(".git/hooks/pre-push");
        std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut perms = std::fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&hook_path, perms)?;
        }
        ws.write_release_config(
            r#"tag_format = "v{version}"
remote_effects = "push"
"#,
        )?;
        write_test_change(&ws.path, &["push-abort"])?;

        let interrupted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(
            !interrupted.status.success(),
            "local pre-push hook should reject release push"
        );
        let state_path = only_release_state(&ws.path)?;

        let aborted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "abort",
                state_path.file_stem().unwrap().to_str().unwrap(),
                "--yes",
            ],
        )?;
        assert!(
            aborted.status.success(),
            "abort stderr:\n{}",
            String::from_utf8_lossy(&aborted.stderr)
        );
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{}\n", initial).as_bytes()
        );
        assert!(git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout.is_empty());
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.0\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_json_output() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("json-release", "0.1.0")?;

        // Configure release
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["json-release"])?;

        // Run release plan with --json
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--json", "--bump", "patch"])?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "release check --json should exit 1 when changes are pending"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| panic!("release --json should output valid JSON. stdout: {}", stdout));
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["command"], serde_json::json!("release"));
        assert_eq!(json["mode"], serde_json::json!("check"));
        assert_eq!(json["result"], serde_json::json!("pending_changes"));
        assert_eq!(json["exit_code"], serde_json::json!(1));
        assert!(json.get("release_plan").is_some());
        assert!(json.get("mutation_plan").is_some());

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_mutation_plan_binds_the_captured_config_override() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-config-override", "0.1.0")?;
        ws.write_release_config("")?;
        let config = ws.path.join("custom-rail.toml");
        std::fs::rename(ws.path.join(".config/rail.toml"), &config)?;
        write_test_change(&ws.path, &["release-config-override"])?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "--config",
                "custom-rail.toml",
                "release",
                "check",
                "--format",
                "json",
                "--bump",
                "patch",
            ],
        )?;
        assert_eq!(output.status.code(), Some(1), "release check: {output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let inputs = json["mutation_plan"]["declared_inputs"]
            .as_array()
            .context("release mutation declared inputs")?;
        assert!(
            inputs.iter().any(|input| input["path"] == "custom-rail.toml"),
            "captured config override was not drift-bound: {inputs:?}"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release --skip-tag flag
#[test]
fn test_release_skip_tag_flag() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("skip-tag-crate", "0.1.0")?;

        // Configure release
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["skip-tag-crate"])?;

        // Run release plan with --skip-tag
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--skip-tag", "--bump", "patch"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Exit code 1 = check found pending changes (correct behavior)
        assert!(
            output.status.code() == Some(1),
            "release --check should exit 1 when release pending"
        );
        assert!(
            stdout.contains("--skip-tag") || !stdout.contains("Tag:") || stdout.contains("skip"),
            "Should indicate tags are skipped in output.\nOutput:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Registry publication is absent unless the operator authorizes it positively.
#[test]
fn test_release_publication_is_default_deny() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("skip-pub-crate", "0.1.0")?;

        // Configure release
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["skip-pub-crate"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "patch"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Exit code 1 = check found pending changes (correct behavior)
        assert!(
            output.status.code() == Some(1),
            "release --check should exit 1 when release pending"
        );
        assert!(
            stdout.contains("not authorized; pass --publish") && stdout.contains("0 to publish"),
            "release preview must show that publication lacks positive authorization.\nOutput:\n{}",
            stdout
        );

        let missing_remote_authority = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--bump", "patch"],
        )?;
        assert_eq!(missing_remote_authority.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&missing_remote_authority.stderr)
                .contains("--publish cannot be combined with release.remote_effects = \"none\"")
        );

        ws.write_release_config("remote_effects = \"push\"\n")?;
        let missing_check_config_authority = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--bump", "patch"],
        )?;
        let missing_config_authority = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--bump", "patch", "--publish", "--yes"],
        )?;
        assert_eq!(missing_check_config_authority.status.code(), Some(2));
        assert_eq!(missing_config_authority.status.code(), Some(2));
        let check_error = String::from_utf8_lossy(&missing_check_config_authority.stderr);
        let run_error = String::from_utf8_lossy(&missing_config_authority.stderr);
        let authority_error = "--publish requires release.registry_publication = \"crates-io\"";
        assert!(check_error.contains(authority_error), "{check_error}");
        assert!(run_error.contains(authority_error), "{run_error}");

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn publication_check_plan_is_accepted_by_the_matching_publish_run() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("publication-plan-parity")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;
        ws.add_crate("publication-core", "0.1.0", &[])?;
        ws.add_crate(
            "publication-app",
            "0.1.0",
            &[(
                "publication-core",
                "{ version = \"^0.1.0\", path = \"../publication-core\" }",
            )],
        )?;
        ws.set_remote("https://github.com/loadingalias/publication-plan-parity.git")?;
        ws.commit("Configure publication preview parity")?;
        write_test_change_levels(&ws.path, &[("publication-core", "patch"), ("publication-app", "patch")])?;

        let preview = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "publication-core",
                "--publication",
                "--bump",
                "minor",
                "--skip-tag",
                "--include-dependents",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(
            preview.status.code(),
            Some(1),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&preview.stdout),
            String::from_utf8_lossy(&preview.stderr)
        );
        let preview: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        assert_eq!(preview["command"], "release");
        assert_eq!(preview["mode"], "check");
        assert_eq!(preview["result"], "pending_changes");
        assert_eq!(preview["release_plan"]["summary"]["total_crates"], 2);
        let core = preview["release_plan"]["crates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|planned| planned["name"] == "publication-core")
            .unwrap();
        assert_eq!(core["new_version"], "0.2.0");
        let codes = preview["mutation_plan"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|action| action["code"].as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"PUBLISH_CRATE"), "{codes:?}");
        assert!(codes.contains(&"PUSH_RELEASE_COMMIT"), "{codes:?}");
        assert!(codes.contains(&"AWAIT_EXACT_SHA_CHECKS"), "{codes:?}");
        assert!(!codes.contains(&"CREATE_TAG"), "{codes:?}");
        assert!(!codes.contains(&"PUSH_RELEASE_TAGS"), "{codes:?}");

        let plan_path = ws.path.join("target/publication-preview-plan.json");
        std::fs::create_dir_all(plan_path.parent().unwrap())?;
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&preview["mutation_plan"])?)?;
        let shim_state = tempfile::TempDir::new()?;
        let log_path = shim_state.path().join("cargo.log");
        let published_path = shim_state.path().join("published");
        let shim = publication_boundary_shim(&log_path, &published_path)?;
        let path = format!(
            "{}:{}",
            shim.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let journal_root = ws.path.join("target/cargo-rail/releases");
        std::fs::create_dir_all(journal_root.parent().unwrap())?;
        std::fs::write(&journal_root, "blocked journal directory")?;
        let run = cargo_rail_command(&ws.path)?
            .env("PATH", path)
            .args([
                "rail",
                "release",
                "run",
                "publication-core",
                "--bump",
                "minor",
                "--publish",
                "--skip-tag",
                "--include-dependents",
                "--plan",
                plan_path.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()?;
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert_eq!(run.status.code(), Some(2), "stdout:\n{}\nstderr:\n{stderr}", stdout);
        assert!(
            stdout.contains("release state directory is not a contained real directory"),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !published_path.exists(),
            "parity validation must stop before publication"
        );
        assert_eq!(std::fs::read_to_string(&journal_root)?, "blocked journal directory");
        for crate_name in ["publication-core", "publication-app"] {
            assert!(
                std::fs::read_to_string(ws.path.join("crates").join(crate_name).join("Cargo.toml"))?
                    .contains("version = \"0.1.0\"")
            );
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_local_effects_never_plan_registry_or_remote_actions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("local-release-authority", "0.1.0")?;
        ws.write_release_config("remote_effects = \"none\"\n")?;
        write_test_change(&ws.path, &["local-release-authority"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--bump", "patch", "--format", "json"],
        )?;
        assert_eq!(output.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let codes = json["mutation_plan"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|action| action["code"].as_str())
            .collect::<Vec<_>>();
        for external in [
            "PUBLISH_CRATE",
            "PUSH_RELEASE_COMMIT",
            "AWAIT_EXACT_SHA_CHECKS",
            "PUSH_RELEASE_TAGS",
            "CREATE_FORGE_RELEASE",
            "PUBLISH_FORGE_RELEASE",
        ] {
            assert!(
                !codes.contains(&external),
                "local-only plan contains {external}: {codes:?}"
            );
        }

        let rejected = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--bump", "patch", "--publish", "--yes"],
        )?;
        assert_eq!(rejected.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("--publish cannot be combined with release.remote_effects = \"none\"")
        );

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
            ws.write_release_config(&format!(
                "remote_effects = \"push\"\nregistry_publication = \"crates-io\"\n\n[crates.{name}.release]\npublish = true\n"
            ))?;
            std::fs::create_dir_all(ws.path.join(".changes"))?;
            std::fs::write(
                ws.path.join(".changes/publish-authority.md"),
                format!("---\n\"{name}\" = \"patch\"\n---\n\nExercise Cargo publication authority.\n"),
            )?;

            let output = run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "check",
                    name,
                    "--bump",
                    "patch",
                    "--publication",
                    "--format",
                    "json",
                ],
            )?;
            assert_eq!(output.status.code(), Some(1));
            let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(json["release_plan"]["summary"]["total_crates"], 1);
            assert_eq!(json["count"], 0, "Cargo registry authority was widened for {name}");
            assert_eq!(json["crates"], serde_json::json!([]));
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

        // Configure release
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["explicit-ver"])?;

        // Run release with explicit version
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "2.0.0"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Exit code 1 = check found pending changes (correct behavior)
        assert!(
            output.status.code() == Some(1),
            "release --check should exit 1 when release pending"
        );
        assert!(
            stdout.contains("2.0.0"),
            "Should show explicit version in plan.\nOutput:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// release.changelog.relative_to tests
/// Test default changelog relative_to behavior (crate-relative)
#[test]
fn test_changelog_relative_to_crate_default() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("changelog-crate-rel")?;
        ws.set_remote("git@github.com:org/repo.git")?;

        // Don't set relative_to - should default to "crate"
        ws.write_release_config(
            r#"semver_check = "off"
[release.changelog]
path = "CHANGELOG.md"
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
        ws.commit("feat: add v2 function")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

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

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn compare_linked_changelog_round_trips_through_preview_and_apply() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("compare-linked-changelog")?;
        ws.set_remote("git@github.com:org/repo.git")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        let changelog_path = ws.path.join("crates/lib-a/CHANGELOG.md");
        std::fs::write(
            &changelog_path,
            "# Changelog\n\n## [0.1.0](https://github.com/org/repo/compare/lib-a-v0.0.0...lib-a-v0.1.0) - 2026-01-01\n\n- Initial release.\n",
        )?;
        ws.commit("Add lib-a")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let preview = run_cargo_rail(
            &ws.path,
            &[
                "rail", "release", "check", "lib-a", "--bump", "patch", "--format", "json",
            ],
        )?;
        assert_eq!(preview.status.code(), Some(1), "{preview:?}");
        let document: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        let expected = "## [0.1.1](https://github.com/org/repo/compare/lib-a-v0.1.0...lib-a-v0.1.1) - ";
        let planned = document["release_plan"]["crates"][0]["presentation"]["changelog"]["content"]
            .as_str()
            .unwrap();
        assert!(planned.contains(expected), "planned changelog:\n{planned}");

        let plan_path = ws.path.join("target/compare-linked-release-plan.json");
        std::fs::create_dir_all(plan_path.parent().unwrap())?;
        std::fs::write(&plan_path, &preview.stdout)?;
        let apply = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "lib-a",
                "--bump",
                "patch",
                "--yes",
                "--plan",
                plan_path.to_str().unwrap(),
            ],
        )?;
        assert!(
            apply.status.success(),
            "release apply failed:\n{}",
            String::from_utf8_lossy(&apply.stderr)
        );
        let applied = std::fs::read_to_string(changelog_path)?;
        assert!(applied.contains(expected), "applied changelog:\n{applied}");
        assert!(applied.contains("## [0.1.0](https://github.com/org/repo/compare/"));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "workspace" creates changelog at workspace root
#[test]
fn test_changelog_relative_to_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("changelog-ws-rel")?;
        ws.set_remote("git@github.com:org/repo.git")?;

        // Explicitly set relative_to = "workspace"
        ws.write_release_config(
            r#"semver_check = "off"
[release.changelog]
path = "CHANGELOG.md"
relative_to = "workspace"
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
        ws.commit("feat: add v2 function")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

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
            content.contains("lib-a") || content.contains("0.1.1"),
            "Changelog should contain release info. Content:\n{}",
            content
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_rejects_an_absolute_changelog_path_outside_the_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("changelog-outside-workspace")?;
        let outside = tempfile::TempDir::new()?;
        let outside_path = outside.path().join("CHANGELOG.md");
        ws.write_release_config(&format!(
            r#"semver_check = "off"
[release.changelog]
path = "{}"
relative_to = "workspace"
"#,
            outside_path.display().to_string().replace('\\', "\\\\")
        ))?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "patch"])?;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("escapes workspace") || stderr.contains("outside git worktree"),
            "outside changelog path should fail before mutation\nstderr:\n{}",
            stderr
        );
        assert!(!outside_path.exists());
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
        ws.write_release_config(
            r#"
[release.changelog]
path = "CHANGELOG.md"
relative_to = "workspace"
"#,
        )?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;
        let outside = tempfile::TempDir::new()?;
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "outside\n")?;
        symlink(&victim, ws.path.join("CHANGELOG.md"))?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--bump", "patch"])?;
        assert!(!output.status.success());
        assert_eq!(std::fs::read_to_string(victim)?, "outside\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that parent directories are auto-created for changelog paths
#[test]
fn test_changelog_parent_directories_auto_created() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("changelog-auto-mkdir")?;
        ws.set_remote("git@github.com:org/repo.git")?;

        // Use a nested path that doesn't exist
        ws.write_release_config(
            r#"semver_check = "off"
[release.changelog]
path = "docs/changelogs/CHANGELOG.md"
relative_to = "workspace"
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
        ws.commit("feat: add v2 function")?;
        write_test_change(&ws.path, &["lib-a"])?;

        // docs/changelogs/ doesn't exist yet - should be auto-created
        assert!(!ws.path.join("docs/changelogs").exists());

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release should succeed with auto-created directories\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
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

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release.changelog.relative_to = "crate" with custom path creates in crate subdir
#[test]
fn test_changelog_relative_to_crate_custom_path() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("changelog-crate-custom")?;
        ws.set_remote("git@github.com:org/repo.git")?;

        // Use custom path with crate-relative
        ws.write_release_config(
            r#"semver_check = "off"
[release.changelog]
path = "docs/CHANGES.md"
relative_to = "crate"
"#,
        )?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial lib-a")?;

        ws.modify_file("lib-a", "src/lib.rs", "pub fn v2() {}")?;
        ws.commit("feat: add v2 function")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "release should succeed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        // Changelog should be at crates/lib-a/docs/CHANGES.md
        let changelog_path = ws.path.join("crates/lib-a/docs/CHANGES.md");
        assert!(
            changelog_path.exists(),
            "Changelog should exist at custom crate-relative path: {}",
            changelog_path.display()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Prerelease Bump Tests

/// Test --bump prerelease from stable version
#[test]
fn test_bump_prerelease_from_stable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("prerelease-test", "1.0.0")?;
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["prerelease-test"])?;

        // Run release plan with --bump prerelease
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "prerelease"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Should show 1.0.0 -> 1.0.0-rc.1
        assert!(
            stdout.contains("1.0.0-rc.1"),
            "Should bump to rc.1 prerelease.\nOutput:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test --bump prerelease increments existing prerelease
#[test]
fn test_bump_prerelease_increment() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("prerelease-inc", "2.0.0-rc.1")?;
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["prerelease-inc"])?;

        // Run release plan with --bump prerelease
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "prerelease"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Should show 2.0.0-rc.1 -> 2.0.0-rc.2
        assert!(
            stdout.contains("2.0.0-rc.2"),
            "Should increment to rc.2.\nOutput:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test --bump release strips prerelease suffix
#[test]
fn test_bump_release_strips_prerelease() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-strip", "1.5.0-beta.3")?;
        ws.write_release_config("")?;
        write_test_change(&ws.path, &["release-strip"])?;

        // Run release plan with --bump release
        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--bump", "release"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Should show 1.5.0-beta.3 -> 1.5.0
        // The output contains both versions in format "1.5.0-beta.3 → 1.5.0"
        assert!(
            stdout.contains("1.5.0-beta.3") && stdout.contains("→ 1.5.0"),
            "Should strip prerelease to 1.5.0.\nOutput:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Extended Check Tests

/// Extended checks run under local-only release authority.
#[test]
fn test_release_check_extended_runs_with_local_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("ext-check", "0.1.0")?;
        ws.write_release_config("remote_effects = \"none\"\n")?;
        write_test_change(&ws.path, &["ext-check"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--extended", "--all", "--format", "json"],
        )?;
        assert!(matches!(output.status.code(), Some(1 | 2)), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["readiness"]["scope"], "local");
        assert_eq!(json["readiness"]["planned_effects"]["registry_publication"], false);
        let checks = json["extended"][0]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["check"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(checks, ["publish-dry-run", "msrv", "semver-checks"]);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test release check --extended with JSON output
#[test]
fn test_release_check_extended_json() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("ext-json", "0.1.0")?;
        ws.write_release_config("remote_effects = \"push\"\nregistry_publication = \"crates-io\"\n")?;
        write_test_change(&ws.path, &["ext-json"])?;

        // Run release check with --extended --json --all
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "--publication",
                "--extended",
                "--json",
                "--all",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Should be valid JSON with extended field
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(
            parsed.is_ok(),
            "Extended check --json should output valid JSON.\nstdout:\n{}",
            stdout
        );

        let json = parsed.unwrap();
        assert_eq!(json["schema_version"], serde_json::json!(1));
        assert_eq!(json["command"], serde_json::json!("release"));
        assert_eq!(json["mode"], serde_json::json!("check"));
        assert!(
            json["result"] == serde_json::json!("pending_changes") || json["result"] == serde_json::json!("failed")
        );
        assert!(
            json["exit_code"] == serde_json::json!(1) || json["exit_code"] == serde_json::json!(2),
            "release check extended should report pending exit_code 1 or validation failure exit_code 2"
        );
        assert!(
            json.get("extended").is_some(),
            "JSON should contain 'extended' field.\nJSON:\n{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
        assert_eq!(json["readiness"]["scope"], "publication");
        assert_eq!(json["readiness"]["planned_effects"]["registry_publication"], true);

        Ok(())
    })();
    super::helpers::finish_test(result);
}

// Release Safety Tests (Branch Detection)

#[test]
fn release_rejects_unsafe_tag_names_before_mutation() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("unsafe-release-tag", "0.1.0")?;
        ws.write_release_config(
            r#"tag_prefix = "-"
tag_format = "{prefix}{version}"
"#,
        )?;
        ws.commit("Configure unsafe release tag")?;
        std::fs::write(ws.path.join("src/lib.rs"), "pub fn changed() {}")?;
        let initial_head = ws.commit("Change unsafe release tag crate")?;
        write_test_change(&ws.path, &["unsafe-release-tag"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("is not a safe Git ref name"));
        assert_eq!(
            String::from_utf8_lossy(&git(&ws.path, &["rev-parse", "HEAD"])?.stdout).trim(),
            initial_head
        );
        assert!(git(&ws.path, &["tag", "--list"])?.stdout.is_empty());
        assert!(
            !ws.path.join("target/cargo-rail/releases").exists(),
            "invalid tag configuration must fail before journal creation"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_resume_reconciles_tag_created_before_failure() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-resume-tag")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn resumed() {}")?;
        ws.commit("feat: resumable release")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
            "tag",
        )?;
        assert!(!interrupted.status.success());
        assert!(String::from_utf8_lossy(&interrupted.stderr).contains("cargo rail release resume"));
        let state_path = only_release_state(&ws.path)?;
        let before = git(&ws.path, &["rev-list", "--count", "HEAD"])?;

        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        assert!(
            resumed.status.success(),
            "resume failed:\n{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let after = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
        assert_eq!(
            before.stdout, after.stdout,
            "resume must not duplicate the release commit"
        );
        let tags = git(&ws.path, &["tag", "--list", "lib-a-v0.1.1"])?;
        assert_eq!(String::from_utf8_lossy(&tags.stdout).lines().count(), 1);
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
        assert_eq!(state["status"], "complete");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_remote_records_survive_runner_loss_without_moving_the_source_branch() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("remote-record", "0.1.0")?;
        ws.write_release_config(
            "tag_format = 'v{version}'\nsemver_check = 'off'\nsign_tags = false\nremote_effects = 'gitlab'\n",
        )?;
        write_test_change(&ws.path, &["remote-record"])?;
        let initial = ws.commit("Review remote release")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;
        let prepared = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--prepare",
                "--retain-remote",
                "--yes",
                "--format",
                "json",
            ],
        )?;
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stdout)
        );
        let prepared: serde_json::Value = serde_json::from_slice(&prepared.stdout)?;
        let transaction = prepared["transaction_id"].as_str().unwrap();
        let source = prepared["source"].as_str().unwrap();
        let intent = prepared["intent"].as_str().unwrap();
        assert_ne!(source, initial);
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/heads/main"])?.stdout,
            format!("{initial}\n").as_bytes()
        );
        let retained = git(remote.path(), &["show", "refs/notes/cargo-rail/active:record.json"])?;
        let retained: serde_json::Value = serde_json::from_slice(&retained.stdout)?;
        assert_eq!(retained["intent"]["identity"], intent);
        assert_eq!(retained["preparation"]["commit"], source);
        assert_eq!(retained["remote_storage"], true);
        assert_eq!(retained["commit_push"]["status"], "pending");
        let recovery = tempfile::tempdir()?;
        let clone = recovery.path().join("clone");
        let cloned = git(
            recovery.path(),
            &["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()],
        )?;
        assert!(cloned.status.success());
        git(&clone, &["config", "user.name", "Release fixture"])?;
        git(&clone, &["config", "user.email", "release@example.invalid"])?;
        let fetched = run_cargo_rail(&clone, &["rail", "release", "record", "fetch"])?;
        assert!(fetched.status.success(), "{}", String::from_utf8_lossy(&fetched.stdout));
        let fetched: serde_json::Value = serde_json::from_slice(&fetched.stdout)?;
        assert_eq!(fetched["transaction_id"], transaction);
        assert_eq!(fetched["intent"], intent);
        git(&clone, &["reset", "--hard", source])?;
        let resumed = cargo_rail_command(&clone)?
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    glab.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .args(["rail", "release", "resume"])
            .output()?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let terminal = git(remote.path(), &["show", "refs/notes/cargo-rail/active:record.json"])?;
        let terminal: serde_json::Value = serde_json::from_slice(&terminal.stdout)?;
        assert_eq!(terminal["status"], "complete");
        assert_eq!(terminal["intent"], retained["intent"]);
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/tags/v0.1.1^{}"])?.stdout,
            format!("{source}\n").as_bytes()
        );
        let refs = git(&clone, &["for-each-ref", "--format=%(refname)", "refs/heads"])?;
        assert_eq!(refs.stdout, b"refs/heads/main\n");
        write_test_change(&clone, &["remote-record"])?;
        git(&clone, &["add", ".changes"])?;
        git(&clone, &["commit", "-m", "Review the next release"])?;
        let second = cargo_rail_command(&clone)?
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    glab.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .args([
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--prepare",
                "--retain-remote",
                "--yes",
                "--format",
                "json",
            ])
            .output()?;
        assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stdout));
        let second: serde_json::Value = serde_json::from_slice(&second.stdout)?;
        assert_ne!(second["transaction_id"], transaction);
        let active = git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout;
        let fetched_old = run_cargo_rail(&clone, &["rail", "release", "record", "fetch", transaction])?;
        assert!(
            fetched_old.status.success(),
            "{}",
            String::from_utf8_lossy(&fetched_old.stdout)
        );
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            active,
            "reading an old transaction must not publish it as the active transaction"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn terminal_remote_lease_from_an_older_schema_does_not_block_a_new_release() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("remote-schema-upgrade", "0.1.0")?;
        ws.write_release_config("semver_check = 'off'\nsign_tags = false\nremote_effects = 'gitlab'\n")?;
        write_test_change(&ws.path, &["remote-schema-upgrade"])?;
        ws.commit("Review release after schema upgrade")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let legacy_head = seed_legacy_release_lease(remote.path(), "complete")?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;

        let prepared = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--prepare",
                "--retain-remote",
                "--yes",
                "--format",
                "json",
            ],
        )?;
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        let prepared: serde_json::Value = serde_json::from_slice(&prepared.stdout)?;
        let transaction = prepared["transaction_id"].as_str().unwrap();
        assert_ne!(transaction, "release-legacy-v9");
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/release-legacy-v9"])?.stdout,
            format!("{legacy_head}\n").as_bytes(),
            "starting a new transaction must preserve the older transaction record"
        );
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            git(
                remote.path(),
                &["rev-parse", &format!("refs/notes/cargo-rail/{transaction}")]
            )?
            .stdout,
            "the active lease must move to the new transaction"
        );
        let fetched = run_cargo_rail(&ws.path, &["rail", "release", "record", "fetch", "release-legacy-v9"])?;
        assert!(
            !fetched.status.success(),
            "transaction records from older schemas must remain strict"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn active_remote_lease_from_an_older_schema_still_blocks_a_new_release() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("remote-active-upgrade", "0.1.0")?;
        ws.write_release_config("semver_check = 'off'\nsign_tags = false\nremote_effects = 'gitlab'\n")?;
        write_test_change(&ws.path, &["remote-active-upgrade"])?;
        ws.commit("Review concurrent release")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let legacy_head = seed_legacy_release_lease(remote.path(), "active")?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;

        let rejected = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--prepare",
                "--retain-remote",
                "--yes",
            ],
        )?;
        assert!(!rejected.status.success(), "a concurrent remote lease was accepted");
        let error = String::from_utf8_lossy(&rejected.stderr);
        assert!(
            error.contains("remote release transaction 'release-legacy-v9' is already active"),
            "{error}"
        );
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            format!("{legacy_head}\n").as_bytes(),
            "rejecting concurrent work must not move the active lease"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_remote_record_accepts_a_lost_push_acknowledgment() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("remote-record-ack", "0.1.0")?;
        ws.write_release_config("semver_check = 'off'\nsign_tags = false\nremote_effects = 'gitlab'\n")?;
        write_test_change(&ws.path, &["remote-record-ack"])?;
        ws.commit("Review remote release")?;
        let remote = tempfile::TempDir::new()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;
        let prepared = run_with_lost_git_acknowledgment_and_path_prefix(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--prepare",
                "--retain-remote",
                "--yes",
                "--format",
                "json",
            ],
            "record-push",
            glab.parent(),
        )?;
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        let prepared: serde_json::Value = serde_json::from_slice(&prepared.stdout)?;
        let transaction = prepared["transaction_id"].as_str().unwrap();
        let transaction_ref = format!("refs/notes/cargo-rail/{transaction}");
        assert_eq!(
            git(remote.path(), &["rev-parse", &transaction_ref])?.stdout,
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            "the acknowledged transaction and active refs must name the same stored record"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_record_handoff_preserves_intent_and_resumes_in_a_fresh_clone() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-handoff", "0.1.0")?;
        ws.write_release_config("tag_format = 'v{version}'\nsemver_check = 'off'\nsign_tags = false\n")?;
        write_test_change(&ws.path, &["release-handoff"])?;
        ws.commit("Review the release intent")?;
        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
            "tag",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        let transaction = state["transaction_id"].as_str().unwrap();
        let intent = state["intent"]["identity"].as_str().unwrap();
        let source = state["preparation"]["commit"].as_str().unwrap();
        let transport = tempfile::tempdir()?;
        let bundle = transport.path().join("bundle");
        let exported = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "record",
                "export",
                transaction,
                bundle.to_str().unwrap(),
            ],
        )?;
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stdout)
        );
        let output: serde_json::Value = serde_json::from_slice(&exported.stdout)?;
        assert_eq!(output["intent"], intent);
        assert_eq!(output["source"], source);
        let portable = std::fs::read_to_string(bundle.join("record.json"))?;
        assert!(!portable.contains(ws.path.to_str().unwrap()));
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/release-record-v10.schema.json"))?;
        let validator = jsonschema::validator_for(&schema)?;
        let record: serde_json::Value = serde_json::from_str(&portable)?;
        let errors = validator
            .iter_errors(&record)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(
            errors.is_empty(),
            "source record violates the release schema: {errors:?}"
        );
        let clone = transport.path().join("clone");
        let cloned = git(
            transport.path(),
            &[
                "clone",
                "--no-local",
                ws.path.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        )?;
        assert!(cloned.status.success(), "{}", String::from_utf8_lossy(&cloned.stderr));
        git(&clone, &["config", "user.name", "Release fixture"])?;
        git(&clone, &["config", "user.email", "release@example.invalid"])?;
        assert!(!clone.join("target/cargo-rail/releases").exists());
        let rejected = run_cargo_rail(
            &clone,
            &[
                "rail",
                "release",
                "record",
                "import",
                bundle.to_str().unwrap(),
                "--intent",
                "sha256:wrong",
                "--source",
                source,
            ],
        )?;
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stdout).contains("authorized intent"));
        assert!(
            !clone
                .join("target/cargo-rail/releases")
                .join(format!("{transaction}.json"))
                .exists()
        );
        let imported = run_cargo_rail(
            &clone,
            &[
                "rail",
                "release",
                "record",
                "import",
                bundle.to_str().unwrap(),
                "--intent",
                intent,
                "--source",
                source,
            ],
        )?;
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stdout)
        );
        let received = only_release_state(&clone)?;
        let resumed = run_cargo_rail(&clone, &["rail", "release", "resume"])?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let completed: serde_json::Value = serde_json::from_slice(&std::fs::read(received)?)?;
        assert_eq!(completed["status"], "complete");
        assert_eq!(completed["intent"], state["intent"]);
        assert_eq!(
            git(&clone, &["rev-parse", "HEAD"])?.stdout,
            format!("{source}\n").as_bytes()
        );
        assert_eq!(
            git(&clone, &["rev-parse", "v0.1.1^{}"])?.stdout,
            format!("{source}\n").as_bytes()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_resume_rejects_same_branch_head_movement() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-resume-head-drift")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}")?;
        ws.commit("feat: prepare release")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
            "tag",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn moved_after_release() {}")?;
        ws.commit("feat: move release branch")?;

        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        assert!(!resumed.status.success());
        let stderr = String::from_utf8_lossy(&resumed.stderr);
        assert!(
            stderr.contains("persisted release commit"),
            "resume should reject same-branch HEAD drift\nstderr:\n{}",
            stderr
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn unsupported_release_journal_blocks_resume_and_new_transactions() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("v025-release-resume", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
"#,
        )?;
        write_test_change(&ws.path, &["v025-release-resume"])?;
        let before = String::from_utf8_lossy(&git(&ws.path, &["rev-list", "--count", "HEAD"])?.stdout)
            .trim()
            .parse::<usize>()?;
        let interrupted = run_with_rejected_commit(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let mut state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        state["schema_version"] = serde_json::json!(5);
        let unsupported = serde_json::to_vec(&state)?;
        std::fs::write(&state_path, &unsupported)?;
        for args in [
            vec![
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
            vec![
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        ] {
            let output = run_cargo_rail(&ws.path, &args)?;
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert_eq!(std::fs::read(&state_path)?, unsupported);
            assert_eq!(
                String::from_utf8_lossy(&git(&ws.path, &["rev-list", "--count", "HEAD"])?.stdout)
                    .trim()
                    .parse::<usize>()?,
                before
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_recovery_survives_invalid_metadata_and_clean_refuses_active_state() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-status-active", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
"#,
        )?;
        write_test_change(&ws.path, &["release-status-active"])?;
        let interrupted = run_with_rejected_commit(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let manifest_path = ws.path.join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)?;
        std::fs::write(&manifest_path, "not valid Cargo metadata\n")?;

        let status = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "status",
                state_path.to_str().unwrap(),
                "--format",
                "json",
            ],
        )?;
        assert!(
            status.status.success(),
            "status must not load broken Cargo metadata: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(status["transactions"][0]["state"], "planned:active");
        assert_eq!(status["transactions"][0]["ambiguity"], true);
        assert!(
            status["transactions"][0]["safe_operator_command"]
                .as_str()
                .unwrap_or_default()
                .contains("release resume")
        );

        std::fs::write(&manifest_path, manifest)?;
        let clean = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert!(!clean.status.success(), "clean must refuse an active journal");
        assert!(String::from_utf8_lossy(&clean.stderr).contains("clean refused active release transaction"));

        std::fs::write(&manifest_path, "not valid Cargo metadata again\n")?;
        let config_path = ws.path.join(".config/rail.toml");
        assert!(config_path.exists(), "test release config disappeared before recovery");
        let resumed = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "resume",
                state_path.file_stem().unwrap().to_str().unwrap(),
            ],
        )?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let cleaned = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
        assert!(!state_path.exists(), "clean should prune the completed journal");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_resume_reconciles_commits_before_and_after_journal_observation() {
    let result: Result<()> = (|| {
        for observed in [false, true] {
            let ws = TestWorkspace::new_single_crate("journal-fault", "0.1.0")?;
            ws.write_release_config(
                r#"tag_format = "v{version}"
"#,
            )?;
            write_test_change(&ws.path, &["journal-fault"])?;
            let before = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
            let interrupted = run_with_lost_git_acknowledgment(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "run",
                    "--all",
                    "--bump",
                    "patch",
                    "--skip-tag",
                    "--yes",
                ],
                "commit",
            )?;
            assert!(!interrupted.status.success());
            let state_path = only_release_state(&ws.path)?;
            let after_fault = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
            assert_ne!(
                before.stdout, after_fault.stdout,
                "the commit effect should have completed"
            );

            let mut state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
            assert_eq!(state["preparation"]["status"], "committing");
            if observed {
                let head = git(&ws.path, &["rev-parse", "HEAD"])?;
                let head = String::from_utf8(head.stdout)?.trim().to_owned();
                state["preparation"] = serde_json::json!({"status": "complete", "commit": head});
                std::fs::write(&state_path, serde_json::to_vec_pretty(&state)?)?;
            }

            let resumed = run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "resume",
                    state_path.file_stem().unwrap().to_str().unwrap(),
                ],
            )?;
            assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
            let after_resume = git(&ws.path, &["rev-list", "--count", "HEAD"])?;
            assert_eq!(
                after_fault.stdout, after_resume.stdout,
                "resume must not duplicate the commit"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_resume_rejects_a_replaced_preparation_tree_or_message() {
    let result: Result<()> = (|| {
        for replace_tree in [false, true] {
            let ws = TestWorkspace::new_single_crate("preparation-binding", "0.1.0")?;
            write_test_change(&ws.path, &["preparation-binding"])?;
            let interrupted = run_with_lost_git_acknowledgment(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "run",
                    "--all",
                    "--bump",
                    "patch",
                    "--skip-tag",
                    "--yes",
                ],
                "commit",
            )?;
            assert!(!interrupted.status.success());
            let state_path = only_release_state(&ws.path)?;
            let saved = std::fs::read(&state_path)?;
            if replace_tree {
                std::fs::write(ws.path.join("unreviewed.txt"), "unreviewed commit content")?;
                let stage = git(&ws.path, &["add", "unreviewed.txt"])?;
                assert!(stage.status.success());
                let amended = git(&ws.path, &["commit", "--amend", "--no-edit"])?;
                assert!(amended.status.success());
            } else {
                let amended = git(
                    &ws.path,
                    &["commit", "--amend", "-m", "chore(release): preparation-binding v0.1.1"],
                )?;
                assert!(amended.status.success());
            }
            let head = git(&ws.path, &["rev-parse", "HEAD"])?;
            let resumed = run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "resume",
                    state_path.file_stem().unwrap().to_str().unwrap(),
                ],
            )?;
            assert_eq!(resumed.status.code(), Some(2), "{resumed:?}");
            assert!(
                String::from_utf8_lossy(&resumed.stderr).contains("saved parent, tree, and message"),
                "{resumed:?}"
            );
            assert_eq!(std::fs::read(&state_path)?, saved);
            assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, head.stdout);
            if replace_tree {
                assert_eq!(
                    std::fs::read_to_string(ws.path.join("unreviewed.txt"))?,
                    "unreviewed commit content"
                );
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn clean_refuses_a_superseded_active_journal_without_commit_effect() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("superseded-journal", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
"#,
        )?;
        write_test_change(&ws.path, &["superseded-journal"])?;
        let interrupted = run_with_rejected_commit(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--yes",
            ],
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        std::fs::write(ws.path.join("superseding.txt"), "new release input\n")?;
        ws.commit("Supersede unstarted release plan")?;

        let cleaned = run_cargo_rail(
            &ws.path,
            &["rail", "clean", "--release-journal", state_path.to_str().unwrap()],
        )?;
        assert_eq!(cleaned.status.code(), Some(2));
        assert!(
            state_path.exists(),
            "active superseded state is not terminal cleanup authority"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_transaction_id_is_recorded_in_commits_and_terminal_status() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("release-transaction", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
"#,
        )?;
        write_test_change(&ws.path, &["release-transaction"])?;
        let released = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        assert!(
            released.status.success(),
            "{}",
            String::from_utf8_lossy(&released.stderr)
        );
        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        let transaction_id = state["transaction_id"].as_str().unwrap();
        assert_eq!(state["phase"], "released");
        let message = git(&ws.path, &["log", "-1", "--format=%B"])?;
        assert!(
            String::from_utf8_lossy(&message.stdout).contains(&format!("Rail-Release: {}", transaction_id)),
            "release commit must carry the plan-bound transaction identity"
        );

        let active = run_cargo_rail(&ws.path, &["rail", "release", "status", "--format", "json"])?;
        let active: serde_json::Value = serde_json::from_slice(&active.stdout)?;
        assert_eq!(active["transactions"], serde_json::json!([]));

        let status = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(status["transactions"][0]["state"], "released:complete");
        assert_eq!(status["transactions"][0]["recoverability"], "terminal");
        assert_eq!(
            status["transactions"][0]["safe_operator_command"],
            format!("cargo rail clean --release-journal {transaction_id}")
        );
        let cleaned = run_cargo_rail(&ws.path, &["rail", "clean", "--release-journal", transaction_id])?;
        assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
        let reconstructed = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        let reconstructed: serde_json::Value = serde_json::from_slice(&reconstructed.stdout)?;
        assert_eq!(reconstructed["transactions"][0]["state"], "record_unavailable");
        assert_eq!(reconstructed["transactions"][0]["recoverability"], "missing_record");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_abort_restores_local_state_before_remote_side_effects() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-abort-local")?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn abortable() {}")?;
        let initial = ws.commit("feat: abortable release")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
            "commit",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let aborted = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "abort",
                state_path.file_stem().unwrap().to_str().unwrap(),
                "--yes",
            ],
        )?;
        assert!(
            aborted.status.success(),
            "abort stderr:\n{}",
            String::from_utf8_lossy(&aborted.stderr)
        );
        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), initial);
        assert!(!String::from_utf8_lossy(&git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout).contains("v0.1.1"));
        let manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        assert!(manifest.contains("version = \"0.1.0\""));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_abort_retires_a_pushed_preparation_without_rewriting_its_successors() {
    let result: Result<()> = (|| {
        let (ws, _remote) = push_release_workspace("retain-pushed-preparation")?;
        let interrupted = run_with_lost_git_acknowledgment(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--retain-remote",
                "--yes",
            ],
            "release-push",
        )?;
        assert!(!interrupted.status.success());
        let state_path = only_release_state(&ws.path)?;
        let transaction = state_path.file_stem().unwrap().to_str().unwrap();
        let prepared = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
        let prepared = String::from_utf8(prepared)?.trim().to_owned();

        let ordinary_abort = run_cargo_rail(&ws.path, &["rail", "release", "abort", transaction, "--yes"])?;
        let ordinary_stderr = String::from_utf8_lossy(&ordinary_abort.stderr);
        assert_eq!(ordinary_abort.status.code(), Some(2), "{ordinary_stderr}");
        assert!(
            ordinary_stderr.contains("remote or registry side effect may already exist"),
            "{ordinary_stderr}"
        );

        std::fs::write(ws.path.join("src/lib.rs"), "pub fn corrected_after_preparation() {}\n")?;
        let successor = ws.commit("fix: correct release preparation")?;
        git(&ws.path, &["push", "origin", "main"])?;

        let retained = run_cargo_rail(
            &ws.path,
            &["rail", "release", "abort", transaction, "--retain-preparation", "--yes"],
        )?;
        assert!(
            retained.status.success(),
            "retain stderr:\n{}",
            String::from_utf8_lossy(&retained.stderr)
        );
        assert_eq!(
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
            format!("{successor}\n").as_bytes()
        );
        assert_eq!(
            git(&ws.path, &["ls-remote", "origin", "refs/heads/main"])?.stdout,
            format!("{successor}\trefs/heads/main\n").as_bytes()
        );
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.1\""));
        assert!(!ws.path.join(".changes/release-test.md").exists());

        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(record["status"], "aborted");
        assert_eq!(record["commit_push"]["status"], "complete");
        assert_eq!(record["commit_push"]["object"], prepared);
        assert_eq!(record["abort"]["status"], "complete");
        assert_eq!(record["abort"]["object"], prepared);

        write_test_change(&ws.path, &["retain-pushed-preparation"])?;
        ws.commit("feat: prepare a replacement release")?;
        let replacement = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--skip-tag",
                "--prepare",
                "--retain-remote",
                "--yes",
            ],
        )?;
        assert!(
            replacement.status.success(),
            "replacement stderr:\n{}",
            String::from_utf8_lossy(&replacement.stderr)
        );
        assert!(std::fs::read_to_string(ws.path.join("Cargo.toml"))?.contains("version = \"0.1.2\""));

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that release apply requires explicit confirmation in non-interactive mode
#[test]
fn test_release_requires_explicit_confirmation_non_interactive() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-confirmation-gate")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn gate() {}")?;
        ws.commit("feat: add release-gated change")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "lib-a", "--bump", "patch"])?;
        assert!(
            !output.status.success(),
            "release should fail without --yes/--plan in non-interactive mode"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("explicit confirmation") && stderr.contains("--yes") && stderr.contains("--plan"),
            "safety gate message missing expected guidance.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_confirmation_pty_accepts_only_yes_and_keeps_the_prompt_on_stderr() {
    fn fixture(name: &str) -> Result<TestWorkspace> {
        let ws = TestWorkspace::new_named(name)?;
        write_release_config(&ws, "")?;
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn pty_confirmation() {}")?;
        ws.commit("feat: add PTY confirmation fixture")?;
        write_test_change(&ws.path, &["lib-a"])?;
        Ok(ws)
    }

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
        let mut child = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
            .args(["rail", "release", "run", "lib-a", "--bump", "patch", "--skip-tag"])
            .current_dir(&ws.path)
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
            let accepted = fixture(&format!("release-confirmation-pty-{name}"))?;
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
            let rejected = fixture(&format!("release-confirmation-pty-{name}"))?;
            let head = git(&rejected.path, &["rev-parse", "HEAD"])?.stdout;
            let (stdout, stderr) = run_in_pty(&rejected, answer)?;
            assert!(
                stderr.contains("Proceed? [y/N] "),
                "prompt missing from stderr for {name}: {stderr}"
            );
            assert!(
                !stdout.contains("Proceed? [y/N] "),
                "prompt leaked to stdout for {name}: {stdout}"
            );
            assert_eq!(
                git(&rejected.path, &["rev-parse", "HEAD"])?.stdout,
                head,
                "rejected answer {name} changed HEAD"
            );
            assert!(
                std::fs::read_to_string(rejected.path.join("crates/lib-a/Cargo.toml"))?.contains("version = \"0.1.0\""),
                "rejected answer {name} mutated the manifest"
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
        let commit_sha = ws.commit("Add lib-a")?;

        // Checkout detached HEAD
        crate::helpers::git(&ws.path, &["checkout", &commit_sha])?;

        // Run release (should fail with detached HEAD error)
        let output = run_cargo_rail(&ws.path, &["rail", "release", "run", "lib-a", "--bump", "patch"]);

        // Should fail (non-zero exit)
        let output = output?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "Release from detached HEAD should fail");
        assert!(
            stderr.contains("detached HEAD") || stderr.contains("Detached HEAD"),
            "Error should mention detached HEAD.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that confirmation does not authorize a non-default branch.
#[test]
fn test_release_non_default_branch_fails_without_branch_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-branch")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        // Create and switch to a feature branch
        crate::helpers::git(&ws.path, &["checkout", "-b", "feature-branch"])?;

        // --yes skips only the prompt; it does not authorize this branch.
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "lib-a", "--bump", "patch", "--yes"],
        )?;

        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "Release from non-default branch should fail without branch authority.\nstderr:\n{}",
            stderr
        );
        assert!(
            stderr.contains("feature-branch") && stderr.contains("--allow-non-default-branch"),
            "Error should name the branch authority flag.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that prompt and branch authority are independently explicit.
#[test]
fn test_release_non_default_branch_requires_confirmation_and_branch_flags() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-branch-yes")?;
        write_release_config(&ws, "")?;

        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.commit("Add lib-a")?;
        ws.tag("lib-a-v0.1.0", "Initial release")?;

        // Create and switch to a feature branch
        crate::helpers::git(&ws.path, &["checkout", "-b", "hotfix-1.0"])?;

        // Make a change for the release
        ws.modify_file("lib-a", "src/lib.rs", "pub fn hotfix() {}")?;
        ws.commit("feat: add hotfix function")?;
        write_test_change(&ws.path, &["lib-a"])?;

        // Run release with --yes (should succeed)
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "lib-a",
                "--bump",
                "patch",
                "--yes",
                "--allow-non-default-branch",
            ],
        )?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "Release with --yes should succeed.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        // Should show warning about non-default branch
        assert!(
            stderr.contains("warning") && stderr.contains("hotfix-1.0"),
            "Should warn about non-default branch.\nstderr:\n{}",
            stderr
        );

        let branch = git(&ws.path, &["branch", "--show-current"])?;
        assert!(
            String::from_utf8_lossy(&branch.stdout).trim() == "hotfix-1.0",
            "release should remain on the explicitly accepted branch"
        );
        let tag = git(&ws.path, &["rev-list", "-n", "1", "lib-a-v0.1.1"])?;
        let head = git(&ws.path, &["rev-parse", "HEAD"])?;
        assert_eq!(
            String::from_utf8_lossy(&tag.stdout).trim(),
            String::from_utf8_lossy(&head.stdout).trim(),
            "local-only release tag should target the release commit"
        );
        assert!(
            !stderr.contains("git push origin"),
            "local-only completion must not suggest a push outside the journaled release protocol.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
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

fn tag_release(ws: &TestWorkspace, crate_name: &str, version: &str) -> Result<()> {
    ws.tag(
        &format!("{}-v{}", crate_name, version),
        &format!("Release {} {}", crate_name, version),
    )
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

#[test]
fn release_check_uses_the_same_local_plan_for_an_unpublishable_workspace() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-local-unpublishable")?;
        write_release_config(&ws, "remote_effects = \"none\"")?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        ws.commit("feat: add internal release")?;
        write_test_change(&ws.path, &["internal"])?;

        let check = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--format", "json"])?;
        let run = run_cargo_rail(&ws.path, &["rail", "release", "check", "--all", "--format", "json"])?;
        assert_eq!(check.status.code(), Some(1));
        assert_eq!(run.status.code(), Some(1));

        let check: serde_json::Value = serde_json::from_slice(&check.stdout)?;
        let run: serde_json::Value = serde_json::from_slice(&run.stdout)?;
        assert_eq!(check["release_plan"], run["release_plan"]);
        assert_eq!(check["mutation_plan"], run["mutation_plan"]);
        assert_eq!(check["release_plan"]["summary"]["crates_to_publish"], 0);
        assert_eq!(check["release_plan"]["summary"]["crates_to_tag"], 1);
        assert_eq!(check["readiness"]["scope"], "local");
        assert_eq!(check["readiness"]["effects_executed"], serde_json::json!([]));
        assert_eq!(check["readiness"]["planned_effects"]["git_tag"], true);
        assert_eq!(check["readiness"]["planned_effects"]["registry_publication"], false);
        assert_eq!(
            check["readiness"]["effects_excluded_from_check"],
            serde_json::json!([
                "workspace_mutation",
                "git_commit",
                "git_tag",
                "git_push",
                "forge_release",
                "registry_publication"
            ])
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn release_publication_check_preserves_unpublishable_release_plan_authority() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-check-publication-unpublishable")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;
        add_unpublishable_crate(&ws, "internal", "0.1.0")?;
        ws.commit("feat: add internal release")?;
        write_test_change(&ws.path, &["internal"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains("internal: not publishable"));

        let json = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--all", "--format", "json"],
        )?;
        assert_eq!(json.status.code(), Some(1));
        let json: serde_json::Value = serde_json::from_slice(&json.stdout)?;
        assert_eq!(json["result"], "pending_changes");
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["readiness"]["scope"], "publication");
        assert_eq!(json["readiness"]["effects_executed"], serde_json::json!([]));
        assert_eq!(json["release_plan"]["summary"]["total_crates"], 1);
        assert_eq!(json["release_plan"]["summary"]["crates_to_publish"], 0);
        assert_eq!(json["count"], 0);
        assert_eq!(json["skipped"][0]["crate"], "internal");
        assert_eq!(json["readiness"]["planned_effects"]["registry_publication"], false);
        assert!(
            json["mutation_plan"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["code"] != "PUBLISH_CRATE")
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that --all skips crates with publish = false in Cargo.toml
#[test]
fn test_release_check_all_skips_unpublishable_cargo_toml() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("check-skip-unpub")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;

        // Add a publishable crate
        ws.add_crate("lib-pub", "0.1.0", &[])?;

        // Add an unpublishable crate (publish = false in Cargo.toml)
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;

        ws.commit("Add crates")?;
        write_test_change(&ws.path, &["lib-pub", "lib-internal"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");

        // Should show lib-pub as ready
        assert!(
            stdout.contains("lib-pub: ready"),
            "Should report lib-pub as ready.\nstdout:\n{}",
            stdout
        );

        // The exact release plan retains lib-internal but excludes its publish action.
        assert!(
            stdout.contains("lib-internal: not publishable"),
            "Should report lib-internal as not publishable.\nstdout:\n{}",
            stdout
        );

        // Should mention publish = false
        assert!(
            stdout.contains("no release-worthy changes") || stdout.contains("publish = false"),
            "Should explain why crate was skipped.\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that path-only deps are allowed for crates with publish = false
#[test]
fn test_release_check_path_deps_allowed_for_unpublishable() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("path-dep-unpub")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;

        // Add a publishable crate
        ws.add_crate("lib-core", "0.1.0", &[])?;

        // Add an unpublishable crate with a path-only dep
        add_crate_with_path_dep(&ws, "wasm-bindings", "0.1.0", "lib-core", false)?;

        ws.commit("Add crates")?;
        write_test_change(&ws.path, &["lib-core", "wasm-bindings"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");

        // Should NOT contain the path-only dependency error
        assert!(
            !stderr.contains("path-only dependency"),
            "Should not complain about path-only deps for unpublishable crates.\nstderr:\n{}",
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that explicitly naming an unpublishable crate reports its status
#[test]
fn test_release_check_explicit_unpublishable_crate() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("explicit-unpub")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;

        // Add an unpublishable crate
        add_unpublishable_crate(&ws, "internal-tool", "0.1.0")?;
        ws.commit("Add crates")?;
        write_test_change(&ws.path, &["internal-tool"])?;

        // Run release check on the specific crate
        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "internal-tool"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(1), "stdout:\n{stdout}");

        assert!(
            stdout.contains("not publishable") || stdout.contains("publish = false"),
            "Should report crate as not publishable.\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test JSON output includes skipped crates
#[test]
fn test_release_check_json_includes_skipped() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("json-skipped")?;
        write_publication_release_config(&ws, r#"validation = { ".github/workflows/ci.yml" = ["tests"] }"#)?;

        // Add publishable and unpublishable crates
        ws.add_crate("lib-pub", "0.1.0", &[])?;
        add_unpublishable_crate(&ws, "lib-internal", "0.1.0")?;
        ws.commit("Add crates")?;
        write_test_change(&ws.path, &["lib-pub", "lib-internal"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "check", "--publication", "--all", "--json"],
        )?;
        assert_eq!(output.status.code(), Some(1));
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON
        let json: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("Should be valid JSON.\nstdout:\n{}", stdout));

        // Should have skipped array
        assert!(
            json.get("skipped").is_some(),
            "JSON should contain 'skipped' field.\nJSON:\n{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );

        // Skipped should contain lib-internal
        let skipped = json["skipped"].as_array().expect("skipped should be array");
        let has_internal = skipped.iter().any(|s| {
            s.get("crate")
                .and_then(|c| c.as_str())
                .map(|c| c == "lib-internal")
                .unwrap_or(false)
        });

        assert!(
            has_internal,
            "Skipped should include lib-internal.\nJSON:\n{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

/// Test that rail.toml publish = false is respected
#[test]
fn test_release_check_respects_rail_toml_publish_false() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("rail-toml-unpub")?;

        // Add crates (both publishable in Cargo.toml)
        ws.add_crate("lib-a", "0.1.0", &[])?;
        ws.add_crate("lib-b", "0.1.0", &[])?;
        ws.commit("Add crates")?;

        // Configure lib-b as non-publishable in rail.toml
        ws.write_release_config(
            r#"remote_effects = "push"
registry_publication = "crates-io"

[crates.lib-b.release]
publish = false
"#,
        )?;
        write_test_change(&ws.path, &["lib-a", "lib-b"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "--publication", "--all"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");

        // Should show lib-a as ready
        assert!(
            stdout.contains("lib-a: ready"),
            "lib-a should be ready.\nstdout:\n{}",
            stdout
        );

        // Should report lib-b as skipped due to rail.toml
        assert!(
            stdout.contains("lib-b"),
            "lib-b should be skipped by the exact release plan.\nstdout:\n{}",
            stdout
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_rejects_partial_dependent_closure() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-partial-closure")?;
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
        ws.commit("Add release closure crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);

        assert!(
            !output.status.success(),
            "partial subset release should be rejected.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            combined.contains("partial release would leave dependent crate(s) out of sync"),
            "expected partial closure error.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            combined.contains("lib-b") && combined.contains("lib-c"),
            "expected missing dependent closure in error output.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            combined.contains("--include-dependents"),
            "expected opt-in guidance for dependent closure.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_include_dependents_expands_full_closure() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-include-dependents")?;
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
        ws.commit("Add release closure crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(&ws.path, &["rail", "release", "check", "lib-a", "--include-dependents"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.code(),
            Some(1),
            "check mode should exit with pending changes.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            stdout.contains("lib-a") && stdout.contains("lib-b") && stdout.contains("lib-c"),
            "expected full dependent closure in plan output.\nstdout:\n{}",
            stdout
        );
        let lib_a_idx = stdout.find("1. lib-a").expect("expected lib-a first in release plan");
        let lib_b_idx = stdout.find("2. lib-b").expect("expected lib-b second in release plan");
        let lib_c_idx = stdout.find("3. lib-c").expect("expected lib-c third in release plan");
        assert!(
            lib_a_idx < lib_b_idx && lib_b_idx < lib_c_idx,
            "dependent closure should be released in dependency order.\nstdout:\n{}",
            stdout
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
        ws.commit("Add release subset crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        tag_release(&ws, "lib-c", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let preview = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "check",
                "lib-a",
                "--include-dependents",
                "--bump",
                "patch",
                "--format",
                "json",
            ],
        )?;
        assert_eq!(preview.status.code(), Some(1));
        let preview: serde_json::Value = serde_json::from_slice(&preview.stdout)?;
        let actions = preview["mutation_plan"]["actions"].as_array().unwrap();
        for code in ["COMMIT_RELEASE", "UPDATE_LOCKFILE"] {
            assert_eq!(
                actions.iter().filter(|action| action["code"] == code).count(),
                1,
                "{code}"
            );
        }
        let commit = actions
            .iter()
            .position(|action| action["code"] == "COMMIT_RELEASE")
            .unwrap();
        for code in [
            "BUMP_VERSION",
            "UPDATE_CHANGELOG",
            "DELETE_CHANGE_FILE",
            "UPDATE_LOCKFILE",
        ] {
            assert!(
                actions
                    .iter()
                    .enumerate()
                    .filter(|(_, action)| action["code"] == code)
                    .all(|(index, _)| index < commit)
            );
        }
        let initial_head = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_owned();
        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "lib-a",
                "--include-dependents",
                "--bump",
                "patch",
                "--yes",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "subset release should succeed.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );

        let head = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_owned();
        let count = git(&ws.path, &["rev-list", "--count", &format!("{initial_head}..HEAD")])?;
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
        for tag in ["lib-a-v0.1.1", "lib-b-v0.1.1"] {
            let target = git(&ws.path, &["rev-parse", &format!("{tag}^{{commit}}")])?;
            assert_eq!(String::from_utf8_lossy(&target.stdout).trim(), head);
        }
        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(
            state["preparation"],
            serde_json::json!({"status":"complete", "commit":head})
        );
        for package in state["crates"].as_array().unwrap() {
            assert!(package.get("commit").is_none());
        }
        std::fs::remove_file(state_path)?;
        let status = run_cargo_rail(
            &ws.path,
            &["rail", "release", "status", "--history", "--format", "json"],
        )?;
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(status["transactions"][0]["exact_sha"], head);
        assert_eq!(status["transactions"][0]["recoverability"], "missing_record");
        assert_eq!(status["transactions"][0]["state"], "record_unavailable");
        assert_eq!(status["transactions"][0]["ambiguity"], false);

        let lib_a_manifest = std::fs::read_to_string(ws.path.join("crates/lib-a/Cargo.toml"))?;
        let lib_b_manifest = std::fs::read_to_string(ws.path.join("crates/lib-b/Cargo.toml"))?;
        let lib_c_manifest = std::fs::read_to_string(ws.path.join("crates/lib-c/Cargo.toml"))?;
        assert!(lib_a_manifest.contains("version = \"0.1.1\""));
        assert!(lib_b_manifest.contains("version = \"0.1.1\""));
        assert!(lib_b_manifest.contains("^0.1.1"));
        assert!(lib_c_manifest.contains("version = \"0.1.0\""));

        let tags = String::from_utf8_lossy(&git(&ws.path, &["tag", "--list"])?.stdout).to_string();
        assert!(tags.contains("lib-a-v0.1.1"), "missing lib-a tag.\ntags:\n{}", tags);
        assert!(tags.contains("lib-b-v0.1.1"), "missing lib-b tag.\ntags:\n{}", tags);
        assert!(
            !tags.contains("lib-c-v0.1.1"),
            "unrelated crate should not be tagged.\ntags:\n{}",
            tags
        );

        assert!(ws.path.join("crates/lib-a/CHANGELOG.md").exists());
        assert!(ws.path.join("crates/lib-b/CHANGELOG.md").exists());
        assert!(
            !ws.path.join("crates/lib-c/CHANGELOG.md").exists(),
            "unrelated crate should not get a changelog"
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_release_run_apply_supports_publish_false_from_rail_toml() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_named("release-run-publish-false")?;
        ws.add_crate("internal-tool", "0.1.0", &[])?;
        ws.commit("Add internal-tool")?;
        ws.write_release_config(
            r#"tag_prefix = "v"
tag_format = "{crate}-v{version}"

[crates.internal-tool.release]
publish = false
"#,
        )?;
        tag_release(&ws, "internal-tool", "0.1.0")?;
        ws.modify_file("internal-tool", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: update internal-tool")?;
        write_test_change(&ws.path, &["internal-tool"])?;

        let output = run_cargo_rail(
            &ws.path,
            &["rail", "release", "run", "internal-tool", "--bump", "patch", "--yes"],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);

        assert!(
            output.status.success(),
            "publish = false release should succeed without crates.io publish.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            combined.contains("skipped publish (publish = false)"),
            "expected publish = false skip message.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(ws.path.join("crates/internal-tool/CHANGELOG.md").exists());
        let tags = String::from_utf8_lossy(&git(&ws.path, &["tag", "--list"])?.stdout).to_string();
        assert!(tags.contains("v0.1.1"));

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
        ws.commit("Add workspace dependency crates")?;
        tag_release(&ws, "lib-a", "0.1.0")?;
        tag_release(&ws, "lib-b", "0.1.0")?;
        ws.modify_file("lib-a", "src/lib.rs", "pub fn changed() {}\n")?;
        ws.commit("feat: change lib-a")?;
        write_test_change(&ws.path, &["lib-a"])?;

        let output = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "run",
                "lib-a",
                "--include-dependents",
                "--bump",
                "patch",
                "--yes",
            ],
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "subset release with workspace dependencies should succeed.\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
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
            "dependent crate should be version bumped as part of the approved closure.\nCargo.toml:\n{}",
            lib_b_manifest
        );
        assert!(
            lib_b_manifest.contains("lib-a = { workspace = true }"),
            "workspace dependency declaration should remain workspace-based.\nCargo.toml:\n{}",
            lib_b_manifest
        );

        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_cannot_authorize_itself_or_replace_a_verified_workflow_attempt() {
    let result: Result<()> = (|| {
        for own_run in [true, false] {
            let ws = TestWorkspace::new_single_crate("workflow-authority", "0.1.0")?;
            ws.write_release_config(
                r#"remote_effects = "push"
semver_check = "off"
sign_tags = false
validation = { ".github/workflows/ci.yml" = ["tests"] }
"#,
            )?;
            ws.set_remote("https://github.com/example/workflow-authority.git")?;
            write_test_change(&ws.path, &["workflow-authority"])?;
            ws.commit("Review workflow-bound release")?;
            let boundary = tempfile::tempdir()?;
            let shim =
                publication_boundary_shim(&boundary.path().join("cargo.log"), &boundary.path().join("published"))?;
            let git_proxy = shim.path().join("git");
            let original_git_proxy = std::fs::read_to_string(&git_proxy)?;
            if !own_run {
                std::fs::write(&git_proxy, original_git_proxy.replacen("#!/bin/sh", "#!/bin/sh\ncase \" $* \" in *\" tag -a \"*) echo 'fixture tag creation interrupted' >&2; exit 1 ;; esac", 1))?;
            }
            let search = format!(
                "{}:{}",
                shim.path().display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let interrupted = cargo_rail_command(&ws.path)?
                .env("PATH", &search)
                .env("GITHUB_RUN_ID", if own_run { "42" } else { "99" })
                .args(["rail", "release", "run", "--all", "--bump", "patch", "--yes"])
                .output()?;
            assert!(!interrupted.status.success());
            let state_path = only_release_state(&ws.path)?;
            let retained = std::fs::read(&state_path)?;
            let state: serde_json::Value = serde_json::from_slice(&retained)?;
            let tags = git(&ws.path, &["tag", "--list", "v0.1.1"])?;
            assert!(tags.stdout.is_empty(), "no release tag may precede required validation");
            if own_run {
                assert!(String::from_utf8_lossy(&interrupted.stderr).contains("cannot authorize itself"));
                assert_eq!(state["validation"], serde_json::json!([]));
                assert_ne!(state["readiness"]["status"], "complete");
            } else {
                assert!(String::from_utf8_lossy(&interrupted.stderr).contains("fixture tag creation interrupted"));
                assert_eq!(
                    state["validation"],
                    serde_json::json!([{
                        "workflow":".github/workflows/ci.yml", "workflow_id":7, "run_id":42, "attempt":3,
                        "jobs":[{"name":"tests","id":17}]
                    }])
                );
                let gh = shim.path().join("gh");
                let response = std::fs::read_to_string(&gh)?;
                let changed = response.replace("\\\"run_attempt\\\":3", "\\\"run_attempt\\\":4");
                assert_ne!(response, changed);
                std::fs::write(&gh, changed)?;
                std::fs::write(&git_proxy, original_git_proxy)?;
                let rejected = cargo_rail_command(&ws.path)?
                    .env("PATH", &search)
                    .env("GITHUB_RUN_ID", "99")
                    .args(["rail", "release", "resume"])
                    .output()?;
                assert!(!rejected.status.success());
                assert!(String::from_utf8_lossy(&rejected.stderr).contains("validation attempt changed"));
                assert_eq!(
                    std::fs::read(state_path)?,
                    retained,
                    "a rerun must not replace sealed validation"
                );
                assert!(git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout.is_empty());
            }
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn remote_release_record_rejects_a_second_upload_attempt_identity() {
    let result: Result<()> = (|| {
        let ws = TestWorkspace::new_single_crate("upload-lease", "0.1.0")?;
        ws.write_release_config(
            "remote_effects = 'gitlab'\nregistry_publication = 'crates-io'\nsemver_check = 'off'\nsign_tags = false\n",
        )?;
        write_test_change(&ws.path, &["upload-lease"])?;
        ws.commit("Review the publication request")?;
        let remote = tempfile::tempdir()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;
        let prepared = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--publish",
                "--skip-tag",
                "--prepare",
                "--retain-remote",
                "--yes",
                "--format",
                "json",
            ],
        )?;
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stdout)
        );
        let summary: serde_json::Value = serde_json::from_slice(&prepared.stdout)?;
        let transaction = summary["transaction_id"].as_str().unwrap();
        let source = summary["source"].as_str().unwrap();
        git(&ws.path, &["push", "origin", "main"])?;
        let other = tempfile::tempdir()?;
        let clone = other.path().join("clone");
        git(
            other.path(),
            &["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()],
        )?;
        git(&clone, &["config", "user.name", "Second executor"])?;
        git(&clone, &["config", "user.email", "second@example.invalid"])?;
        let fetched = run_cargo_rail(&clone, &["rail", "release", "record", "fetch", transaction])?;
        assert!(fetched.status.success(), "{}", String::from_utf8_lossy(&fetched.stdout));
        let first_path = only_release_state(&ws.path)?;
        let second_path = only_release_state(&clone)?;
        let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&first_path)?)?;
        let archive_bytes = b"sealed upload lease fixture";
        let checksum = rscrypto::Sha256::digest(archive_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let archive_directory = first_path.with_extension("artifacts");
        std::fs::create_dir_all(archive_directory.join("attempts"))?;
        std::fs::write(archive_directory.join("upload-lease-0.1.1.crate"), archive_bytes)?;
        record["package_seal"] = serde_json::json!({
            "source_commit":source, "cargo_version":"cargo fixture", "registry_index":"https://github.com/rust-lang/crates.io-index",
            "packages":[{"name":"upload-lease","version":"0.1.1","bytes":archive_bytes.len(),"sha256":checksum}]
        });
        record["phase"] = serde_json::json!("publishing");
        record["commit_push"] = serde_json::json!({"status":"complete","object":source});
        record["readiness"] = serde_json::json!({"status":"complete","object":"gitlab:success"});
        record["crates"][0]["publication"] = serde_json::json!({"status":"in_progress","object":checksum});
        record["crates"][0]["publication_attempt"] = serde_json::json!("1".repeat(64));
        std::fs::write(&first_path, serde_json::to_vec(&record)?)?;
        let claimed = run_cargo_rail(&ws.path, &["rail", "release", "record", "store", transaction])?;
        assert!(claimed.status.success(), "{}", String::from_utf8_lossy(&claimed.stdout));
        let first = git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout;
        record["crates"][0]["publication_attempt"] = serde_json::json!("2".repeat(64));
        std::fs::write(second_path, serde_json::to_vec(&record)?)?;
        let rejected = run_cargo_rail(&clone, &["rail", "release", "record", "store", transaction])?;
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stdout).contains("progress conflicts"));
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            first
        );
        let retained = git(remote.path(), &["show", "refs/notes/cargo-rail/active:record.json"])?;
        let retained: serde_json::Value = serde_json::from_slice(&retained.stdout)?;
        assert_eq!(retained["crates"][0]["publication_attempt"], "1".repeat(64));
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_push_rejects_a_changed_captured_branch_tip() {
    let result: Result<()> = (|| {
        for during_push in [false, true] {
            let (ws, remote) = push_release_workspace("branch-lease")?;
            let prior = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?;
            ws.commit("Review the complete release request")?;
            git(&ws.path, &["push", "origin", "main"])?;
            let initial = String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?;
            let prepared = run_cargo_rail(
                &ws.path,
                &[
                    "rail",
                    "release",
                    "run",
                    "--all",
                    "--bump",
                    "patch",
                    "--skip-tag",
                    "--prepare",
                    "--yes",
                ],
            )?;
            assert!(
                prepared.status.success(),
                "{}",
                String::from_utf8_lossy(&prepared.stderr)
            );
            let prepared_head = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
            if during_push {
                install_pre_push_hook(
                    &ws,
                    r#"#!/bin/sh
git --git-dir="$FIXTURE_REMOTE" update-ref refs/heads/main "$FIXTURE_PRIOR" "$FIXTURE_INITIAL"
"#,
                )?;
            } else {
                git(
                    remote.path(),
                    &["update-ref", "refs/heads/main", prior.trim(), initial.trim()],
                )?;
            }
            let rejected = cargo_rail_command(&ws.path)?
                .env("FIXTURE_REMOTE", remote.path())
                .env("FIXTURE_PRIOR", prior.trim())
                .env("FIXTURE_INITIAL", initial.trim())
                .args(["rail", "release", "resume"])
                .output()?;
            assert!(
                !rejected.status.success(),
                "a changed remote tip cannot authorize a release push"
            );
            assert_eq!(
                git(remote.path(), &["rev-parse", "refs/heads/main"])?.stdout,
                prior.as_bytes()
            );
            assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, prepared_head);
            let state: serde_json::Value = serde_json::from_slice(&std::fs::read(only_release_state(&ws.path)?)?)?;
            assert_eq!(state["status"], "active");
            assert_ne!(state["commit_push"]["status"], "complete");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn signed_release_tag_recovers_original_object_without_the_private_key() {
    let result: Result<()> = (|| {
        let (ws, remote) = push_release_workspace("signed-handoff")?;
        ws.write_release_config(
            "tag_format = 'v{version}'\nremote_effects = 'gitlab'\nsemver_check = 'off'\nsign_tags = true\n",
        )?;
        ws.commit("Review the signed release")?;
        git(&ws.path, &["push", "origin", "main"])?;
        let key = ws.path.join(".git/release-key");
        let generated = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .output()?;
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let public = std::fs::read_to_string(key.with_extension("pub"))?;
        let signers = ws.path.join(".git/allowed-signers");
        std::fs::write(&signers, format!("* {public}"))?;
        git(&ws.path, &["config", "gpg.format", "ssh"])?;
        git(&ws.path, &["config", "user.signingkey", key.to_str().unwrap()])?;
        git(
            &ws.path,
            &["config", "gpg.ssh.allowedSignersFile", signers.to_str().unwrap()],
        )?;
        install_pre_push_hook(
            &ws,
            r#"#!/bin/sh
while read local_ref local_oid remote_ref remote_oid; do
  case "$local_ref" in refs/tags/*) echo 'fixture tag push interrupted' >&2; exit 1 ;; esac
done
"#,
        )?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim(&logs.path().join("glab.log"))?;
        let interrupted = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
        )?;
        assert!(!interrupted.status.success());
        assert!(String::from_utf8_lossy(&interrupted.stderr).contains("fixture tag push interrupted"));
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(only_release_state(&ws.path)?)?)?;
        let tag = &record["crates"][0]["tag_object"];
        assert!(tag["content"].as_str().unwrap().contains("BEGIN SSH SIGNATURE"));
        let transaction = record["transaction_id"].as_str().unwrap();
        let intent = record["intent"]["identity"].as_str().unwrap();
        let source = record["preparation"]["commit"].as_str().unwrap();
        let transfer = logs.path().join("transfer");
        let exported = run_cargo_rail(
            &ws.path,
            &[
                "rail",
                "release",
                "record",
                "export",
                transaction,
                transfer.to_str().unwrap(),
            ],
        )?;
        assert!(
            exported.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&exported.stdout),
            String::from_utf8_lossy(&exported.stderr)
        );
        std::fs::remove_file(&key)?;
        let clone = logs.path().join("clone");
        git(
            logs.path(),
            &["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()],
        )?;
        git(&clone, &["config", "user.name", "Recovery executor"])?;
        git(&clone, &["config", "user.email", "recovery@example.invalid"])?;
        git(&clone, &["config", "gpg.format", "ssh"])?;
        let recovered_signers = clone.join(".git/allowed-signers");
        std::fs::write(&recovered_signers, format!("* {public}"))?;
        git(
            &clone,
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                recovered_signers.to_str().unwrap(),
            ],
        )?;
        let imported = run_cargo_rail(
            &clone,
            &[
                "rail",
                "release",
                "record",
                "import",
                transfer.to_str().unwrap(),
                "--intent",
                intent,
                "--source",
                source,
            ],
        )?;
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stdout)
        );
        let resumed = cargo_rail_command(&clone)?
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    glab.parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .args(["rail", "release", "resume"])
            .output()?;
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        let restored = git(&clone, &["cat-file", "tag", "refs/tags/v0.1.1"])?;
        assert_eq!(restored.stdout, tag["content"].as_str().unwrap().as_bytes());
        assert_eq!(
            String::from_utf8(git(remote.path(), &["rev-parse", "refs/tags/v0.1.1"])?.stdout)?.trim(),
            tag["id"].as_str().unwrap()
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_abort_preserves_a_replaced_local_tag() {
    let result: Result<()> = (|| {
        for replacement in ["unchanged", "other-commit", "other-object"] {
            let retained_object = replacement == "other-object";
            let ws = TestWorkspace::new_single_crate("abort-tag-owner", "0.1.0")?;
            ws.write_release_config("tag_format = 'v{version}'\nsemver_check = 'off'\nsign_tags = false\n")?;
            write_test_change(&ws.path, &["abort-tag-owner"])?;
            let initial = ws.commit("Review local release")?;
            let interrupted = run_with_lost_git_acknowledgment(
                &ws.path,
                &["rail", "release", "run", "--all", "--bump", "patch", "--yes"],
                "tag",
            )?;
            assert!(!interrupted.status.success());
            let prepared = git(&ws.path, &["rev-parse", "HEAD"])?.stdout;
            let state_path = only_release_state(&ws.path)?;
            if retained_object {
                let resumed = run_cargo_rail(&ws.path, &["rail", "release", "resume"])?;
                assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
                // Arrange the durable state immediately before the final completion write.
                let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
                assert!(record["crates"][0]["tag_object"].is_object());
                record["status"] = serde_json::json!("active");
                record["phase"] = serde_json::json!("publishing");
                std::fs::write(&state_path, serde_json::to_vec(&record)?)?;
            }
            if replacement != "unchanged" {
                let replacement_target = if retained_object { "HEAD" } else { initial.as_str() };
                git(
                    &ws.path,
                    &[
                        "tag",
                        "-f",
                        "-a",
                        "v0.1.1",
                        replacement_target,
                        "-m",
                        "Independent tag replacement",
                    ],
                )?;
            }
            let replacement_object = git(&ws.path, &["rev-parse", "refs/tags/v0.1.1"])?.stdout;
            let rejected = run_cargo_rail(&ws.path, &["rail", "release", "abort", "--yes"])?;
            if replacement == "unchanged" {
                assert!(rejected.status.success(), "{rejected:?}");
                assert_eq!(
                    String::from_utf8(git(&ws.path, &["rev-parse", "HEAD"])?.stdout)?.trim(),
                    initial
                );
                assert!(git(&ws.path, &["tag", "--list", "v0.1.1"])?.stdout.is_empty());
                let record: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
                assert_eq!(record["status"], "aborted");
                assert_eq!(record["abort"]["status"], "complete");
                continue;
            }
            assert!(!rejected.status.success());
            assert!(
                String::from_utf8_lossy(&rejected.stderr).contains("conflicting local tag object"),
                "{rejected:?}"
            );
            assert_eq!(
                git(&ws.path, &["rev-parse", "refs/tags/v0.1.1"])?.stdout,
                replacement_object
            );
            assert_eq!(git(&ws.path, &["rev-parse", "HEAD"])?.stdout, prepared);
            let record: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
            assert_eq!(record["status"], "active");
            assert_eq!(record["abort"]["status"], "in_progress");
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_native_assets_and_drafts_recover_lost_acknowledgments_without_repeating_effects() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt;
        let ws = TestWorkspace::new_single_crate("registry-shadow", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
semver_check = "off"
remote_effects = "github"
validation = { ".github/workflows/ci.yml" = ["tests"] }
[release.artifacts.registry-shadow]
workflow = ".github/workflows/ci.yml"
[release.artifacts.registry-shadow.files."app-{version}-x86_64-unknown-linux-gnu.zip"]
target = "x86_64-unknown-linux-gnu"
[release.artifacts.registry-shadow.files.LICENSE]
source = "LICENSE"
"#,
        )?;
        std::fs::write(ws.path.join("LICENSE"), "fixture license\n")?;
        ws.set_remote("https://github.com/loadingalias/registry-shadow.git")?;
        ws.commit("Configure complete native release inventory")?;
        ws.tag("v0.1.0", "Initial release")?;
        write_test_change(&ws.path, &["registry-shadow"])?;
        let boundary = tempfile::TempDir::new()?;
        let shim = publication_boundary_shim(&boundary.path().join("cargo.log"), &boundary.path().join("published"))?;
        std::fs::write(
            shim.path().join("remote-head"),
            git(&ws.path, &["rev-parse", "HEAD"])?.stdout,
        )?;
        let gh = shim.path().join("gh");
        let fixture = shim.path().join("forge.py");
        std::fs::write(
            &fixture,
            r#"import sys,json,pathlib,subprocess,hashlib,io,zipfile
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
mode=(root/'mode').read_text().strip()
sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
def emit(value): print(json.dumps(value))
def record_effect(effect):
 with (root/'effects').open('a') as f:f.write(effect+'\n')
if args[0] in ['--version','auth']:sys.exit(0)
if args[:2]==['release','view']:sys.exit(1)
if args[:2]==['release','upload']:
 release=json.loads((root/'release.json').read_text())
 asset=pathlib.Path(args[3]);data=asset.read_bytes()
 release['assets'].append({'id':len(release['assets'])+10,'name':asset.name,'size':len(data),'digest':'sha256:'+hashlib.sha256(data).hexdigest(),'state':'uploaded'})
 (root/'release.json').write_text(json.dumps(release));record_effect('upload '+asset.name)
 if mode=='lost-upload-ack':sys.exit(1)
 sys.exit(0)
endpoint=next((arg for arg in args if arg.startswith('repos/')),None)
run={'id':42,'run_attempt':3,'workflow_id':7,'head_sha':sha,'path':'.github/workflows/ci.yml','repository':{'full_name':'loadingalias/registry-shadow'},'head_repository':{'full_name':'loadingalias/registry-shadow'},'event':'workflow_dispatch','status':'completed','conclusion':'success','run_started_at':'2026-01-01T00:00:00Z','updated_at':'2026-01-01T01:00:00Z'}
if endpoint.endswith('/actions/workflows/ci.yml'):emit({'id':7,'path':'.github/workflows/ci.yml','state':'active'})
elif '/actions/workflows/7/runs?' in endpoint:emit({'total_count':1,'workflow_runs':[run]})
elif endpoint.endswith('/jobs?per_page=100'):emit({'total_count':1,'jobs':[{'id':17,'name':'tests','run_id':42,'head_sha':sha,'status':'completed','conclusion':'success'}]})
elif endpoint.endswith('/actions/runs/42') or endpoint.endswith('/attempts/3'):emit(run)
elif '/actions/' in endpoint:
 data=io.BytesIO()
 with zipfile.ZipFile(data,'w',compression=zipfile.ZIP_STORED) as archive:
  def add(name,body):
   info=zipfile.ZipInfo(name,date_time=(2026,1,1,0,0,0));archive.writestr(info,body)
  add('app-0.1.1-aarch64-unknown-linux-gnu.zip' if mode=='wrong-target' else 'app-0.1.1-x86_64-unknown-linux-gnu.zip',b'product package')
  if mode!='missing-file':add('../LICENSE' if mode=='unsafe-path' else 'LICENSE',b'wrong license' if mode=='wrong-license' else b'fixture license\n')
  if mode=='extra-file':add('extra',b'undeclared')
 raw=data.getvalue()
 metadata={'id':92 if mode=='replacement' else 91,'name':'release-registry-shadow-42-3','size_in_bytes':len(raw),'digest':'sha256:'+hashlib.sha256(raw).hexdigest(),'expired':mode=='expired','created_at':'2026-01-01T00:30:00Z','expires_at':'2099-01-01T00:00:00Z','workflow_run':{'id':42,'head_sha':'0'*40 if mode=='wrong-source' else sha,'repository_id':1,'head_repository_id':1}}
 if endpoint.endswith('/zip'):sys.stdout.buffer.write(bytes([raw[0]^1])+raw[1:] if mode=='tampered' else raw)
 elif '/artifacts?' in endpoint:emit({'total_count':1,'artifacts':[metadata]})
 else:emit(metadata)
elif '/releases' in endpoint:
 if '--method' in args:
  method=args[args.index('--method')+1];body=json.loads(pathlib.Path(args[args.index('--input')+1]).read_text())
  if method=='POST':body.update(id=81,assets=[]);record_effect('draft')
  else:
   release=json.loads((root/'release.json').read_text());release.update(body);body=release;record_effect('publish')
  (root/'release.json').write_text(json.dumps(body));emit(body)
  if mode=='lost-draft-ack' and method=='POST':sys.exit(1)
  if mode=='lost-publish-ack' and method=='PATCH':sys.exit(1)
 elif '/releases?per_page=100&page=' in endpoint:
  if mode=='listing-unavailable':
   print('HTTP/2.0 403 Forbidden\r\n\r\n{}');sys.exit(1)
  response=json.loads((root/'release.json').read_text()) if (root/'release.json').exists() else None
  page=int(endpoint.rsplit('=',1)[1])
  values=[{'tag_name':'v0.0.'+str(i)} for i in range(100)] if page==1 else ([response] if response else [])
  if mode=='duplicate-draft' and page==2:values.append(dict(response,id=82))
  print('HTTP/2.0 200 OK\r\n\r\n',end='');emit(values)
 elif (root/'release.json').exists():
  response=json.loads((root/'release.json').read_text())
  if mode=='missing-retained' or '/releases/tags/' in endpoint and response['draft']:
   print('HTTP/2.0 404 Not Found\r\n\r\n{}');sys.exit(1)
  if '/releases/tags/' not in endpoint and not endpoint.endswith('/releases/81'):raise RuntimeError(args)
  if mode=='wrong-notes':response['body']='Another release'
  if mode=='wrong-release-id':response['id']=82
  if mode=='wrong-asset-digest':response['assets'][0]['digest']='sha256:'+'f'*64
  if mode=='public-incomplete':response['draft']=False
  print('HTTP/2.0 200 OK\r\n\r\n',end='');emit(response)
 else:
  print('HTTP/2.0 404 Not Found\r\n\r\n{}');sys.exit(1)
else:raise RuntimeError(args)
"#,
        )?;
        std::fs::write(&gh, format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", fixture.display()))?;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))?;
        let path = format!(
            "{}:{}",
            shim.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let invoke = |arguments: &[&str]| -> Result<std::process::Output> {
            Ok(cargo_rail_command(&ws.path)?
                .env("PATH", &path)
                .args(arguments)
                .output()?)
        };
        std::fs::write(shim.path().join("mode"), "wrong-source")?;
        let first = invoke(&["rail", "release", "run", "--all", "--yes"])?;
        assert!(!first.status.success(), "{first:?}");
        assert!(
            String::from_utf8_lossy(&first.stderr).contains("wrong source"),
            "{first:?}"
        );
        let state_path = only_release_state(&ws.path)?;
        for (mode, diagnostic) in [
            ("expired", "expired"),
            ("wrong-target", "undeclared file"),
            ("missing-file", "missing or extra"),
            ("unsafe-path", "unsafe"),
            ("tampered", "digest and size"),
            ("extra-file", "missing or extra"),
            ("wrong-license", "committed source"),
        ] {
            std::fs::write(shim.path().join("mode"), mode)?;
            let rejected = invoke(&["rail", "release", "resume"])?;
            assert!(!rejected.status.success(), "{mode}: {rejected:?}");
            assert!(
                String::from_utf8_lossy(&rejected.stderr).contains(diagnostic),
                "{mode}: {rejected:?}"
            );
            let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
            assert_eq!(state["artifacts"], serde_json::json!([]));
            assert_eq!(state["crates"][0]["tag"]["status"], "pending");
            assert!(!shim.path().join("effects").exists());
        }
        std::fs::write(shim.path().join("mode"), "lost-draft-ack")?;
        let lost_draft = invoke(&["rail", "release", "resume"])?;
        assert!(!lost_draft.status.success(), "{lost_draft:?}");
        assert!(String::from_utf8_lossy(&lost_draft.stderr).contains("not acknowledged"));
        let unobserved: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(unobserved["crates"][0]["forge_draft"]["status"], "in_progress");
        assert!(unobserved["crates"][0]["forge_draft"]["object"].is_null());
        assert_eq!(std::fs::read_to_string(shim.path().join("effects"))?, "draft\n");
        for (mode, diagnostic) in [
            ("listing-unavailable", "observation is unavailable"),
            ("duplicate-draft", "multiple GitHub releases"),
        ] {
            std::fs::write(shim.path().join("mode"), mode)?;
            let blocked = invoke(&["rail", "release", "resume"])?;
            assert!(!blocked.status.success(), "{mode}: {blocked:?}");
            assert!(
                String::from_utf8_lossy(&blocked.stderr).contains(diagnostic),
                "{blocked:?}"
            );
            assert_eq!(std::fs::read_to_string(shim.path().join("effects"))?, "draft\n");
        }
        std::fs::write(shim.path().join("mode"), "lost-upload-ack")?;
        let interrupted = invoke(&["rail", "release", "resume"])?;
        assert!(!interrupted.status.success(), "{interrupted:?}");
        assert!(
            String::from_utf8_lossy(&interrupted.stderr).contains("not acknowledged"),
            "{interrupted:?}"
        );
        let sealed: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(sealed["artifacts"][0]["artifact_id"], 91);
        assert_eq!(sealed["artifacts"][0]["files"].as_array().unwrap().len(), 2);
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/release-record-v10.schema.json"))?;
        jsonschema::validator_for(&schema)?
            .validate(&sealed)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        for mode in [
            "missing-retained",
            "replacement",
            "expired",
            "wrong-notes",
            "wrong-release-id",
            "wrong-asset-digest",
            "public-incomplete",
        ] {
            std::fs::write(shim.path().join("mode"), mode)?;
            let blocked = invoke(&["rail", "release", "resume"])?;
            assert!(!blocked.status.success(), "{blocked:?}");
            assert_eq!(
                std::fs::read_to_string(shim.path().join("effects"))?,
                "draft\nupload LICENSE\n"
            );
        }
        std::fs::write(shim.path().join("mode"), "lost-publish-ack")?;
        let lost_publication = invoke(&["rail", "release", "resume"])?;
        assert!(!lost_publication.status.success(), "{lost_publication:?}");
        assert!(String::from_utf8_lossy(&lost_publication.stderr).contains("not acknowledged"));
        std::fs::write(shim.path().join("mode"), "ok")?;
        let resumed = invoke(&["rail", "release", "resume"])?;
        assert!(resumed.status.success(), "{resumed:?}");
        assert_eq!(
            std::fs::read_to_string(shim.path().join("effects"))?,
            "draft\nupload LICENSE\nupload app-0.1.1-x86_64-unknown-linux-gnu.zip\npublish\n"
        );
        let completed: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
        assert_eq!(completed["artifacts"], sealed["artifacts"]);
        assert_eq!(completed["status"], "complete");
        assert_eq!(completed["crates"][0]["forge_draft"]["object"], "81");
        assert_eq!(completed["crates"][0]["forge_publication"]["object"], "81");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_remote_record_recovers_original_cargo_bytes_after_the_preparing_runner_is_lost() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt;
        let ws = TestWorkspace::new_single_crate("retained-cargo-package", "0.1.0")?;
        ws.write_release_config(
            "remote_effects = 'gitlab'\nregistry_publication = 'crates-io'\nsemver_check = 'off'\nsign_tags = false\n",
        )?;
        write_test_change(&ws.path, &["retained-cargo-package"])?;
        let initial = ws.commit("Review the retained package release")?;
        let remote = tempfile::tempdir()?;
        git(remote.path(), &["init", "--bare", "--initial-branch=main"])?;
        ws.set_remote(remote.path().to_str().unwrap())?;
        git(&ws.path, &["push", "-u", "origin", "main"])?;
        let hook = remote.path().join("hooks/update");
        std::fs::write(&hook, "#!/bin/sh\n[ \"$1\" != refs/heads/main ]\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        let logs = tempfile::tempdir()?;
        let (_shim, glab) = glab_shim_with_status(&logs.path().join("glab.log"), "success")?;
        let interrupted = run_with_path_prefix(
            &ws,
            glab.parent().unwrap(),
            &[
                "rail",
                "release",
                "run",
                "--all",
                "--bump",
                "patch",
                "--publish",
                "--skip-tag",
                "--retain-remote",
                "--yes",
            ],
        )?;
        assert!(!interrupted.status.success(), "{interrupted:?}");
        assert!(
            String::from_utf8_lossy(&interrupted.stderr).contains("hook declined"),
            "{interrupted:?}"
        );
        let state_path = only_release_state(&ws.path)?;
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        let transaction = state["transaction_id"].as_str().unwrap();
        let package = &state["package_seal"]["packages"][0];
        let filename = "retained-cargo-package-0.1.1.crate";
        let original = std::fs::read(state_path.with_extension("artifacts").join(filename))?;
        assert_eq!(package["bytes"], original.len());
        assert_eq!(
            package["sha256"],
            rscrypto::Sha256::digest(&original)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert_eq!(state["crates"][0]["publication"]["status"], "pending");
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/heads/main"])?.stdout,
            format!("{initial}\n").as_bytes()
        );
        assert_eq!(
            git(
                remote.path(),
                &["show", &format!("refs/notes/cargo-rail/active:packages/{filename}")]
            )?
            .stdout,
            original
        );
        std::fs::remove_dir_all(&ws.path)?;
        let recovery = tempfile::tempdir()?;
        let clone = recovery.path().join("clone");
        git(
            recovery.path(),
            &["clone", remote.path().to_str().unwrap(), clone.to_str().unwrap()],
        )?;
        let before = git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout;
        let fetched = run_cargo_rail(&clone, &["rail", "release", "record", "fetch", transaction])?;
        assert!(fetched.status.success(), "{fetched:?}");
        let recovered = only_release_state(&clone)?;
        assert_eq!(
            std::fs::read(recovered.with_extension("artifacts").join(filename))?,
            original
        );
        let recovered: serde_json::Value = serde_json::from_slice(&std::fs::read(recovered)?)?;
        assert_eq!(recovered, state);
        assert_eq!(
            git(remote.path(), &["rev-parse", "refs/notes/cargo-rail/active"])?.stdout,
            before
        );
        // A local archive with different bytes cannot be overwritten by record recovery.
        let archive = only_release_state(&clone)?.with_extension("artifacts").join(filename);
        std::fs::write(&archive, "independent bytes")?;
        let rejected = run_cargo_rail(&clone, &["rail", "release", "record", "fetch", transaction])?;
        assert!(!rejected.status.success(), "{rejected:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stdout).contains("differs from sealed evidence"),
            "{rejected:?}"
        );
        assert_eq!(std::fs::read_to_string(archive)?, "independent bytes");
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[cfg(unix)]
#[test]
fn release_alias_requires_verified_publication_and_reconciles_a_lost_push_acknowledgment() {
    let result: Result<()> = (|| {
        use std::os::unix::fs::PermissionsExt;
        let ws = TestWorkspace::new_single_crate("alias-fixture", "0.1.0")?;
        ws.write_release_config(
            r#"tag_format = "v{version}"
semver_check = "off"
remote_effects = "github"
aliases = { alias-fixture = "v1" }
validation = { ".github/workflows/ci.yml" = ["tests"] }
"#,
        )?;
        write_test_change(&ws.path, &["alias-fixture"])?;
        generate_lockfile(&ws.path)?;
        ws.commit("Review immutable release and alias promotion")?;
        ws.tag("v1", "Existing action alias")?;
        let previous = String::from_utf8(git(&ws.path, &["rev-parse", "refs/tags/v1"])?.stdout)?
            .trim()
            .to_owned();
        let transport = tempfile::tempdir()?;
        let remote = transport.path().join("origin.git");
        git(
            transport.path(),
            &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()],
        )?;
        let ssh = transport.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\ncase \"$*\" in *git-receive-pack*) exec git-receive-pack '{}' ;; *git-upload-pack*) exec git-upload-pack '{}' ;; esac\nexit 1\n",
                remote.display(),
                remote.display()
            ),
        )?;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))?;
        ws.set_remote("git@github.com:org/repo.git")?;
        git(&ws.path, &["config", "core.sshCommand", ssh.to_str().unwrap()])?;
        git(&ws.path, &["push", "-u", "origin", "main", "refs/tags/v1"])?;
        let (shim, gh) = gh_shim(&transport.path().join("gh.log"))?;
        let api = shim.path().join("forge.py");
        std::fs::write(shim.path().join("remote"), remote.to_str().unwrap())?;
        std::fs::write(shim.path().join("mode"), "drift")?;
        std::fs::write(
            &api,
            r#"import json,pathlib,subprocess,sys
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
p=root/'release.json'
endpoint=next(arg for arg in args if arg.startswith('repos/'))
if '--method' in args:
 body=json.loads(pathlib.Path(args[args.index('--input')+1]).read_text())
 if args[args.index('--method')+1]=='POST':body.update(id=81,assets=[])
 else:
  previous=json.loads(p.read_text());previous.update(body);body=previous
  with (root/'publications').open('a') as output:output.write('publish\n')
  if (root/'mode').read_text()=='drift':
   sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
   subprocess.check_call(['git','--git-dir='+ (root/'remote').read_text(),'update-ref','refs/tags/v1',sha])
 p.write_text(json.dumps(body));print(json.dumps(body))
elif '/releases?per_page=100&page=' in endpoint:
 print('HTTP/2.0 200 OK\r\n\r\n',end='');print('['+p.read_text()+']' if p.exists() else '[]')
elif p.exists():
 if '/releases/tags/' in endpoint and json.loads(p.read_text())['draft']:
  print('HTTP/2.0 404 Not Found\r\n\r\n{}');sys.exit(1)
 if '/releases/tags/' not in endpoint and not endpoint.endswith('/releases/81'):raise RuntimeError(args)
 print('HTTP/2.0 200 OK\r\n\r\n',end='');print(p.read_text())
else:
 print('HTTP/2.0 404 Not Found\r\n\r\n{}');sys.exit(1)
"#,
        )?;
        let script = std::fs::read_to_string(&gh)?;
        std::fs::write(&gh,script.replace("if [ \"$1\" = \"--version\" ]; then",&format!("case \"$*\" in *repos/org/repo/releases*) exec python3 '{}' \"$@\" ;; esac\nif [ \"$1\" = \"release\" ]; then exit 1; fi\nif [ \"$1\" = \"--version\" ]; then",api.display())))?;
        let path = format!(
            "{}:{}",
            shim.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let invoke = |args: &[&str]| -> Result<std::process::Output> {
            Ok(cargo_rail_command(&ws.path)?.env("PATH", &path).args(args).output()?)
        };
        let interrupted = invoke(&["rail", "release", "run", "--all", "--yes"])?;
        assert!(
            !interrupted.status.success(),
            "conflicting alias was overwritten: {interrupted:?}"
        );
        assert!(
            String::from_utf8_lossy(&interrupted.stderr).contains("moved from its authorized prior object"),
            "{interrupted:?}"
        );
        let state_path = only_release_state(&ws.path)?;
        let partial: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
        assert_eq!(partial["intent"]["alias_previous"]["alias-fixture"], previous);
        assert_eq!(partial["crates"][0]["alias"]["status"], "pending");
        assert_eq!(partial["crates"][0]["forge_publication"]["status"], "complete");
        assert_eq!(
            String::from_utf8(git(&remote, &["rev-parse", "refs/tags/v1"])?.stdout)?.trim(),
            partial["preparation"]["commit"].as_str().unwrap()
        );
        git(&remote, &["update-ref", "refs/tags/v1", &previous])?;
        std::fs::write(shim.path().join("mode"), "ok")?;
        let real_git = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output()?.stdout)?
            .trim()
            .to_owned();
        let wrapper = shim.path().join("git");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ncase \" $* \" in *\" push \"*\":refs/tags/v1 \"*) '{}' \"$@\" || exit $?; echo 'fixture lost alias push acknowledgment' >&2; exit 1 ;; esac\nexec '{}' \"$@\"\n",
                real_git, real_git
            ),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
        let uncertain = invoke(&["rail", "release", "resume"])?;
        assert!(!uncertain.status.success(), "{uncertain:?}");
        assert!(
            String::from_utf8_lossy(&uncertain.stderr).contains("lost alias push acknowledgment"),
            "{uncertain:?}"
        );
        let alias = git(&remote, &["rev-parse", "refs/tags/v1"])?.stdout;
        assert_eq!(alias, git(&remote, &["rev-parse", "refs/tags/v0.1.1"])?.stdout);
        // Leave the rejecting wrapper installed: a correct resume observes the completed push.
        let resumed = invoke(&["rail", "release", "resume"])?;
        assert!(resumed.status.success(), "{resumed:?}");
        let complete: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path)?)?;
        assert_eq!(complete["intent"], partial["intent"]);
        assert_eq!(complete["status"], "complete");
        assert_eq!(complete["crates"][0]["alias"]["status"], "complete");
        assert_eq!(std::fs::read_to_string(shim.path().join("publications"))?, "publish\n");
        Ok(())
    })();
    super::helpers::finish_test(result);
}
