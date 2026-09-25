//! Compiler-evidence reuse across Unify commands and retries through the production binary.
//!
//! Evidence is reusable while every build script in its view would stay fresh under Cargo's
//! rerun rules. A changed declared input invalidates only the views that ran that script, and
//! views that completed before another view failed survive for the retry.

use crate::helpers::{NestedWorkspace, TestWorkspace, cargo_command, cargo_rail_command, file_url, git};
use anyhow::{Context as _, Result, ensure};
use std::fs;
use std::path::Path;

/// `app` has a build script whose declared inputs decide whether its generated source uses
/// `helper`; source analysis cannot see that use.
/// `other` expands a local proc macro and never uses `helper`. Both leave `helper` as a
/// compiler-evidence candidate, so Unify acquires one view per member.
fn reuse_workspace() -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named("unify-evidence-reuse")?;
    let root = ws.path.join("Cargo.toml");
    let manifest = fs::read_to_string(&root)?.replace(
        "members = [\"crates/*\"]",
        "members = [\"crates/*\"]\nexclude = [\"vendor\"]",
    );
    fs::write(&root, manifest)?;
    write_package(&ws.path, "vendor/helper", "helper", "", "pub fn help() -> u8 { 1 }\n")?;
    write_package(
        &ws.path,
        "vendor/marker",
        "marker",
        "[lib]\nproc-macro = true\n",
        "use proc_macro::TokenStream;\n\n#[proc_macro_derive(Marker)]\npub fn marker(_: TokenStream) -> TokenStream {\n    TokenStream::new()\n}\n",
    )?;
    write_package(
        &ws.path,
        "crates/app",
        "app",
        "[dependencies]\nhelper = { path = \"../../vendor/helper\" }\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
    )?;
    fs::write(
        ws.path.join("crates/app/build.rs"),
        r#"fn main() {
    println!("cargo::rerun-if-changed=switch.txt");
    println!("cargo::rerun-if-env-changed=D3_APP_USE_HELPER");
    let switch = std::fs::read_to_string("switch.txt").unwrap_or_default();
    let uses_helper = std::env::var("D3_APP_USE_HELPER").as_deref() == Ok("1") || switch.trim() == "on";
    // Spelled in pieces so source analysis of this file cannot see the dependency's name.
    let dependency = ["hel", "per"].concat();
    let generated = if uses_helper { format!("pub fn value() -> u8 {{ {dependency}::help() }}\n") } else { String::new() };
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::write(std::path::Path::new(&out_dir).join("generated.rs"), generated).expect("generated source");
}
"#,
    )?;
    fs::write(ws.path.join("crates/app/switch.txt"), "off\n")?;
    write_package(
        &ws.path,
        "crates/other",
        "other",
        "[dependencies]\nhelper = { path = \"../../vendor/helper\" }\nmarker = { path = \"../../vendor/marker\" }\n",
        "#[derive(marker::Marker)]\npub struct Tagged;\n",
    )?;
    let lockfile = cargo_command(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    ensure!(
        lockfile.status.success(),
        "offline lockfile generation failed: {lockfile:?}"
    );
    ws.commit("Add a build script, a proc macro, and unused helper candidates")?;
    Ok(ws)
}

fn write_package(root: &Path, directory: &str, name: &str, sections: &str, source: &str) -> Result<()> {
    let package = root.join(directory);
    fs::create_dir_all(package.join("src"))?;
    fs::write(
        package.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{sections}"),
    )?;
    fs::write(package.join("src/lib.rs"), source)?;
    Ok(())
}

/// One Unify run's machine result and diagnostic counters.
struct Run {
    status: Option<i32>,
    value: serde_json::Value,
    counters: serde_json::Value,
    stderr: String,
}

impl Run {
    fn cargo_views(&self) -> u64 {
        self.counters["compiler_acquisition"]["cargo_views"]
            .as_u64()
            .unwrap_or(u64::MAX)
    }

    /// The `evidence_cache` entry for one member's unused `helper` candidate.
    fn helper_cache(&self, member: &str) -> Option<&serde_json::Value> {
        self.value["evidence_cache"]
            .as_array()?
            .iter()
            .find(|entry| entry["member"] == member && entry["dependency"] == "helper")
    }
}

fn unify(ws: &TestWorkspace, arguments: &[&str], environment: &[(&str, &str)]) -> Result<Run> {
    unify_at(&ws.path, arguments, environment)
}

