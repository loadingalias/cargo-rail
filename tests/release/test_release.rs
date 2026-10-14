//! Each test drives the real `cargo rail release` against a bare `origin` reached as
//! `git@github.com:owner/repo.git`, a fake GitHub API, and a fake sparse registry.

use crate::fakes::{Check, FakeGithub, FakeRegistry, write_gh, write_ssh};
use crate::helpers::{cargo_command, cargo_rail_command, finish_test, git};
use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Output;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
    bin: PathBuf,
    attested: PathBuf,
    cargo_home: PathBuf,
    github: FakeGithub,
    registry: FakeRegistry,
}

impl Fixture {
    /// A workspace with library `base` and, when `dependent`, `app` that depends on it.
    fn new(dependent: bool, release: &str) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("workspace");
        let origin = temp.path().join("origin.git");
        let bin = temp.path().join("bin");
        let attested = temp.path().join("attested");
        let cargo_home = temp.path().join("cargo-home");
        for directory in [&root, &bin, &attested, &cargo_home] {
            std::fs::create_dir_all(directory)?;
        }
        let github = FakeGithub::start()?;
        let registry = FakeRegistry::start()?;
        std::fs::write(
            cargo_home.join("config.toml"),
            format!(
                "[registries.fixture]\nindex = {:?}\n[net]\nretry = 0\n",
                registry.index_url()
            ),
        )?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        )?;
        std::fs::write(root.join(".gitignore"), "/target\n")?;
        std::fs::create_dir_all(root.join(".config"))?;
        std::fs::write(
            root.join(".config/rail.toml"),
            format!("[release]\npublish = \"fixture\"\nsemver_check = \"off\"\n{release}"),
        )?;
        let mut crates = vec![("base", "pub fn value() -> u32 { 1 }\n", String::new())];
        if dependent {
            crates.push((
                "app",
                "pub fn value() -> u32 { base::value() + 1 }\n",
                "[dependencies]\nbase = { path = \"../base\", version = \"0.1.0\", registry = \"fixture\" }\n"
                    .to_owned(),
            ));
        }
        for (name, source, dependencies) in crates {
            let directory = root.join("crates").join(name);
            std::fs::create_dir_all(directory.join("src"))?;
            std::fs::write(
                directory.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\ndescription = \"Release fixture\"\npublish = [\"fixture\"]\n\n{dependencies}"
                ),
            )?;
            std::fs::write(directory.join("src/lib.rs"), source)?;
        }
        let fixture = Self {
            _temp: temp,
            root,
            origin,
            bin,
            attested,
            cargo_home,
            github,
            registry,
        };
        let lockfile = fixture.cargo().args(["generate-lockfile", "--offline"]).output()?;
        ensure!(lockfile.status.success(), "{lockfile:?}");
        git(&fixture.root, &["init", "--quiet", "--initial-branch=main"])?;
        git(&fixture.root, &["add", "."])?;
        git(&fixture.root, &["commit", "--quiet", "-m", "Initial workspace"])?;
        // Every crate starts released, so later changes are measured from its tag.
        for name in if dependent { vec!["base", "app"] } else { vec!["base"] } {
            git(
                &fixture.root,
                &["tag", "-a", &format!("{name}-v0.1.0"), "-m", "Initial release"],
            )?;
        }
        git(
            fixture.origin.parent().context("origin parent")?,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                fixture.origin.to_str().context("UTF-8")?,
            ],
        )?;
        let ssh = write_ssh(&fixture.bin, &fixture.origin)?;
        write_gh(&fixture.bin)?;
        git(
            &fixture.root,
            &["remote", "add", "origin", "git@github.com:owner/repo.git"],
        )?;
        git(
            &fixture.root,
            &["config", "core.sshCommand", ssh.to_str().context("UTF-8")?],
        )?;
        git(
            &fixture.root,
            &["push", "--quiet", "--set-upstream", "origin", "main", "--tags"],
        )?;
        Ok(fixture)
    }

    fn cargo(&self) -> std::process::Command {
        let mut command = cargo_command(&self.root);
        command.env("CARGO_HOME", &self.cargo_home);
        command
    }

    /// Commit a code change with a change file and push it.
    fn change(&self, intents: &[(&str, &str)], body: &str) -> Result<String> {
        let changes = self.root.join(".changes");
        std::fs::create_dir_all(&changes)?;
        let mut content = String::from("---\n");
        for (name, bump) in intents {
            content.push_str(&format!("\"{name}\" = \"{bump}\"\n"));
            let source = self.root.join("crates").join(name).join("src/lib.rs");
            let mut code = std::fs::read_to_string(&source)?;
            let next = code.matches("pub fn added_").count();
            code.push_str(&format!("pub fn added_{next}() {{}}\n"));
            std::fs::write(source, code)?;
        }
        content.push_str(&format!("---\n\n{body}\n"));
        std::fs::write(changes.join(format!("{}.md", intents[0].0)), content)?;
        git(&self.root, &["add", "."])?;
        git(&self.root, &["commit", "--quiet", "-m", body])?;
        git(&self.root, &["push", "--quiet"])?;
        self.head()
    }

    fn command(&self, args: &[&str]) -> Result<std::process::Command> {
        let mut command = cargo_rail_command(&self.root)?;
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )?;
        command
            .args(["rail", "release"])
            .args(args)
            .env("PATH", path)
            .env("GITHUB_API_URL", self.github.api_url())
            .env("GH_TOKEN", "test-token")
            .env("CARGO_HOME", &self.cargo_home)
            .env("CARGO_REGISTRIES_FIXTURE_TOKEN", "fixture-token")
            .env("CARGO_NET_RETRY", "0")
            .env("FAKE_GH_ATTESTED", &self.attested);
        Ok(command)
    }

    /// Run `cargo rail release -f json` with `args` and return the output and its JSON value.
    fn release(&self, args: &[&str]) -> Result<(Output, Value)> {
        let output = self.command(&[args, &["-f", "json"]].concat())?.output()?;
        let value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        Ok((output, value))
    }

    /// Run a release that must succeed and end in `state`.
    fn release_ok(&self, args: &[&str], state: &str) -> Result<Value> {
        let (output, value) = self.release(args)?;
        ensure!(
            output.status.success() && value["state"] == state,
            "expected {state}: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(value)
    }

    /// Run a release that must fail before printing a result, and return its error text.
    fn release_err(&self, args: &[&str]) -> Result<String> {
        let (output, _) = self.release(args)?;
        ensure!(!output.status.success(), "release unexpectedly succeeded: {output:?}");
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    fn head(&self) -> Result<String> {
        Ok(String::from_utf8(git(&self.root, &["rev-parse", "HEAD"])?.stdout)?
            .trim()
            .to_owned())
    }

    /// The commit a ref names on origin, peeling tags.
    fn origin_ref(&self, reference: &str) -> Result<Option<String>> {
        let output = crate::helpers::git_command(&self.root)
            .args([
                "--git-dir",
                self.origin.to_str().context("UTF-8")?,
                "rev-parse",
                "--verify",
                "--quiet",
            ])
            .arg(format!("{reference}^{{commit}}"))
            .output()?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    /// Record an attestation for `name` made from `commit`, as `actions/attest` would.
    #[cfg(unix)]
    fn attest(&self, name: &str, commit: &str) -> Result<()> {
        std::fs::write(self.attested.join(name), commit)?;
        Ok(())
    }

    fn uploads(&self) -> Vec<String> {
        self.registry.with(|state| state.attempts.clone())
    }

    #[cfg(unix)]
    fn hook(&self, repository: &std::path::Path, name: &str, script: &str) -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let path = repository.join("hooks").join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
}

#[test]
fn a_library_release_runs_each_effect_once_and_a_rerun_finds_nothing() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let source = fixture.change(&[("base", "minor")], "Add a feature.")?;

        let planned = fixture.release_ok(&["--dry-run"], "planned")?;
        let release = &planned["crates"][0];
        assert_eq!(release["name"], "base");
        assert_eq!(release["previous_version"], "0.1.0");
        assert_eq!(release["version"], "0.2.0");
        assert_eq!(release["tag"], "base-v0.2.0");
        assert_eq!(release["registry"], "pending");
        assert_eq!(release["github_release"], "pending");
        assert!(
            release["notes"]
                .as_str()
                .is_some_and(|notes| notes.contains("Add a feature.")),
            "{planned}"
        );
        assert_eq!(fixture.head()?, source, "a dry run changes nothing");
        assert_eq!(fixture.origin_ref("refs/tags/base-v0.2.0")?, None);
        assert!(fixture.uploads().is_empty());
        assert!(fixture.github.with(|state| state.releases.is_empty()));

        let released = fixture.release_ok(&["--yes"], "released")?;
        let commit = released["commit"].as_str().context("release commit")?.to_owned();
        assert_eq!(fixture.head()?, commit);
        assert_eq!(fixture.origin_ref("refs/heads/main")?.as_deref(), Some(commit.as_str()));
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v0.2.0")?.as_deref(),
            Some(commit.as_str())
        );
        let message = String::from_utf8(git(&fixture.root, &["log", "-1", "--format=%B"])?.stdout)?;
        assert!(message.starts_with("chore(release): base v0.2.0\n"), "{message}");
        assert!(message.contains("Rail-Release-Crates: base@0.2.0"), "{message}");
        assert!(
            !fixture.root.join(".changes/base.md").exists(),
            "the change file is consumed"
        );
        assert!(std::fs::read_to_string(fixture.root.join("crates/base/Cargo.toml"))?.contains("version = \"0.2.0\""));
        assert_eq!(fixture.uploads(), ["base@0.2.0"]);
        assert_eq!(
            fixture.registry.source_commit("base", "0.2.0")?.as_deref(),
            Some(commit.as_str())
        );
        let github = fixture.github.release("base-v0.2.0").context("GitHub release")?;
        assert!(!github.draft);
        assert_eq!(github.target, commit);
        assert_eq!(github.name, "base v0.2.0");
        assert!(github.body.contains("Add a feature."), "{}", github.body);
        assert!(git(&fixture.root, &["status", "--porcelain"])?.stdout.is_empty());

        fixture.release_ok(&["--yes"], "nothing_to_release")?;
        assert_eq!(fixture.uploads(), ["base@0.2.0"], "nothing is uploaded twice");
        assert_eq!(fixture.github.with(|state| state.releases.len()), 1);
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn dependent_crates_publish_in_order_and_a_failed_upload_finishes_on_rerun() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(true, "")?;
        fixture.change(&[("base", "minor")], "Change the base API.")?;
        fixture.registry.with(|state| state.reject_once.push("app".into()));

        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("`cargo publish` failed for app 0.1.1"), "{error}");
        let commit = fixture.origin_ref("refs/heads/main")?.context("pushed release")?;
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v0.2.0")?.as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(
            fixture.origin_ref("refs/tags/app-v0.1.1")?.as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(fixture.uploads(), ["base@0.2.0", "app@0.1.1"]);
        assert!(
            fixture.github.with(|state| state.releases.is_empty()),
            "GitHub releases follow the registry"
        );
        let manifest = std::fs::read_to_string(fixture.root.join("crates/app/Cargo.toml"))?;
        assert!(
            manifest.contains("version = \"0.2.0\""),
            "the dependent requires the new base:\n{manifest}"
        );

        let released = fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(released["commit"], commit.as_str());
        assert_eq!(
            fixture.uploads(),
            ["base@0.2.0", "app@0.1.1", "app@0.1.1"],
            "only the missing upload repeats"
        );
        for name in ["base", "app"] {
            let version = if name == "base" { "0.2.0" } else { "0.1.1" };
            assert_eq!(
                fixture.registry.source_commit(name, version)?.as_deref(),
                Some(commit.as_str())
            );
            assert!(
                fixture
                    .github
                    .release(&format!("{name}-v{version}"))
                    .is_some_and(|release| !release.draft)
            );
        }
        Ok(())
    })();
    finish_test(result);
}

