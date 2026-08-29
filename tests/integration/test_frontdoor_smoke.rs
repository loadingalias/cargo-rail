use crate::helpers::{TestWorkspace, cargo_rail_command, git, run_cargo_rail};
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
fn test_documented_frontdoor_commands_smoke() {
    let result: Result<()> = (|| {
        let ws = setup_frontdoor_workspace("frontdoor-smoke")?;
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let readme = std::fs::read_to_string(repo_root.join("README.md"))?;
        let justfile = std::fs::read_to_string(repo_root.join("justfile"))?;
        let cargo_script = std::fs::read_to_string(repo_root.join("scripts/cargo/run.sh"))?;
        assert!(
            justfile.contains("cargo build --workspace --all-targets --all-features --release --locked"),
            "just build-release should produce the complete release artifact set"
        );
        assert!(justfile.contains("@scripts/cargo/run.sh build"));
        assert!(justfile.contains("@scripts/cargo/run.sh test"));
        assert!(justfile.contains("@scripts/plan/read.py create -"));
        assert!(!justfile.contains("cargo run --quiet --locked --target-dir"));
        assert!(!justfile.contains("rail run"));
        assert!(cargo_script.contains("scripts/plan/read.py"));
        assert!(cargo_script.contains("run_cargo_work cargo.build build"));
        assert!(cargo_script.contains("run_cargo_work cargo.test nextest run"));
        assert!(cargo_script.contains("run_cargo_work cargo.doctest test --doc"));
        assert!(!cargo_script.contains(" rail run"));

        let cases: &[(&str, &[&str], &str)] = &[
            ("README plan", &["rail", "plan"], "cargo rail plan"),
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
    })();
    super::helpers::finish_test(result);
}

#[test]
fn direct_and_cargo_sentinel_invocations_share_one_cli_grammar() {
    let result: Result<()> = (|| {
        let ws = setup_frontdoor_workspace("frontdoor-normalized-argv")?;
        let direct = run_cargo_rail(&ws.path, &["plan", "--since", "HEAD", "--json"])?;
        let cargo = run_cargo_rail(&ws.path, &["rail", "plan", "--since", "HEAD", "--json"])?;
        assert!(direct.status.success(), "direct plan failed: {direct:?}");
        assert!(cargo.status.success(), "Cargo sentinel plan failed: {cargo:?}");
        assert_eq!(direct.stdout, cargo.stdout);
        assert_eq!(direct.stderr, cargo.stderr);

        let help = cargo_rail_command(&ws.path)?.arg("--help").output()?;
        assert!(help.status.success(), "direct help failed: {help:?}");
        let help = String::from_utf8(help.stdout)?;
        assert!(help.contains("Usage: cargo-rail"), "unexpected direct help:\n{help}");
        assert!(
            !help.contains("Usage: cargo rail"),
            "direct help exposed Cargo's shim:\n{help}"
        );

        let version = cargo_rail_command(&ws.path)?.arg("--version").output()?;
        assert!(version.status.success(), "direct version failed: {version:?}");
        assert!(
            String::from_utf8(version.stdout)?.starts_with("cargo-rail "),
            "direct version must identify the executable"
        );

        let misplaced = run_cargo_rail(&ws.path, &["release", "--yes", "status"])?;
        assert_eq!(
            misplaced.status.code(),
            Some(2),
            "parent-only grammar must reject child options"
        );
        assert!(
            String::from_utf8_lossy(&misplaced.stderr).contains("unexpected argument '--yes'"),
            "unexpected parser error: {}",
            String::from_utf8_lossy(&misplaced.stderr)
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn every_completion_shell_uses_the_normalized_root_and_nested_options() {
    let result: Result<()> = (|| {
        let ws = setup_frontdoor_workspace("frontdoor-completions")?;
        for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
            let direct = run_cargo_rail(&ws.path, &["completions", shell])?;
            let cargo = run_cargo_rail(&ws.path, &["rail", "completions", shell])?;
            assert!(direct.status.success(), "direct {shell} completions failed: {direct:?}");
            assert!(cargo.status.success(), "Cargo {shell} completions failed: {cargo:?}");
            assert_eq!(direct.stdout, cargo.stdout, "{shell} grammar diverged by front door");
            let completion = String::from_utf8(direct.stdout)?;
            assert!(completion.contains("cargo-rail"), "{shell} omitted the executable root");
            assert!(
                completion.contains("allow-non-default-branch"),
                "{shell} omitted a nested release option"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}
