//! Integration tests for canonical path classification.

use cargo_rail::change_detection::classify::{ChangeKind, ConfigKind, classify_file, classify_path};
use serde::Deserialize;
use std::path::Path;

const CLASSIFICATION_CORPUS: &str = include_str!("../fixtures/change_detection/path_corpus.json");

#[derive(Debug, Deserialize)]
struct CorpusCase {
    path: String,
    kind: String,
    #[serde(default)]
    sub_kind: Option<String>,
    #[serde(default)]
    default_surfaces: Option<Vec<String>>,
    surfaces: Vec<String>,
}

fn corpus_cases() -> Vec<CorpusCase> {
    serde_json::from_str(CLASSIFICATION_CORPUS).expect("classification corpus should parse")
}

#[test]
fn test_classification_corpus_matches_planner_taxonomy() {
    for case in corpus_cases() {
        let profile = classify_path(Path::new(&case.path));
        assert_eq!(profile.planned_kind(), case.kind, "kind mismatch for {}", case.path);
        assert_eq!(
            profile.planned_sub_kind().map(str::to_string),
            case.sub_kind,
            "sub kind mismatch for {}",
            case.path
        );
        assert_eq!(
            profile
                .default_surfaces()
                .iter()
                .map(|surface| (*surface).to_string())
                .collect::<Vec<_>>(),
            case.default_surfaces.clone().unwrap_or_else(|| case.surfaces.clone()),
            "default surface mismatch for {}",
            case.path
        );
    }
}

#[test]
fn test_legacy_classification_adapter_still_covers_public_cases() {
    assert!(matches!(
        classify_file(Path::new("examples/demo.rs")),
        ChangeKind::Example
    ));
    assert!(matches!(classify_file(Path::new("build.rs")), ChangeKind::BuildScript));
    assert!(matches!(
        classify_file(Path::new("Cargo.lock")),
        ChangeKind::Config {
            kind: ConfigKind::CargoLock
        }
    ));
}