// The fake `gh` is a POSIX script; on Windows, `Command::new("gh")` finds only `gh.exe`.
#[cfg(unix)]
#[test]
fn assets_hold_every_irreversible_effect_until_each_attested_file_is_attached() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(
            false,
            "[release.assets]\nbase = [\"base-{version}-linux.zip\", \"SHA256SUMS\"]\n[release.aliases]\nbase = \"v{major}\"\n",
        )?;
        fixture.change(&[("base", "minor")], "Ship a binary.")?;

        let waiting = fixture.release_ok(&["--yes"], "awaiting_assets")?;
        let commit = waiting["commit"].as_str().context("commit")?.to_owned();
        assert_eq!(
            waiting["crates"][0]["missing_assets"],
            serde_json::json!(["base-0.2.0-linux.zip", "SHA256SUMS"])
        );
        let draft = fixture.github.release("base-v0.2.0").context("draft")?;
        assert!(draft.draft && draft.body.contains("Ship a binary."));
        assert!(fixture.uploads().is_empty(), "no upload before the assets exist");

        fixture
            .github
            .attach("base-v0.2.0", "base-0.2.0-linux.zip", b"binary")?;
        let waiting = fixture.release_ok(&["--yes"], "awaiting_assets")?;
        assert_eq!(
            waiting["crates"][0]["missing_assets"],
            serde_json::json!(["SHA256SUMS"])
        );

        fixture.github.attach("base-v0.2.0", "SHA256SUMS", b"sums")?;
        fixture.attest("base-0.2.0-linux.zip", &commit)?;
        let error = fixture.release_err(&["--yes"])?;
        assert!(
            error.contains("SHA256SUMS has no attestation") && error.contains("no attestations found for SHA256SUMS"),
            "{error}"
        );
        fixture.attest("SHA256SUMS", "0000000000000000000000000000000000000000")?;
        let error = fixture.release_err(&["--yes"])?;
        assert!(
            error.contains("its attestations name 0000000000000000000000000000000000000000"),
            "{error}"
        );
        assert!(
            fixture.uploads().is_empty()
                && fixture
                    .github
                    .release("base-v0.2.0")
                    .is_some_and(|release| release.draft)
        );

        // The release workflow that made the release commit ran at its parent; that run's attestations count.
        let prepared_from = String::from_utf8(git(&fixture.root, &["rev-parse", &format!("{commit}^")])?.stdout)?;
        fixture.attest("SHA256SUMS", prepared_from.trim())?;
        fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(fixture.uploads(), ["base@0.2.0"]);
        assert!(
            fixture
                .github
                .release("base-v0.2.0")
                .is_some_and(|release| !release.draft)
        );
        assert_eq!(fixture.origin_ref("refs/tags/v0")?.as_deref(), Some(commit.as_str()));
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn unattested_assets_are_accepted_only_when_attestations_are_off() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(
            false,
            "asset_attestations = \"off\"\n[release.assets]\nbase = [\"base.zip\"]\n",
        )?;
        fixture.change(&[("base", "patch")], "Fix the binary.")?;
        fixture.release_ok(&["--yes"], "awaiting_assets")?;
        fixture.github.attach("base-v0.1.1", "base.zip", b"binary")?;
        fixture.release_ok(&["--yes"], "released")?;
        Ok(())
    })();
    finish_test(result);
}