fn unify_at(root: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Result<Run> {
    let diagnostics = tempfile::TempDir::new()?;
    let file = diagnostics.path().join("counters.json");
    let mut command = cargo_rail_command(root)?;
    command
        .args(["rail", "--diagnostics-file", file.to_str().context("non-UTF-8 path")?])
        .args(arguments)
        .args(["--format", "json"])
        .env_remove("D3_APP_USE_HELPER");
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("stdout must be one JSON value\nstderr:\n{stderr}"))?;
    let counters = serde_json::from_slice(&fs::read(&file)?)?;
    Ok(Run {
        status: output.status.code(),
        value,
        counters,
        stderr,
    })
}

#[test]
fn adjacent_unify_commands_reuse_evidence_with_build_scripts_and_proc_macros() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        let cold = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(cold.status, Some(1), "{}", cold.stderr);
        assert_eq!(cold.cargo_views(), 2, "one view per member on a cold cache");

        for arguments in [&["unify"][..], &["unify", "--check"][..], &["unify", "--explain"][..]] {
            let warm = unify(&ws, arguments, &[])?;
            assert_eq!(
                warm.cargo_views(),
                0,
                "{arguments:?} must reuse complete evidence\n{:#}",
                warm.value["evidence_cache"]
            );
            for member in ["app", "other"] {
                let cache = warm.helper_cache(member).context("helper stays an unused candidate")?;
                assert_eq!(cache["hits"], 1, "{cache:#}");
                assert_eq!(cache["misses"], 0, "{cache:#}");
            }
        }

        // An unverified rustc wrapper can read anything, so its runs never reuse evidence.
        // Reuse under Cargo-Rail's installed wrapper is proved in the cache test suite.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let wrapper = ws.path.join("target/passthrough-wrapper");
            fs::create_dir_all(wrapper.parent().context("wrapper directory")?)?;
            fs::write(&wrapper, "#!/bin/sh\nexec \"$@\"\n")?;
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
            let wrapper = wrapper.to_str().context("non-UTF-8 wrapper")?;
            unify(&ws, &["unify", "--check"], &[("RUSTC_WRAPPER", wrapper)])?;
            let wrapped = unify(&ws, &["unify"], &[("RUSTC_WRAPPER", wrapper)])?;
            assert_eq!(wrapped.cargo_views(), 2, "{}", wrapped.stderr);
            let cache = wrapped.helper_cache("app").context("helper unused in app")?;
            assert_eq!(
                cache["miss_reasons"],
                serde_json::json!(["rustc_wrapper_dynamic_executable_inputs_unavailable=1"]),
                "{cache:#}"
            );
        }
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn changed_build_script_inputs_invalidate_exactly_the_affected_view() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        let cold = unify(&ws, &["unify", "--check"], &[])?;
        assert!(cold.helper_cache("app").is_some(), "helper starts unused in app");

        // A declared environment variable changes app's build-script output.
        let switched = unify(&ws, &["unify", "--check"], &[("D3_APP_USE_HELPER", "1")])?;
        assert_eq!(switched.cargo_views(), 1, "only app's view reruns\n{}", switched.stderr);
        assert!(
            switched.helper_cache("app").is_none(),
            "app now uses helper: {:#}",
            switched.value["evidence_cache"]
        );
        let other = switched.helper_cache("other").context("other keeps its evidence")?;
        assert_eq!((other["hits"].as_u64(), other["misses"].as_u64()), (Some(1), Some(0)));

        // Both variants stay stored; restoring the variable reuses the first one.
        let restored = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(restored.cargo_views(), 0, "{:#}", restored.value["evidence_cache"]);
        assert!(restored.helper_cache("app").is_some());

        // A declared file changes app's build-script output.
        fs::write(ws.path.join("crates/app/switch.txt"), "on\n")?;
        let changed = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(changed.cargo_views(), 1, "{}", changed.stderr);
        assert!(changed.helper_cache("app").is_none(), "app now uses helper");
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn retry_after_a_failed_view_reuses_views_that_completed() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        // `zulu` compiles after a delay and then fails, so `app` and `other` complete first.
        write_package(
            &ws.path,
            "crates/zulu",
            "zulu",
            "[dependencies]\nhelper = { path = \"../../vendor/helper\" }\n",
            "pub fn broken( {\n",
        )?;
        fs::write(
            ws.path.join("crates/zulu/build.rs"),
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(3)); }\n",
        )?;
        let lockfile = cargo_command(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(lockfile.status.success(), "{lockfile:?}");
        ws.commit("Add a member that fails late")?;

        let failed = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(failed.status, Some(2), "{}", failed.stderr);
        assert_eq!(failed.value["failure_class"], "source", "{:#}", failed.value);

        fs::write(ws.path.join("crates/zulu/src/lib.rs"), "pub fn fixed() {}\n")?;
        ws.commit("Fix the failing member")?;
        let retry = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(retry.status, Some(1), "{}", retry.stderr);
        assert_eq!(
            retry.cargo_views(),
            1,
            "only the fixed view reruns\n{:#}",
            retry.value["evidence_cache"]
        );
        for member in ["app", "other"] {
            let cache = retry.helper_cache(member).context("completed view kept")?;
            assert_eq!(cache["hits"], 1, "{cache:#}");
        }
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

#[test]
fn a_secret_build_script_input_is_never_stored_and_says_why() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        let build_script = ws.path.join("crates/app/build.rs");
        let declared = fs::read_to_string(&build_script)?.replacen(
            "fn main() {\n",
            "fn main() {\n    println!(\"cargo::rerun-if-env-changed=D3_SERVICE_TOKEN\");\n",
            1,
        );
        fs::write(&build_script, declared)?;
        ws.commit("Declare a secret build-script input")?;
        let first = unify(&ws, &["unify", "--check"], &[("D3_SERVICE_TOKEN", "value")])?;
        let app = first.helper_cache("app").context("helper unused in app")?;
        assert_eq!(
            app["publication_bypasses"],
            serde_json::json!(["build_script_secret_environment=1"]),
            "{app:#}"
        );
        let second = unify(&ws, &["unify", "--check"], &[("D3_SERVICE_TOKEN", "value")])?;
        assert_eq!(second.cargo_views(), 1, "only app reacquires; other still reuses");
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// `consumer` calls a macro from `switch`, whose build script decides whether the macro
/// expands to a use of `helper` in the caller. No unit of `consumer` itself reads that input.
fn dependency_build_script_workspace() -> Result<TestWorkspace> {
    let ws = TestWorkspace::new_named("unify-dependency-build-script")?;
    let root = ws.path.join("Cargo.toml");
    let manifest = fs::read_to_string(&root)?.replace(
        "members = [\"crates/*\"]",
        "members = [\"crates/*\"]\nexclude = [\"vendor\"]",
    );
    fs::write(&root, manifest)?;
    write_package(&ws.path, "vendor/helper", "helper", "", "pub fn help() -> u8 { 1 }\n")?;
    write_package(
        &ws.path,
        "vendor/switch",
        "switch",
        "",
        "#[cfg(expand_to_dependency)]\n#[macro_export]\nmacro_rules! value {\n    () => {\n        ::helper::help()\n    };\n}\n\n\
         #[cfg(not(expand_to_dependency))]\n#[macro_export]\nmacro_rules! value {\n    () => {\n        0\n    };\n}\n",
    )?;
    fs::write(
        ws.path.join("vendor/switch/build.rs"),
        r#"fn main() {
    println!("cargo::rustc-check-cfg=cfg(expand_to_dependency)");
    println!("cargo::rerun-if-env-changed=D3_SWITCH_EXPANDS");
    if std::env::var("D3_SWITCH_EXPANDS").as_deref() == Ok("1") {
        println!("cargo::rustc-cfg=expand_to_dependency");
    }
}
"#,
    )?;
    write_package(
        &ws.path,
        "crates/consumer",
        "consumer",
        "[dependencies]\nhelper = { path = \"../../vendor/helper\" }\nswitch = { path = \"../../vendor/switch\" }\n",
        "pub fn value() -> u8 {\n    switch::value!()\n}\n",
    )?;
    let lockfile = cargo_command(&ws.path)
        .args(["generate-lockfile", "--offline"])
        .output()?;
    ensure!(
        lockfile.status.success(),
        "offline lockfile generation failed: {lockfile:?}"
    );
    ws.commit("Add a dependency build script that decides a consumer's dependency use")?;
    Ok(ws)
}

