//! Regression wall for planner file taxonomy and builtin surfaces.

use crate::helpers::{TestWorkspace, git, run_cargo_rail};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::process::Command;

const CLASSIFICATION_CORPUS: &str = include_str!("../fixtures/change_detection/path_corpus.json");

#[derive(Debug, Deserialize)]
struct CorpusCase {
    path: String,
    kind: String,
    #[serde(default)]
    sub_kind: Option<String>,
    surfaces: Vec<String>,
}

fn corpus_cases() -> Vec<CorpusCase> {
    serde_json::from_str(CLASSIFICATION_CORPUS).expect("classification corpus should parse")
}

fn write_case_file(ws: &TestWorkspace, case: &CorpusCase) -> Result<()> {
    if case.path == "Cargo.lock" {
        let output = Command::new("cargo")
            .current_dir(&ws.path)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "fixture lockfile generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }

    let path = ws.path.join(&case.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        let semantic_change = match case.path.as_str() {
            "Cargo.toml" => "\n[profile.dev]\nopt-level = 1\n",
            "crates/lib-a/Cargo.toml" => "\n[features]\nplanner-corpus = []\n",
            _ => "\n# planner corpus regression\n",
        };
        std::fs::write(&path, format!("{existing}{semantic_change}"))?;
        return Ok(());
    }

    let contents = match case.path.as_str() {
        ".github/workflows/ci.yml" => "name: CI\non: [push]\n",
        "config/pipeline.yaml" => "jobs:\n  build: true\n",
        "scripts/check.sh" => "#!/usr/bin/env bash\necho check\n",
        "rust-toolchain.toml" => "[toolchain]\nchannel = \"stable\"\n",
        "vendor/foreign/Cargo.lock" => {
            "# foreign lockfile\nversion = 3\n\n[[package]]\nname = \"foreign\"\nversion = \"1.0.0\"\n"
        }
        ".gitignore" => "target/\n*.log\n",
        path if path.ends_with(".md") => "# Planner corpus\n",
        path if path.ends_with(".rs") => "fn corpus_case() {}\n",
        _ => "planner corpus\n",
    };

    std::fs::write(path, contents)?;
    Ok(())
}

#[test]
fn test_plan_classification_corpus() {
    let result: Result<()> = (|| {
        for (index, case) in corpus_cases().into_iter().enumerate() {
            let ws = TestWorkspace::new_named(&format!("plan-classification-corpus-{index}"))?;
            ws.add_crate("lib-a", "0.1.0", &[])?;
            ws.commit("add crate")?;

            git(&ws.path, &["branch", "origin/main"])?;
            write_case_file(&ws, &case)?;
            ws.commit("apply classification corpus case")?;

            let output = run_cargo_rail(
                &ws.path,
                &["rail", "plan", "--since", "origin/main", "--format", "json"],
            )?;
            assert!(
                output.status.success(),
                "plan should succeed for {}\nstdout:\n{}\nstderr:\n{}",
                case.path,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            let json: Value = serde_json::from_slice(&output.stdout)?;
            let planned_file = json["files"]
                .as_array()
                .and_then(|files| {
                    files
                        .iter()
                        .find(|file| file["path"].as_str() == Some(case.path.as_str()))
                })
                .ok_or_else(|| anyhow!("planned file '{}' missing from output", case.path))?;

            assert_eq!(
                planned_file["kind"],
                Value::String(case.kind.clone()),
                "kind mismatch for {}",
                case.path
            );
            assert_eq!(
                planned_file["sub_kind"].as_str().map(str::to_string),
                case.sub_kind.clone(),
                "sub kind mismatch for {}",
                case.path
            );

            for surface in ["build", "test", "bench", "docs", "infra", "surface"] {
                let expected = case.surfaces.iter().any(|configured| configured == surface);
                assert_eq!(
                    json["surfaces"][surface]["enabled"],
                    Value::Bool(expected),
                    "surface '{}' mismatch for {}",
                    surface,
                    case.path
                );
            }
        }

        Ok(())
    })();
    super::helpers::finish_test(result);
}