#[cfg(unix)]
#[test]
fn a_release_killed_before_its_push_is_pushed_by_the_rerun() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let source = fixture.change(&[("base", "patch")], "Fix a bug.")?;
        // The hook's parent is `git push`; its parent is the release, killed mid-push.
        fixture.hook(
            &fixture.root.join(".git"),
            "pre-push",
            "#!/bin/sh\nkill -9 \"$(ps -o ppid= -p \"$PPID\" | tr -d ' ')\"\nexit 1\n",
        )?;
        let (output, _) = fixture.release(&["--yes"])?;
        assert!(!output.status.success());
        let commit = fixture.head()?;
        assert_ne!(commit, source, "the release commit exists locally");
        assert_eq!(fixture.origin_ref("refs/heads/main")?.as_deref(), Some(source.as_str()));
        assert_eq!(fixture.origin_ref("refs/tags/base-v0.1.1")?, None);

        std::fs::remove_file(fixture.root.join(".git/hooks/pre-push"))?;
        let released = fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(released["commit"], commit.as_str());
        assert_eq!(fixture.origin_ref("refs/heads/main")?.as_deref(), Some(commit.as_str()));
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v0.1.1")?.as_deref(),
            Some(commit.as_str())
        );
        assert_eq!(fixture.uploads(), ["base@0.1.1"]);
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn a_github_failure_after_publication_resumes_without_a_second_upload() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        fixture.change(&[("base", "patch")], "Fix a bug.")?;
        fixture
            .github
            .with(|state| state.failures.insert("POST /repos/owner/repo/releases".into(), 502));
        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("HTTP 502"), "{error}");
        assert_eq!(fixture.uploads(), ["base@0.1.1"]);

        fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(
            fixture.uploads(),
            ["base@0.1.1"],
            "the rerun proves the upload instead of repeating it"
        );
        assert!(
            fixture
                .github
                .release("base-v0.1.1")
                .is_some_and(|release| !release.draft)
        );
        Ok(())
    })();
    finish_test(result);
}