#[test]
fn a_dependency_build_script_input_invalidates_its_consumers() {
    let result: Result<()> = (|| {
        let ws = dependency_build_script_workspace()?;
        let unused = unify(&ws, &["unify", "--check"], &[])?;
        assert!(
            unused.helper_cache("consumer").is_some(),
            "the macro does not expand to helper: {:#}",
            unused.value["evidence_cache"]
        );
        let warm = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(warm.cargo_views(), 0, "unchanged inputs reuse the view");

        let used = unify(&ws, &["unify", "--check"], &[("D3_SWITCH_EXPANDS", "1")])?;
        assert_eq!(used.cargo_views(), 1, "the dependency's build script input changed");
        assert!(
            used.helper_cache("consumer").is_none(),
            "stale evidence would still report helper unused: {:#}",
            used.value["evidence_cache"]
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// Compiler evidence runs build scripts with the environment plain Cargo would give them:
/// no Cargo-Rail session state reaches them, and they can still run their own compiler probes.
#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the literal is the fixture's build-script source"
)]
fn build_scripts_see_no_private_cargo_rail_environment() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        fs::write(
            ws.path.join("crates/other/build.rs"),
            r#"fn main() {
    let leaked = [
        "CARGO_RAIL_RUSTC_WRAPPER",
        "CARGO_RAIL_RUSTDOC_WRAPPER",
        "CARGO_RAIL_COMPILER_CACHE_WRAPPER",
        "CARGO_RAIL_COMPILER_FACT_SESSION",
        "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY",
        "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT",
        "CARGO_RAIL_COMPILER_OBSERVATION_ONLY",
        "CARGO_RAIL_INNER_WORKSPACE_WRAPPER",
        "CARGO_RAIL_INNER_RUSTDOC",
    ]
    .into_iter()
    .filter(|name| std::env::var_os(name).is_some())
    .collect::<Vec<_>>();
    assert!(leaked.is_empty(), "private Cargo-Rail environment reached a build script: {leaked:?}");
    let rustc = std::env::var_os("RUSTC").expect("Cargo sets RUSTC");
    let probe = match std::env::var_os("RUSTC_WORKSPACE_WRAPPER") {
        Some(wrapper) => std::process::Command::new(wrapper).arg(rustc).arg("-vV").output(),
        None => std::process::Command::new(rustc).arg("-vV").output(),
    }
    .expect("compiler probe starts");
    assert!(probe.status.success(), "compiler probe failed: {probe:?}");
}
"#,
        )?;
        ws.commit("Assert the build-script environment")?;

        let run = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(run.status, Some(1), "{:#}\n{}", run.value, run.stderr);
        assert!(run.helper_cache("other").is_some(), "{:#}", run.value["evidence_cache"]);
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// A dependency enabled only by a feature is absent from Cargo's default resolution. A view that
/// enables it still runs its build script, and that view's evidence must be stored and reused.
#[test]
fn a_build_script_enabled_only_by_a_feature_keeps_evidence_reusable() {
    let result: Result<()> = (|| {
        let ws = reuse_workspace()?;
        write_package(
            &ws.path,
            "vendor/generated",
            "generated",
            "",
            "pub fn value() -> u8 { 2 }\n",
        )?;
        fs::write(
            ws.path.join("vendor/generated/build.rs"),
            "fn main() {\n    println!(\"cargo::rerun-if-changed=build.rs\");\n}\n",
        )?;
        write_package(
            &ws.path,
            "crates/other",
            "other",
            "[features]\nextra = [\"dep:generated\"]\n\n[dependencies]\nhelper = { path = \"../../vendor/helper\" }\ngenerated = { path = \"../../vendor/generated\", optional = true }\n",
            "#[cfg(feature = \"extra\")]\npub fn value() -> u8 { generated::value() }\n",
        )?;
        let lockfile = cargo_command(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(
            lockfile.status.success(),
            "offline lockfile generation failed: {lockfile:?}"
        );
        ws.commit("Add a build script behind an optional feature")?;

        let cold = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(cold.status, Some(1), "{:#}\n{}", cold.value, cold.stderr);
        assert!(cold.cargo_views() > 2, "feature views run\n{}", cold.stderr);
        let warm = unify(&ws, &["unify", "--check"], &[])?;
        assert_eq!(
            warm.cargo_views(),
            0,
            "feature-enabled build scripts must not prevent reuse\n{:#}",
            warm.value["evidence_cache"]
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// A Git dependency has no lockfile checksum; its resolved commit is its source identity.
/// Reuse must follow that commit instead of being disabled for the whole workspace.
#[test]
fn a_git_dependency_pinned_to_a_commit_keeps_evidence_reusable() {
    let result: Result<()> = (|| {
        let upstream = tempfile::TempDir::new()?;
        write_package(upstream.path(), ".", "upstream", "", "pub fn value() -> u8 { 1 }\n")?;
        git(upstream.path(), &["init", "--initial-branch=main"])?;
        git(upstream.path(), &["config", "user.name", "Test User"])?;
        git(upstream.path(), &["config", "user.email", "test@example.com"])?;
        git(upstream.path(), &["add", "."])?;
        git(upstream.path(), &["commit", "-m", "Publish upstream"])?;

        let ws = reuse_workspace()?;
        write_package(
            &ws.path,
            "crates/other",
            "other",
            &format!(
                "[dependencies]\nhelper = {{ path = \"../../vendor/helper\" }}\nupstream = {{ git = \"{}\", branch = \"main\" }}\n",
                file_url(upstream.path())
            ),
            "pub fn value() -> u8 { upstream::value() }\n",
        )?;
        let cargo_home = ws.path.join("target/cargo-home");
        let cargo_home = cargo_home.to_str().context("non-UTF-8 Cargo home")?;
        let lockfile = cargo_command(&ws.path)
            .env("CARGO_HOME", cargo_home)
            .arg("generate-lockfile")
            .output()?;
        ensure!(lockfile.status.success(), "lockfile generation failed: {lockfile:?}");
        ws.commit("Depend on a Git package")?;
        let environment = [("CARGO_HOME", cargo_home)];

        let cold = unify(&ws, &["unify", "--check"], &environment)?;
        assert_eq!(cold.cargo_views(), 2, "{}", cold.stderr);
        let warm = unify(&ws, &["unify", "--check"], &environment)?;
        assert_eq!(
            warm.cargo_views(),
            0,
            "a pinned Git source must not disable reuse\n{:#}",
            warm.value["evidence_cache"]
        );

        // A new upstream commit is a new source identity. The lockfile binds every view.
        fs::write(upstream.path().join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n")?;
        git(upstream.path(), &["commit", "-am", "Change upstream"])?;
        let update = cargo_command(&ws.path)
            .env("CARGO_HOME", cargo_home)
            .args(["update", "--package", "upstream"])
            .output()?;
        ensure!(update.status.success(), "Git dependency update failed: {update:?}");
        ws.commit("Update the Git package")?;
        let updated = unify(&ws, &["unify", "--check"], &environment)?;
        assert_eq!(updated.cargo_views(), 2, "{}", updated.stderr);
        let rewarmed = unify(&ws, &["unify", "--check"], &environment)?;
        assert_eq!(rewarmed.cargo_views(), 0, "{:#}", rewarmed.value["evidence_cache"]);
        Ok(())
    })();
    crate::helpers::finish_test(result);
}

/// Observed paths are recorded relative to the repository root, so a workspace below it must
/// revalidate them there rather than at the workspace root.
#[test]
fn a_workspace_below_its_repository_root_reuses_its_evidence() {
    let result: Result<()> = (|| {
        let ws = NestedWorkspace::new("rust")?;
        let root = ws.workspace_root.join("Cargo.toml");
        let manifest = fs::read_to_string(&root)?.replace(
            "members = [\"crates/*\"]",
            "members = [\"crates/*\"]\nexclude = [\"vendor\"]",
        );
        fs::write(&root, manifest)?;
        write_package(
            &ws.workspace_root,
            "vendor/helper",
            "helper",
            "",
            "pub fn help() -> u8 { 1 }\n",
        )?;
        write_package(
            &ws.workspace_root,
            "crates/app",
            "app",
            "[dependencies]\nhelper = { path = \"../../vendor/helper\" }\n",
            "pub fn value() -> u8 { 0 }\n",
        )?;
        let lockfile = cargo_command(&ws.workspace_root)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        ensure!(
            lockfile.status.success(),
            "offline lockfile generation failed: {lockfile:?}"
        );
        ws.commit("Add a nested workspace with an unused path dependency")?;

        let cold = unify_at(&ws.workspace_root, &["unify", "--check"], &[])?;
        assert_eq!(cold.cargo_views(), 1, "{}", cold.stderr);
        let warm = unify_at(&ws.workspace_root, &["unify", "--check"], &[])?;
        assert_eq!(
            warm.cargo_views(),
            0,
            "a nested workspace must reuse its evidence\n{:#}",
            warm.value["evidence_cache"]
        );
        Ok(())
    })();
    crate::helpers::finish_test(result);
}
