use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use cargo_rail::cargo::ResolutionRequest;
use cargo_rail::source::SourceSnapshot;
use cargo_rail::workspace::WorkspaceContext;

use crate::helpers::{TestWorkspace, git, run_cargo_rail};

fn generate_lockfile(root: &Path) -> Result<()> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["generate-lockfile", "--offline"])
        .output()
        .context("failed to generate fixture lockfile")?;
    anyhow::ensure!(
        output.status.success(),
        "fixture lockfile generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn workspace_snapshot_captures_exact_inputs_and_reuses_the_base_resolution() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("workspace-snapshot")?;
        workspace.add_crate("member", "0.1.0", &[])?;
        let rail_config = "# retained snapshot comment\ntargets = []\n";
        std::fs::write(workspace.path.join(".config/rail.toml"), rail_config)?;
        generate_lockfile(&workspace.path)?;
        git(&workspace.path, &["rm", "--cached", ".config/rail.toml"])?;
        std::fs::write(workspace.path.join(".gitignore"), "target/\n.config/rail.toml\n")?;
        workspace.commit("Add snapshot inputs")?;

        let context = WorkspaceContext::build_with_snapshot(&workspace.path)?;
        let snapshot = context.snapshot()?;

        assert!(matches!(snapshot.source(), SourceSnapshot::GitBacked(_)));
        assert_eq!(
            snapshot
                .manifests()
                .iter()
                .map(|manifest| manifest.path().as_str())
                .collect::<Vec<_>>(),
            ["Cargo.toml", "crates/member/Cargo.toml"]
        );
        assert_eq!(
            snapshot
                .manifests()
                .iter()
                .find(|manifest| manifest.path().as_str() == "crates/member/Cargo.toml")
                .expect("member manifest should be captured")
                .bytes(),
            std::fs::read(workspace.path.join("crates/member/Cargo.toml"))?
        );
        assert_eq!(
            snapshot
                .lockfile()
                .expect("generated lockfile should be captured")
                .file()
                .bytes(),
            std::fs::read(workspace.path.join("Cargo.lock"))?
        );
        assert!(
            snapshot
                .lockfile()
                .expect("generated lockfile should be captured")
                .packages()
                .iter()
                .any(|package| package.name() == "member" && package.version() == "0.1.0")
        );
        assert_eq!(
            snapshot
                .rail_config()
                .expect("rail config should be captured losslessly")
                .bytes(),
            rail_config.as_bytes()
        );
        assert!(
            snapshot
                .source()
                .tree()
                .entries()
                .iter()
                .all(|entry| entry.path.as_str() != ".config/rail.toml"),
            "ignored configuration must be an explicit snapshot input, not canonical source"
        );

        let member = snapshot
            .packages()
            .iter()
            .find(|package| package.name() == "member")
            .expect("workspace package identity should be captured");
        assert!(member.is_workspace_member());
        assert_eq!(member.package_root(), Some(Path::new("crates/member")));
        assert_eq!(
            member.manifest_path().map(|path| path.as_str()),
            Some("crates/member/Cargo.toml")
        );
        assert!(
            member.source().is_none(),
            "workspace path packages have no Cargo source"
        );

        let base = context.resolution_view(ResolutionRequest::default())?;
        assert!(std::ptr::eq(base.as_ref(), snapshot.base_resolution()));
        assert!(std::ptr::eq(
            snapshot.base_resolution().metadata(),
            context.cargo().metadata()
        ));
        assert!(std::ptr::eq(snapshot.base_resolution().graph(), context.graph()));
        assert!(snapshot.toolchain().cargo_verbose_version().starts_with("cargo "));
        assert!(snapshot.toolchain().rustc_verbose_version().starts_with("rustc "));
        assert!(snapshot.toolchain().rustdoc_verbose_version().starts_with("rustdoc "));
        assert_eq!(snapshot.targets().len(), 1);
        assert!(snapshot.targets()[0].is_host());
        assert!(snapshot.targets()[0].is_build_target());
        assert!(snapshot.cargo_config().effective_file_settings().is_object());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn workspace_snapshot_preserves_the_filesystem_backed_cargo_only_path() {
    let result: Result<()> = (|| {
        let workspace = tempfile::tempdir()?;
        std::fs::create_dir_all(workspace.path().join("src"))?;
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"filesystem-member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(workspace.path().join("src/lib.rs"), "pub fn value() {}\n")?;
        std::fs::write(workspace.path().join("rail.toml"), "# exact empty configuration\n")?;
        generate_lockfile(workspace.path())?;

        let context = WorkspaceContext::build_with_snapshot(workspace.path())?;
        let snapshot = context.snapshot()?;

        assert!(matches!(snapshot.source(), SourceSnapshot::FilesystemBacked(_)));
        assert!(!context.has_git());
        assert_eq!(snapshot.manifests()[0].path().as_str(), "Cargo.toml");
        assert_eq!(snapshot.packages()[0].package_root(), Some(Path::new("")));
        assert_eq!(
            snapshot
                .rail_config()
                .expect("root rail config should be captured")
                .bytes(),
            b"# exact empty configuration\n"
        );
        assert!(snapshot.lockfile().is_some());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn snapshot_adopts_a_lockfile_generated_by_its_initial_metadata_load() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("generated-lock-snapshot")?;
        workspace.add_crate("member", "0.1.0", &[])?;
        workspace.commit("Add member without lockfile")?;
        assert!(!workspace.path.join("Cargo.lock").exists());

        let context = WorkspaceContext::build_with_snapshot(&workspace.path)?;
        let snapshot = context.snapshot()?;

        assert!(workspace.path.join("Cargo.lock").is_file());
        assert!(snapshot.lockfile().is_some());
        assert!(
            snapshot
                .source()
                .tree()
                .entries()
                .iter()
                .all(|entry| entry.path.as_str() != "Cargo.lock"),
            "a lockfile created by metadata is an explicit snapshot input, not a user-authored worktree change"
        );
        let first_id = snapshot.id();
        let first_source = snapshot.source().clone();
        let first_cargo_config = snapshot.cargo_config().clone();
        let first_toolchain = snapshot.toolchain().clone();
        let first_targets = snapshot.targets().to_vec();
        drop(context);

        let repeated = WorkspaceContext::build_with_snapshot(&workspace.path)?;
        assert_eq!(first_id, repeated.snapshot()?.id());
        assert!(
            repeated
                .snapshot()?
                .source()
                .tree()
                .entries()
                .iter()
                .all(|entry| entry.path.as_str() != "Cargo.lock")
        );
        drop(repeated);

        std::fs::write(
            workspace.path.join("target/cargo-rail/generated-lockfile-v1"),
            b"corrupt provenance marker\n",
        )?;
        let marker_miss = WorkspaceContext::build_with_snapshot(&workspace.path)?;
        assert_eq!(&first_source, marker_miss.snapshot()?.source());
        assert_eq!(&first_cargo_config, marker_miss.snapshot()?.cargo_config());
        assert_eq!(&first_toolchain, marker_miss.snapshot()?.toolchain());
        assert_eq!(&first_targets, marker_miss.snapshot()?.targets());
        assert_eq!(first_id, marker_miss.snapshot()?.id());
        assert!(
            marker_miss
                .snapshot()?
                .source()
                .tree()
                .entries()
                .iter()
                .all(|entry| entry.path.as_str() != "Cargo.lock"),
            "local lockfile provenance must not contaminate authoritative snapshot identity"
        );
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn equivalent_checkout_roots_have_the_same_versioned_snapshot_id() {
    let result: Result<()> = (|| {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        for root in [first.path(), second.path()] {
            std::fs::create_dir_all(root.join("src"))?;
            std::fs::create_dir_all(root.join(".cargo"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"portable-snapshot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            )?;
            std::fs::write(root.join("src/lib.rs"), "pub fn portable() -> bool { true }\n")?;
            std::fs::write(root.join(".cargo/config.toml"), "[build]\nrustflags = []\n")?;
            std::fs::write(root.join("rail.toml"), "# identical lossless config\n")?;
            generate_lockfile(root)?;
        }

        let first_context = WorkspaceContext::build_with_snapshot(first.path())?;
        let second_context = WorkspaceContext::build_with_snapshot(second.path())?;
        let first_id = first_context.snapshot()?.id();
        let second_id = second_context.snapshot()?.id();

        assert_eq!(first_id.version(), 1);
        assert_eq!(first_id, second_id);
        assert_eq!(first_id.to_string(), second_id.to_string());

        std::fs::write(
            second.path().join("src/lib.rs"),
            "pub fn portable() -> bool { false }\n",
        )?;
        let changed_context = WorkspaceContext::build_with_snapshot(second.path())?;
        assert_ne!(first_id, changed_context.snapshot()?.id());
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn plan_identity_binds_lossless_rail_and_semantic_cargo_configuration() {
    let result: Result<()> = (|| {
        let workspace = TestWorkspace::new_named("snapshot-config-identity")?;
        workspace.add_crate("member", "0.1.0", &[])?;
        std::fs::create_dir_all(workspace.path.join(".cargo"))?;
        std::fs::write(workspace.path.join(".gitignore"), "target/\n.cargo/config.toml\n")?;
        std::fs::write(workspace.path.join("rail.toml"), "# first lossless form\n")?;
        std::fs::write(workspace.path.join(".cargo/config.toml"), "[net]\nretry = 2\n")?;
        generate_lockfile(&workspace.path)?;
        workspace.commit("Add snapshot configuration fixture")?;

        let diagnostics = tempfile::tempdir()?;
        let plan_identity = |name: &str| -> Result<String> {
            let output_path = diagnostics.path().join(format!("{name}.json"));
            let output = run_cargo_rail(
                &workspace.path,
                &[
                    "rail",
                    "--diagnostics-file",
                    output_path.to_str().context("diagnostics path should be UTF-8")?,
                    "plan",
                    "--since",
                    "HEAD",
                    "--json",
                ],
            )?;
            anyhow::ensure!(
                output.status.success(),
                "snapshot plan failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            Ok(plan["identity"]
                .as_str()
                .context("plan should contain a portable identity")?
                .to_string())
        };

        let first_id = plan_identity("first")?;
        std::fs::write(workspace.path.join("rail.toml"), "# second lossless form\n")?;
        assert_ne!(first_id, plan_identity("changed-rail")?);

        std::fs::write(workspace.path.join("rail.toml"), "# first lossless form\n")?;
        std::fs::write(workspace.path.join(".cargo/config.toml"), "[net]\nretry = 3\n")?;
        assert_ne!(first_id, plan_identity("changed-cargo")?);
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn equivalent_git_checkouts_ignore_host_roots_and_git_object_ids() {
    let result: Result<()> = (|| {
        let first = TestWorkspace::new_named("portable-git-snapshot")?;
        let second = TestWorkspace::new_named("portable-git-snapshot")?;
        for workspace in [&first, &second] {
            workspace.add_crate("member", "0.1.0", &[])?;
            generate_lockfile(&workspace.path)?;
        }
        first.commit("Add first portable member")?;
        second.commit("Add second portable member")?;

        let first_context = WorkspaceContext::build_with_snapshot(&first.path)?;
        let second_context = WorkspaceContext::build_with_snapshot(&second.path)?;
        assert_ne!(
            git(&first.path, &["rev-parse", "HEAD"])?.stdout,
            git(&second.path, &["rev-parse", "HEAD"])?.stdout,
            "fixture commits should prove Git object IDs are not snapshot identity"
        );
        assert_eq!(first_context.snapshot()?.id(), second_context.snapshot()?.id());
        Ok(())
    })();
    super::helpers::finish_test(result);
}