#[cfg(unix)]
#[test]
fn a_rejected_alias_moves_on_rerun_and_follows_each_release() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "[release.aliases]\nbase = \"v{major}\"\n")?;
        fixture.change(&[("base", "patch")], "Fix a bug.")?;
        let hook = fixture.origin.join("hooks/pre-receive");
        fixture.hook(
            &fixture.origin,
            "pre-receive",
            &format!(
                "#!/bin/sh\nif grep -q ' refs/tags/v0$'; then rm -f '{}'; echo 'alias rejected' >&2; exit 1; fi\n",
                hook.display()
            ),
        )?;
        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("alias rejected"), "{error}");
        assert!(
            fixture
                .github
                .release("base-v0.1.1")
                .is_some_and(|release| !release.draft)
        );
        assert_eq!(fixture.origin_ref("refs/tags/v0")?, None);

        let released = fixture.release_ok(&["--yes"], "released")?;
        let first = released["commit"].as_str().context("commit")?.to_owned();
        assert_eq!(fixture.origin_ref("refs/tags/v0")?.as_deref(), Some(first.as_str()));

        fixture.change(&[("base", "patch")], "Fix another bug.")?;
        let second = fixture.release_ok(&["--yes"], "released")?;
        let second = second["commit"].as_str().context("commit")?.to_owned();
        assert_eq!(fixture.origin_ref("refs/tags/v0")?.as_deref(), Some(second.as_str()));
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn a_literal_major_alias_never_names_another_major() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "[release.aliases]\nbase = \"v0\"\n")?;
        fixture.change(&[("base", "major")], "Break the API.")?;
        let error = fixture.release_err(&["--yes", "--bump", "major"])?;
        assert!(
            error.contains("alias v0 would name base 1.0.0") && error.contains("v{major}"),
            "{error}"
        );
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v1.0.0")?,
            None,
            "no effect precedes the check"
        );
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn checks_on_the_source_commit_gate_the_first_effect() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let source = fixture.change(&[("base", "patch")], "Fix a bug.")?;
        let unchanged = |fixture: &Fixture| -> Result<()> {
            assert_eq!(fixture.head()?, source);
            assert_eq!(fixture.origin_ref("refs/heads/main")?.as_deref(), Some(source.as_str()));
            assert_eq!(fixture.origin_ref("refs/tags/base-v0.1.1")?, None);
            assert!(fixture.uploads().is_empty());
            Ok(())
        };
        let set = |checks: Vec<Check>| fixture.github.with(|state| state.checks.insert(source.clone(), checks));

        set(vec![Check {
            name: "tests".into(),
            status: "in_progress",
            conclusion: None,
            suite: 2,
        }]);
        let (output, value) = fixture.release(&["--yes"])?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(value["state"], "awaiting_checks");
        assert_eq!(value["pending_checks"], serde_json::json!(["tests"]));
        unchanged(&fixture)?;

        set(vec![Check {
            name: "tests".into(),
            status: "completed",
            conclusion: Some("failure"),
            suite: 2,
        }]);
        let error = fixture.release_err(&["--yes"])?;
        assert!(
            error.contains("checks failed") && error.contains("tests (failure)"),
            "{error}"
        );
        unchanged(&fixture)?;

        set(Vec::new());
        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("reports no checks"), "{error}");
        unchanged(&fixture)?;

        // The workflow run that invokes the release reports itself as pending; it is excluded.
        set(vec![
            Check::success("tests"),
            Check {
                name: "release".into(),
                status: "in_progress",
                conclusion: None,
                suite: 77,
            },
        ]);
        fixture.github.with(|state| state.runs.insert(9001, 77));
        let output = fixture
            .command(&["--yes", "-f", "json"])?
            .env("GITHUB_RUN_ID", "9001")
            .env("GITHUB_REPOSITORY", "owner/repo")
            .output()?;
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fixture.uploads(), ["base@0.1.1"]);
        Ok(())
    })();
    finish_test(result);
}

