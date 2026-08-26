//! Phase 0 ownership inventory checks for the planner replacement.

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::BTreeSet;

const INVENTORY: &str = include_str!("../fixtures/plan/work-inventory-v1.json");
const JUSTFILE: &str = include_str!("../../justfile");
const COMPATIBILITY_MANIFEST: &str = include_str!("../compatibility/manifest.json");
const RELEASE_TARGETS: &str = include_str!("../../distribution/release-targets.json");

fn string_set(values: &Value) -> Result<BTreeSet<String>> {
    values
        .as_array()
        .context("expected JSON array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("expected string array member")
        })
        .collect()
}

fn just_recipe_names() -> BTreeSet<String> {
    JUSTFILE
        .lines()
        .filter_map(|line| {
            if line.is_empty() || line.starts_with([' ', '\t', '#']) {
                return None;
            }
            let head = line.strip_suffix(':')?;
            let name = head.split_whitespace().next()?;
            name.chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
                .then(|| name.to_owned())
        })
        .collect()
}

#[test]
fn test_plan_work_inventory_covers_every_recipe_and_consumer() {
    let result: Result<()> = (|| {
        let inventory: Value = serde_json::from_str(INVENTORY)?;
        ensure!(inventory["schema_version"] == 1);
        ensure!(inventory["role"] == "phase0_baseline_audit_only");

        let work_ids: BTreeSet<String> = inventory["work"]
            .as_array()
            .context("work must be an array")?
            .iter()
            .map(|work| {
                work["id"]
                    .as_str()
                    .map(str::to_owned)
                    .context("work item must have a string id")
            })
            .collect::<Result<_>>()?;
        ensure!(work_ids.len() == inventory["work"].as_array().unwrap().len());

        let mut inventoried_recipes = BTreeSet::new();
        for recipes in inventory["recipe_classes"]
            .as_object()
            .context("recipe_classes must be an object")?
            .values()
        {
            for recipe in string_set(recipes)? {
                ensure!(
                    inventoried_recipes.insert(recipe.clone()),
                    "recipe '{recipe}' is classified more than once"
                );
            }
        }
        ensure!(
            inventoried_recipes == just_recipe_names(),
            "Phase 0 recipe inventory drifted from justfile"
        );

        for collection in ["quality_operations", "commit_jobs", "recipe_consumers"] {
            for consumer in inventory[collection]
                .as_array()
                .with_context(|| format!("{collection} must be an array"))?
            {
                for work in string_set(&consumer["work"])? {
                    ensure!(
                        work_ids.contains(&work),
                        "{collection} references unknown work id '{work}'"
                    );
                }
            }
        }

        for path in inventory["source_paths"]
            .as_array()
            .context("source_paths must be an array")?
        {
            let path = path.as_str().context("source path must be a string")?;
            ensure!(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path).exists(),
                "inventoried source path '{path}' does not exist"
            );
        }
        Ok(())
    })();
    super::helpers::finish_test(result);
}

#[test]
fn test_plan_work_inventory_matches_authoritative_variant_rows() {
    let result: Result<()> = (|| {
        let inventory: Value = serde_json::from_str(INVENTORY)?;
        let compatibility: Value = serde_json::from_str(COMPATIBILITY_MANIFEST)?;
        let release_targets: Value = serde_json::from_str(RELEASE_TARGETS)?;
        let variants = &inventory["variant_families"];

        let native_ci: BTreeSet<String> = compatibility["native_hosts"]
            .as_array()
            .context("native_hosts must be an array")?
            .iter()
            .filter(|row| row["qualification"] == "ci")
            .map(|row| {
                row["target"]
                    .as_str()
                    .map(str::to_owned)
                    .context("native host target must be a string")
            })
            .collect::<Result<_>>()?;
        ensure!(native_ci == string_set(&variants["compatibility_native_ci_targets"])?);

        let filesystems: BTreeSet<String> = compatibility["filesystem_profiles"]
            .as_array()
            .context("filesystem_profiles must be an array")?
            .iter()
            .map(|row| {
                row["name"]
                    .as_str()
                    .map(str::to_owned)
                    .context("filesystem profile name must be a string")
            })
            .collect::<Result<_>>()?;
        ensure!(filesystems == string_set(&variants["compatibility_filesystems"])?);

        let all_release_targets: BTreeSet<String> = release_targets
            .as_array()
            .context("release targets must be an array")?
            .iter()
            .map(|row| {
                row["target"]
                    .as_str()
                    .map(str::to_owned)
                    .context("release target must be a string")
            })
            .collect::<Result<_>>()?;
        ensure!(all_release_targets == string_set(&variants["release_archive_release_targets"])?);

        let commit_release_targets: BTreeSet<String> = release_targets
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["commit_ci"] == true)
            .map(|row| row["target"].as_str().unwrap().to_owned())
            .collect();
        ensure!(commit_release_targets == string_set(&variants["release_archive_commit_targets"])?);
        Ok(())
    })();
    super::helpers::finish_test(result);
}