#[cfg(unix)]
#[test]
fn a_concurrent_push_fails_the_lease_and_restores_the_checkout() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let source = fixture.change(&[("base", "patch")], "Fix a bug.")?;
        // A teammate's commit lands on origin while the release pushes.
        let tree = String::from_utf8(git(&fixture.root, &["rev-parse", &format!("{source}^{{tree}}")])?.stdout)?;
        let teammate = String::from_utf8(
            git(
                &fixture.root,
                &["commit-tree", tree.trim(), "-p", &source, "-m", "Teammate change"],
            )?
            .stdout,
        )?;
        let teammate = teammate.trim().to_owned();
        git(
            &fixture.root,
            &["push", "--quiet", "origin", &format!("{teammate}:refs/heads/teammate")],
        )?;
        let hook = fixture.root.join(".git/hooks/pre-push");
        fixture.hook(
            &fixture.root.join(".git"),
            "pre-push",
            &format!(
                "#!/bin/sh\ngit --git-dir='{}' update-ref refs/heads/main {teammate}\nrm -f '{}'\n",
                fixture.origin.display(),
                hook.display()
            ),
        )?;

        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("nothing was released"), "{error}");
        assert_eq!(fixture.head()?, source, "the checkout is back at its starting commit");
        assert!(git(&fixture.root, &["tag", "--list", "base-v0.1.1"])?.stdout.is_empty());
        assert_eq!(
            fixture.origin_ref("refs/heads/main")?.as_deref(),
            Some(teammate.as_str())
        );
        assert_eq!(fixture.origin_ref("refs/tags/base-v0.1.1")?, None);
        assert!(fixture.uploads().is_empty());
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn a_squash_merged_release_pull_request_is_tagged_and_published_without_its_trailer() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let source = fixture.change(&[("base", "minor")], "Add a feature.")?;

        let opened = fixture.release_ok(&["--pr", "--yes"], "pull_request")?;
        assert_eq!(
            fixture.head()?,
            source,
            "a release pull request leaves the branch alone"
        );
        let release_commit = fixture
            .origin_ref("refs/heads/rail/release/base-v0.2.0")?
            .context("release branch")?;
        assert_eq!(opened["commit"], release_commit.as_str());
        assert_eq!(fixture.origin_ref("refs/tags/base-v0.2.0")?, None);
        let number = fixture.github.with(|state| {
            let pull = state.pulls.last().cloned().expect("pull request");
            assert_eq!(pull.head_ref, "rail/release/base-v0.2.0");
            assert_eq!(pull.title, "chore(release): base v0.2.0");
            assert_eq!(pull.base, "main");
            assert!(pull.body.contains("Rail-Release-Crates: base@0.2.0"), "{}", pull.body);
            pull.number
        });
        fixture.release_ok(&["--yes"], "pull_request")?;

        // Squash with the "pull request title" message setting: the trailer is gone.
        let tree =
            String::from_utf8(git(&fixture.root, &["rev-parse", &format!("{release_commit}^{{tree}}")])?.stdout)?;
        git(
            &fixture.root,
            &["fetch", "--quiet", "origin", "rail/release/base-v0.2.0"],
        )?;
        let squashed = String::from_utf8(
            git(
                &fixture.root,
                &[
                    "commit-tree",
                    tree.trim(),
                    "-p",
                    &source,
                    "-m",
                    &format!("Release base 0.2.0 (#{number})"),
                ],
            )?
            .stdout,
        )?;
        let squashed = squashed.trim().to_owned();
        git(
            &fixture.root,
            &["push", "--quiet", "origin", &format!("{squashed}:refs/heads/main")],
        )?;
        git(
            &fixture.root,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{release_commit}:refs/pull/{number}/head"),
            ],
        )?;
        git(
            &fixture.root,
            &["push", "--quiet", "origin", "--delete", "rail/release/base-v0.2.0"],
        )?;
        fixture.github.with(|state| {
            let pull = state.pulls.last_mut().expect("pull request");
            pull.open = false;
            pull.head_sha = release_commit.clone();
            pull.merge_commit = Some(squashed.clone());
        });
        git(&fixture.root, &["pull", "--quiet", "--ff-only"])?;
        fixture.github.with(|state| {
            state.checks.insert(
                squashed.clone(),
                vec![Check {
                    name: "tests".into(),
                    status: "queued",
                    conclusion: None,
                    suite: 3,
                }],
            )
        });
        let (output, waiting) = fixture.release(&["--yes"])?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            waiting["state"], "awaiting_checks",
            "the merged commit's own checks gate its tag"
        );
        assert_eq!(fixture.origin_ref("refs/tags/base-v0.2.0")?, None);

        fixture.github.with(|state| state.checks.remove(&squashed));
        let released = fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(released["commit"], squashed.as_str());
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v0.2.0")?.as_deref(),
            Some(squashed.as_str())
        );
        assert_eq!(
            fixture.registry.source_commit("base", "0.2.0")?.as_deref(),
            Some(squashed.as_str())
        );
        assert_eq!(
            fixture.github.release("base-v0.2.0").context("release")?.target,
            squashed
        );
        Ok(())
    })();
    finish_test(result);
}

/// A release commit as Cargo-Rail 0.31 made it: versions in the subject, a record trailer, no new trailer.
fn legacy_release(fixture: &Fixture) -> Result<String> {
    let manifest = fixture.root.join("crates/base/Cargo.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)?.replace("version = \"0.1.0\"", "version = \"0.2.0\""),
    )?;
    let lockfile = fixture.cargo().args(["generate-lockfile", "--offline"]).output()?;
    ensure!(lockfile.status.success(), "{lockfile:?}");
    git(&fixture.root, &["add", "."])?;
    git(
        &fixture.root,
        &[
            "commit",
            "--quiet",
            "-m",
            "chore(release): base v0.2.0\n\nRail-Release: release-0123456789ab\nRail-Release-Contract: 11\nRail-Release-Intent: 0123456789abcdef",
        ],
    )?;
    let commit = fixture.head()?;
    git(
        &fixture.root,
        &["tag", "-a", "base-v0.2.0", "-m", "Release base v0.2.0"],
    )?;
    git(&fixture.root, &["push", "--quiet", "origin", "main", "base-v0.2.0"])?;
    Ok(commit)
}

#[test]
fn a_release_started_by_cargo_rail_0_31_is_finished_and_its_records_are_named() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let commit = legacy_release(&fixture)?;
        git(
            &fixture.root,
            &["push", "--quiet", "origin", "HEAD:refs/notes/cargo-rail/active"],
        )?;

        let released = fixture.release_ok(&["--yes"], "released")?;
        assert_eq!(released["commit"], commit.as_str());
        assert_eq!(
            fixture.registry.source_commit("base", "0.2.0")?.as_deref(),
            Some(commit.as_str())
        );
        assert!(
            fixture
                .github
                .release("base-v0.2.0")
                .is_some_and(|release| !release.draft)
        );
        let warning = released["warnings"][0].as_str().unwrap_or_default();
        assert!(
            warning.contains("refs/notes/cargo-rail/active") && warning.contains("git push origin --delete"),
            "{released}"
        );
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn a_version_published_from_another_commit_stops_the_release() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let start = fixture.head()?;
        let commit = legacy_release(&fixture)?;
        fixture.release_ok(&["--yes"], "released")?;

        // Rewrite history so another commit claims the version already on the registry.
        git(
            &fixture.root,
            &[
                "push",
                "--quiet",
                "--force",
                "origin",
                &format!("{start}:refs/heads/main"),
            ],
        )?;
        git(&fixture.root, &["push", "--quiet", "origin", "--delete", "base-v0.2.0"])?;
        git(&fixture.root, &["tag", "-d", "base-v0.2.0"])?;
        git(&fixture.root, &["reset", "--quiet", "--hard", &start])?;
        fixture.github.with(|state| state.releases.clear());
        let other = legacy_release(&fixture)?;
        assert_ne!(other, commit);

        let error = fixture.release_err(&["--yes"])?;
        assert!(
            error.contains(&format!("was published from {commit}, not release commit {other}")),
            "{error}"
        );
        assert!(fixture.github.with(|state| state.releases.is_empty()));
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn a_new_release_never_reuses_a_published_version() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        let start = fixture.head()?;
        fixture.change(&[("base", "minor")], "Add a feature.")?;
        let released = fixture.release_ok(&["--yes"], "released")?;
        let commit = released["commit"].as_str().context("commit")?.to_owned();

        // Roll origin back as if the release never landed; the registry still holds 0.2.0.
        git(&fixture.root, &["reset", "--quiet", "--hard", &format!("{commit}^")])?;
        git(&fixture.root, &["push", "--quiet", "--force", "origin", "HEAD:main"])?;
        git(&fixture.root, &["push", "--quiet", "origin", "--delete", "base-v0.2.0"])?;
        git(&fixture.root, &["tag", "-d", "base-v0.2.0"])?;
        assert_ne!(fixture.head()?, start);

        let error = fixture.release_err(&["--yes"])?;
        assert!(error.contains("base 0.2.0 is already on fixture"), "{error}");
        assert_eq!(
            fixture.origin_ref("refs/tags/base-v0.2.0")?,
            None,
            "no effect precedes the check"
        );
        Ok(())
    })();
    finish_test(result);
}

#[test]
fn removed_release_subcommands_name_their_replacement() {
    let result: Result<()> = (|| {
        let fixture = Fixture::new(false, "")?;
        for (removed, replacement) in [
            ("run", "cargo rail release`"),
            ("check", "--dry-run"),
            ("resume", "again"),
            ("status", "--dry-run"),
            ("abort", "failed push restores the checkout"),
        ] {
            let output = cargo_rail_command(&fixture.root)?
                .args(["rail", "release", removed])
                .output()?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{removed}: {stderr}");
            assert!(
                stderr.contains(&format!(
                    "`cargo rail release {removed}` was removed in Cargo-Rail 0.32.0"
                )) && stderr.contains(replacement),
                "{removed}: {stderr}"
            );
        }
        Ok(())
    })();
    finish_test(result);
}
