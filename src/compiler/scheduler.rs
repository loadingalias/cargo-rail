//! Pure compiler-fact analysis view scheduling.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use serde::{Deserialize, Serialize};

use crate::cargo::manifest_analyzer::{DepKind, ParsedManifest};
use crate::compiler::cfg_eval::{TargetCfgSet, cfg_expression_may_apply, feature_selections_for_cfg};
use crate::compiler::facts::CompilerFactDomain;
use crate::compiler::model::{CoverageView, FeatureSelection, PlatformTarget};
use crate::error::{RailError, RailResult};

/// One exact rustc crate candidate and the platforms where its declaration applies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompilerCandidate {
    /// Workspace member owning the declaration.
    pub member: String,
    /// rustc crate name passed through `--extern`.
    pub crate_name: String,
    /// Dependency domain whose compilation units provide evidence.
    pub kind: DepKind,
    /// Configured platform targets where this declaration applies.
    pub applicable_targets: BTreeSet<String>,
    /// Restrict evidence to one feature mode (used for optional activation proofs).
    pub required_features: Option<FeatureSelection>,
}

/// Compiler-owned fact protocol required from one analysis view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum CompilerFactFamily {
    /// Stable Cargo and rustc diagnostic evidence.
    StableDiagnostics,
    /// Typed Rust item and relationship fragments from a matched driver.
    TypedRustItems,
}

/// Fixed Cargo acquisition shape for one normalized analysis view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum AnalysisAcquisition {
    /// Compile every ordinary Cargo target without executing it.
    CheckAllTargets,
    /// Ask rustdoc to compile documentation tests without executing them.
    CompileDoctests,
}

/// One normalized Cargo analysis view.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AnalysisView {
    acquisition: AnalysisAcquisition,
    platform: PlatformTarget,
    features: FeatureSelection,
    packages: BTreeSet<String>,
    domains: BTreeSet<CompilerFactDomain>,
    fact_families: BTreeSet<CompilerFactFamily>,
}

impl AnalysisView {
    pub(crate) fn command_class(&self) -> &'static str {
        match self.acquisition {
            AnalysisAcquisition::CheckAllTargets => "cargo-check-all-targets",
            AnalysisAcquisition::CompileDoctests => "cargo-test-doc-no-run",
        }
    }

    pub(crate) fn platform(&self) -> &PlatformTarget {
        &self.platform
    }

    pub(crate) fn features(&self) -> &FeatureSelection {
        &self.features
    }

    pub(crate) fn packages(&self) -> &BTreeSet<String> {
        &self.packages
    }

    pub(crate) fn fact_families(&self) -> &BTreeSet<CompilerFactFamily> {
        &self.fact_families
    }

    pub(crate) fn compiles_doctests(&self) -> bool {
        self.acquisition == AnalysisAcquisition::CompileDoctests
    }

    /// Root-independent typed-fact identity for the exact Cargo package set.
    ///
    /// Diagnostics are deliberately excluded: adding a stable diagnostic
    /// consumer cannot invalidate otherwise identical compiler-owned facts.
    pub(crate) fn fact_cache_identity(
        &self,
        cargo_members: &[&str],
        typed_members: &BTreeSet<String>,
    ) -> RailResult<String> {
        let selected = cargo_members.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != cargo_members.len()
            || selected.iter().any(|member| !self.packages.contains(*member))
            || typed_members.is_empty()
            || typed_members.iter().any(|member| !selected.contains(member.as_str()))
        {
            return Err(RailError::message(
                "compiler fact cache identity contains duplicate, empty, or unscheduled packages",
            ));
        }
        let bytes = serde_json::to_vec(&(
            self.acquisition,
            &self.platform,
            &self.features,
            selected,
            typed_members,
        ))?;
        Ok(format!(
            "{}{}",
            crate::compiler::facts::VIEW_IDENTITY_PREFIX,
            crate::source::ContentDigest::sha256(&bytes)
        ))
    }

    /// Produce the fixed Cargo argv for an executable subset of this view.
    pub(crate) fn cargo_arguments(&self, members: &[&str]) -> RailResult<Vec<OsString>> {
        let selected = members.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != members.len() {
            return Err(RailError::message(
                "compiler analysis package selection contains duplicates",
            ));
        }
        if selected.is_empty() || selected.iter().any(|member| !self.packages.contains(*member)) {
            return Err(RailError::message(
                "compiler analysis package selection is empty or outside its scheduled view",
            ));
        }
        if self.acquisition == AnalysisAcquisition::CompileDoctests && selected.len() != 1 {
            return Err(RailError::message(
                "compile-only doctest acquisition requires exactly one Cargo package",
            ));
        }

        let mut arguments: Vec<OsString> = match self.acquisition {
            AnalysisAcquisition::CheckAllTargets => vec![
                "check".into(),
                "--locked".into(),
                "--all-targets".into(),
                "--message-format=json".into(),
            ],
            AnalysisAcquisition::CompileDoctests => vec![
                "test".into(),
                "--locked".into(),
                "--doc".into(),
                "--message-format=json".into(),
            ],
        };
        match &self.features {
            FeatureSelection::Default => {}
            FeatureSelection::DefaultWith(features) => {
                for member in &selected {
                    for feature in features {
                        arguments.push("--features".into());
                        arguments.push(format!("{member}/{feature}").into());
                    }
                }
            }
            FeatureSelection::NoDefaultFeatures => arguments.push("--no-default-features".into()),
            FeatureSelection::AllFeatures => arguments.push("--all-features".into()),
            FeatureSelection::Selected(features) => {
                arguments.push("--no-default-features".into());
                for member in &selected {
                    for feature in features {
                        arguments.push("--features".into());
                        arguments.push(format!("{member}/{feature}").into());
                    }
                }
            }
        }
        for member in selected {
            arguments.push("--package".into());
            arguments.push(member.into());
        }
        if self.platform.as_str() != "default" {
            arguments.push("--target".into());
            arguments.push(self.platform.as_str().into());
        }
        Ok(arguments)
    }
}

/// Deterministically ordered minimum analysis views for one set of fact requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisSchedule {
    packages: BTreeSet<String>,
    views: Vec<AnalysisView>,
}

impl AnalysisSchedule {
    /// Derive stable-diagnostics views without filesystem access or subprocesses.
    #[cfg(test)]
    pub(crate) fn for_diagnostics(
        manifests: &[ParsedManifest],
        targets: &[&str],
        candidates: &[CompilerCandidate],
    ) -> RailResult<Self> {
        Self::for_combined(manifests, targets, candidates, &BTreeSet::new(), &BTreeSet::new())
    }

    /// Derive and collapse stable-diagnostic plus typed-item acquisitions.
    ///
    /// `typed_packages` names captured workspace packages that need ordinary
    /// target facts. `doctest_packages` is its exact subset whose library
    /// targets enable doctests in Cargo metadata.
    #[cfg(test)]
    pub(crate) fn for_combined(
        manifests: &[ParsedManifest],
        targets: &[&str],
        candidates: &[CompilerCandidate],
        typed_packages: &BTreeSet<String>,
        doctest_packages: &BTreeSet<String>,
    ) -> RailResult<Self> {
        Self::for_combined_with_features(manifests, targets, candidates, typed_packages, doctest_packages, None)
    }

    /// Derive combined acquisitions with an optional exact feature-profile matrix.
    pub(crate) fn for_combined_with_features(
        manifests: &[ParsedManifest],
        targets: &[&str],
        candidates: &[CompilerCandidate],
        typed_packages: &BTreeSet<String>,
        doctest_packages: &BTreeSet<String>,
        explicit_features: Option<&[FeatureSelection]>,
    ) -> RailResult<Self> {
        if explicit_features.is_some_and(<[FeatureSelection]>::is_empty) {
            return Err(RailError::message("explicit compiler feature profile set is empty"));
        }
        let configured_targets = targets.iter().copied().collect::<BTreeSet<_>>();
        let mut manifests_by_name = BTreeMap::new();
        for manifest in manifests {
            if manifests_by_name
                .insert(manifest.package_name.as_str(), manifest)
                .is_some()
            {
                return Err(RailError::message(format!(
                    "compiler analysis package name '{}' is ambiguous",
                    manifest.package_name
                )));
            }
        }
        if let Some(package) = typed_packages
            .iter()
            .find(|package| !manifests_by_name.contains_key(package.as_str()))
        {
            return Err(RailError::message(format!(
                "typed compiler analysis names unknown workspace package '{package}'"
            )));
        }
        if let Some(package) = doctest_packages
            .iter()
            .find(|package| !typed_packages.contains(package.as_str()))
        {
            return Err(RailError::message(format!(
                "compiler doctest analysis package '{package}' does not request typed ordinary-target facts"
            )));
        }

        let mut candidates_by_member = BTreeMap::<&str, Vec<&CompilerCandidate>>::new();
        for candidate in candidates {
            let Some(manifest) = manifests_by_name.get(candidate.member.as_str()) else {
                return Err(RailError::message(format!(
                    "compiler analysis candidate names unknown workspace package '{}'",
                    candidate.member
                )));
            };
            if let Some(target) = candidate
                .applicable_targets
                .iter()
                .find(|target| !configured_targets.contains(target.as_str()))
            {
                return Err(RailError::message(format!(
                    "compiler analysis candidate for '{}' names unconfigured target '{}'",
                    candidate.member, target
                )));
            }
            let selections = scheduled_feature_selections(manifest, explicit_features);
            if let Some(required) = &candidate.required_features
                && !candidate.applicable_targets.is_empty()
                && !selections.contains(required)
            {
                return Err(RailError::message(format!(
                    "compiler analysis candidate for '{}' requires unscheduled feature view '{}'",
                    candidate.member,
                    required.label()
                )));
            }
            candidates_by_member
                .entry(candidate.member.as_str())
                .or_default()
                .push(candidate);
        }

        #[derive(Default)]
        struct ViewAccumulator {
            packages: BTreeSet<String>,
            domains: BTreeSet<CompilerFactDomain>,
            fact_families: BTreeSet<CompilerFactFamily>,
        }

        let mut normalized =
            BTreeMap::<(AnalysisAcquisition, PlatformTarget, FeatureSelection, Option<String>), ViewAccumulator>::new();
        for (member_name, member_candidates) in candidates_by_member {
            let manifest = manifests_by_name[member_name];
            for features in scheduled_feature_selections(manifest, explicit_features) {
                for target in &configured_targets {
                    let applicable = member_candidates.iter().filter(|candidate| {
                        candidate.applicable_targets.contains(*target)
                            && candidate
                                .required_features
                                .as_ref()
                                .is_none_or(|required| required == &features)
                    });
                    let domains = applicable
                        .map(|candidate| dependency_domain(candidate.kind))
                        .collect::<BTreeSet<_>>();
                    if domains.is_empty() {
                        continue;
                    }
                    let view = normalized
                        .entry((
                            AnalysisAcquisition::CheckAllTargets,
                            PlatformTarget::from(*target),
                            features.clone(),
                            None,
                        ))
                        .or_default();
                    view.packages.insert(member_name.to_string());
                    view.domains.extend(domains);
                    view.fact_families.insert(CompilerFactFamily::StableDiagnostics);
                }
            }
        }

        for package in typed_packages {
            let manifest = manifests_by_name[package.as_str()];
            for features in scheduled_feature_selections(manifest, explicit_features) {
                for target in &configured_targets {
                    let view = normalized
                        .entry((
                            AnalysisAcquisition::CheckAllTargets,
                            PlatformTarget::from(*target),
                            features.clone(),
                            None,
                        ))
                        .or_default();
                    view.packages.insert(package.clone());
                    view.domains.extend([
                        CompilerFactDomain::Production,
                        CompilerFactDomain::NonProduction,
                        CompilerFactDomain::BuildScript,
                        CompilerFactDomain::ProcMacro,
                    ]);
                    view.fact_families.insert(CompilerFactFamily::TypedRustItems);

                    if doctest_packages.contains(package) {
                        // Cargo's doctest front door accepts one selected package, so equivalent
                        // package views cannot be collapsed into one Cargo invocation.
                        let doctest = normalized
                            .entry((
                                AnalysisAcquisition::CompileDoctests,
                                PlatformTarget::from(*target),
                                features.clone(),
                                Some(package.clone()),
                            ))
                            .or_default();
                        doctest.packages.insert(package.clone());
                        doctest.domains.insert(CompilerFactDomain::Doctest);
                        doctest.fact_families.insert(CompilerFactFamily::TypedRustItems);
                    }
                }
            }
        }

        let views = normalized
            .into_iter()
            .map(|((acquisition, platform, features, _package), view)| AnalysisView {
                acquisition,
                platform,
                features,
                packages: view.packages,
                domains: view.domains,
                fact_families: view.fact_families,
            })
            .collect::<Vec<_>>();
        let packages = views.iter().flat_map(|view| view.packages.iter().cloned()).collect();
        Ok(Self { packages, views })
    }

    pub(crate) fn packages(&self) -> &BTreeSet<String> {
        &self.packages
    }

    pub(crate) fn views(&self) -> &[AnalysisView] {
        &self.views
    }
}

fn scheduled_feature_selections(
    manifest: &ParsedManifest,
    explicit: Option<&[FeatureSelection]>,
) -> Vec<FeatureSelection> {
    let Some(explicit) = explicit else {
        return planned_feature_selections(manifest);
    };
    explicit
        .iter()
        .map(|selection| match selection {
            FeatureSelection::Selected(features) => {
                let features = features
                    .iter()
                    .filter(|feature| manifest.declared_features.contains(*feature))
                    .cloned()
                    .collect::<Vec<_>>();
                if features.is_empty() {
                    FeatureSelection::NoDefaultFeatures
                } else {
                    FeatureSelection::Selected(features)
                }
            }
            FeatureSelection::DefaultWith(features) => {
                let features = features
                    .iter()
                    .filter(|feature| manifest.declared_features.contains(*feature))
                    .cloned()
                    .collect::<Vec<_>>();
                if features.is_empty() {
                    FeatureSelection::Default
                } else {
                    FeatureSelection::DefaultWith(features)
                }
            }
            other => other.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn dependency_domain(kind: DepKind) -> CompilerFactDomain {
    match kind {
        DepKind::Normal => CompilerFactDomain::Production,
        DepKind::Dev => CompilerFactDomain::NonProduction,
        DepKind::Build => CompilerFactDomain::BuildScript,
    }
}

pub(crate) fn planned_feature_selections(member: &ParsedManifest) -> Vec<FeatureSelection> {
    let has_optional_dependency = member
        .dependencies
        .values()
        .flatten()
        .any(|dependency| dependency.optional);
    let mut selections = if member.declared_features.is_empty() && !has_optional_dependency {
        vec![FeatureSelection::Default]
    } else {
        FeatureSelection::BASELINES.to_vec()
    };
    selections.extend(
        member
            .condition_feature_selections
            .iter()
            .filter(|selected| {
                selected
                    .iter()
                    .all(|feature| member.declared_features.contains(feature))
            })
            .cloned()
            .map(FeatureSelection::Selected),
    );
    selections.extend(
        member
            .required_feature_selections
            .iter()
            .cloned()
            .map(FeatureSelection::Selected),
    );
    selections.sort();
    selections.dedup();
    selections
}

/// Derive root-independent coverage views from the same feature scheduler used
/// by compiler-fact acquisition.
pub(crate) fn planned_coverage_views(
    manifests: &[ParsedManifest],
    targets: &[&str],
    target_cfg_sets: &std::collections::HashMap<String, TargetCfgSet>,
) -> RailResult<Vec<CoverageView>> {
    let packages = manifests
        .iter()
        .map(|manifest| manifest.package_name.clone())
        .collect::<BTreeSet<_>>();
    if packages.len() != manifests.len() {
        return Err(RailError::message(
            "coverage scheduling requires unique workspace package names",
        ));
    }

    let mut grouped = BTreeMap::<(PlatformTarget, FeatureSelection), BTreeSet<String>>::new();
    for target in targets.iter().copied().collect::<BTreeSet<_>>() {
        let cfg_set = target_cfg_sets.get(target);
        for manifest in manifests {
            for selection in coverage_feature_selections(manifest, cfg_set) {
                grouped
                    .entry((PlatformTarget::from(target), selection))
                    .or_default()
                    .insert(manifest.package_name.clone());
            }
        }
    }

    Ok(grouped
        .into_iter()
        .map(|((target, features), packages)| CoverageView::new(target, features, packages))
        .collect())
}

fn coverage_feature_selections(member: &ParsedManifest, cfg_set: Option<&TargetCfgSet>) -> Vec<FeatureSelection> {
    let has_optional_dependency = member
        .dependencies
        .values()
        .flatten()
        .any(|dependency| dependency.optional);
    let mut selections = if member.declared_features.is_empty() && !has_optional_dependency {
        vec![FeatureSelection::Default]
    } else {
        FeatureSelection::BASELINES.to_vec()
    };
    selections.extend(
        member
            .source_cfg_expressions
            .iter()
            .filter(|expression| cfg_expression_may_apply(expression, std::iter::once(cfg_set)))
            .flat_map(|expression| feature_selections_for_cfg(expression))
            .filter(|selected| {
                !selected.is_empty()
                    && selected
                        .iter()
                        .all(|feature| member.declared_features.contains(feature))
            })
            .map(FeatureSelection::Selected),
    );
    selections.extend(
        member
            .required_feature_selections
            .iter()
            .cloned()
            .map(FeatureSelection::Selected),
    );
    selections.sort();
    selections.dedup();
    selections
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use cargo_metadata::PackageId;

    use super::*;

    fn manifest(name: &str, root: &str) -> ParsedManifest {
        ParsedManifest {
            package_id: PackageId {
                repr: format!("path+file:///{root}#{name}@0.1.0"),
            },
            path: PathBuf::from(root).join("Cargo.toml"),
            package_name: name.to_string(),
            declared_features: BTreeSet::from(["backend".to_string(), "common".to_string(), "example".to_string()]),
            required_feature_selections: BTreeSet::from([vec!["example".to_string()]]),
            condition_feature_selections: BTreeSet::from([vec!["backend".to_string(), "common".to_string()]]),
            source_cfg_features: BTreeSet::from(["backend".to_string(), "common".to_string()]),
            source_cfg_expressions: BTreeSet::from([r#"all(feature = "backend", feature = "common")"#.to_string()]),
            retention_identifiers: BTreeSet::new(),
            inherits_workspace_msrv: false,
            package_fields: BTreeMap::new(),
            dependencies: HashMap::new(),
        }
    }

    fn candidate(member: &str, targets: &[&str]) -> CompilerCandidate {
        CompilerCandidate {
            member: member.to_string(),
            crate_name: "dependency".to_string(),
            kind: DepKind::Normal,
            applicable_targets: targets.iter().map(|target| (*target).to_string()).collect(),
            required_features: None,
        }
    }

    #[test]
    fn schedule_is_root_independent_and_collapses_duplicate_requirements() {
        let targets = ["default", "x86_64-unknown-linux-gnu"];
        let left = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "first/root/app")],
            &targets,
            &[candidate("app", &targets), candidate("app", &targets)],
        )
        .expect("left schedule");
        let right = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "another/root/app")],
            &[targets[1], targets[0]],
            &[candidate("app", &targets)],
        )
        .expect("right schedule");

        assert_eq!(left, right);
        assert_eq!(left.views().len(), 10);
        assert!(left.views().iter().all(|view| {
            view.packages() == &BTreeSet::from(["app".to_string()])
                && view.fact_families() == &BTreeSet::from([CompilerFactFamily::StableDiagnostics])
        }));
    }

    #[test]
    fn selected_feature_view_emits_fixed_package_qualified_argv() {
        let view = AnalysisView {
            acquisition: AnalysisAcquisition::CheckAllTargets,
            platform: PlatformTarget::from("x86_64-unknown-linux-gnu"),
            features: FeatureSelection::Selected(vec!["backend".to_string(), "common".to_string()]),
            packages: BTreeSet::from(["app".to_string(), "worker".to_string()]),
            domains: BTreeSet::from([CompilerFactDomain::Production]),
            fact_families: BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
        };

        let arguments = view
            .cargo_arguments(&["worker", "app"])
            .expect("scheduled package subset");
        assert_eq!(
            arguments,
            [
                "check",
                "--locked",
                "--all-targets",
                "--message-format=json",
                "--no-default-features",
                "--features",
                "app/backend",
                "--features",
                "app/common",
                "--features",
                "worker/backend",
                "--features",
                "worker/common",
                "--package",
                "app",
                "--package",
                "worker",
                "--target",
                "x86_64-unknown-linux-gnu",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn coverage_views_are_root_independent_and_emit_non_shell_argv() {
        let targets = ["x86_64-unknown-linux-gnu"];
        let target_cfg_sets = HashMap::from([(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetCfgSet::from_test_lines(&[r#"target_os="linux""#]),
        )]);
        let left = planned_coverage_views(
            &[
                manifest("app", "first/root/app"),
                manifest("worker", "first/root/worker"),
            ],
            &targets,
            &target_cfg_sets,
        )
        .expect("left coverage");
        let right = planned_coverage_views(
            &[
                manifest("worker", "another/root/worker"),
                manifest("app", "another/root/app"),
            ],
            &targets,
            &target_cfg_sets,
        )
        .expect("right coverage");

        assert_eq!(left, right);
        assert_eq!(left.len(), 5);
        assert!(
            left.iter()
                .all(|view| view.packages == ["app".to_string(), "worker".to_string()])
        );
        let selected = left
      .iter()
      .find(
        |view| matches!(view.features, FeatureSelection::Selected(ref features) if features == &["backend", "common"]),
      )
      .expect("condition-derived coverage view");
        assert_eq!(
            selected.cargo_arguments(),
            [
                "check",
                "--locked",
                "--all-targets",
                "--no-default-features",
                "--features",
                "app/backend",
                "--features",
                "app/common",
                "--features",
                "worker/backend",
                "--features",
                "worker/common",
                "--package",
                "app",
                "--package",
                "worker",
                "--target",
                "x86_64-unknown-linux-gnu",
            ]
        );
    }

    #[test]
    fn combined_schedule_shares_identical_check_runs_but_keeps_doctests_distinct() {
        let manifests = [manifest("app", "app"), manifest("worker", "worker")];
        let typed = BTreeSet::from(["app".to_string(), "worker".to_string()]);
        let doctests = BTreeSet::from(["app".to_string()]);
        let schedule = AnalysisSchedule::for_combined(
            &manifests,
            &["default"],
            &[candidate("app", &["default"])],
            &typed,
            &doctests,
        )
        .expect("combined schedule");

        assert_eq!(schedule.views().len(), 10);
        let check_views = schedule
            .views()
            .iter()
            .filter(|view| view.acquisition == AnalysisAcquisition::CheckAllTargets)
            .collect::<Vec<_>>();
        assert_eq!(
            check_views.len(),
            5,
            "diagnostics and typed facts must share each check view"
        );
        assert!(check_views.iter().all(|view| {
            view.packages() == &typed
                && view.fact_families()
                    == &BTreeSet::from([
                        CompilerFactFamily::StableDiagnostics,
                        CompilerFactFamily::TypedRustItems,
                    ])
        }));

        let doctest_views = schedule
            .views()
            .iter()
            .filter(|view| view.acquisition == AnalysisAcquisition::CompileDoctests)
            .collect::<Vec<_>>();
        assert_eq!(doctest_views.len(), 5);
        assert!(doctest_views.iter().all(|view| {
            view.packages() == &doctests
                && view.fact_families() == &BTreeSet::from([CompilerFactFamily::TypedRustItems])
        }));
        assert_eq!(
            doctest_views[0].cargo_arguments(&["app"]).expect("doctest argv"),
            ["test", "--locked", "--doc", "--message-format=json", "--package", "app",]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn combined_schedule_rejects_unknown_or_untyped_doctest_packages() {
        let manifests = [manifest("app", "app")];
        AnalysisSchedule::for_combined(
            &manifests,
            &["default"],
            &[],
            &BTreeSet::from(["unknown".to_string()]),
            &BTreeSet::new(),
        )
        .unwrap_err();
        AnalysisSchedule::for_combined(
            &manifests,
            &["default"],
            &[],
            &BTreeSet::new(),
            &BTreeSet::from(["app".to_string()]),
        )
        .unwrap_err();
    }

    #[test]
    fn explicit_feature_profiles_replace_automatic_views_without_losing_default_semantics() {
        let manifests = [manifest("app", "app"), manifest("worker", "worker")];
        let typed = BTreeSet::from(["app".to_string(), "worker".to_string()]);
        let profiles = [
            FeatureSelection::DefaultWith(vec!["backend".to_string()]),
            FeatureSelection::Selected(vec!["example".to_string()]),
            FeatureSelection::AllFeatures,
        ];
        let schedule = AnalysisSchedule::for_combined_with_features(
            &manifests,
            &["default"],
            &[],
            &typed,
            &BTreeSet::from(["app".to_string()]),
            Some(&profiles),
        )
        .expect("explicit feature schedule");

        assert_eq!(schedule.views().len(), 6);
        assert_eq!(
            schedule
                .views()
                .iter()
                .filter(|view| view.acquisition == AnalysisAcquisition::CheckAllTargets)
                .map(|view| view.features().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(profiles)
        );
        let default_with = schedule
            .views()
            .iter()
            .find(|view| view.features() == &FeatureSelection::DefaultWith(vec!["backend".to_string()]))
            .expect("default-plus-selected view");
        let arguments = default_with
            .cargo_arguments(&["app", "worker"])
            .expect("default-plus-selected argv");
        assert!(!arguments.contains(&OsString::from("--no-default-features")));
        assert!(arguments.windows(2).any(|pair| pair == ["--features", "app/backend"]));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--features", "worker/backend"])
        );
    }

    #[test]
    fn featureless_packages_have_one_semantic_feature_selection() {
        let mut app = manifest("app", "app");
        app.declared_features.clear();
        app.required_feature_selections.clear();
        app.condition_feature_selections.clear();

        assert_eq!(planned_feature_selections(&app), vec![FeatureSelection::Default]);

        app.dependencies
            .entry(crate::cargo::manifest_analyzer::DepKey::new("optional-dep"))
            .or_default()
            .push(crate::cargo::manifest_analyzer::DepUsage {
                unconditional_features: BTreeSet::new(),
                local_features: BTreeSet::new(),
                conditional_features: BTreeSet::new(),
                default_features: true,
                kind: DepKind::Normal,
                target: None,
                used_by: "app".into(),
                optional: true,
                path: None,
                declared_version: None,
                manifest_path: None,
                cargo_toml_key: "optional-dep".into(),
                referenced_in_features: false,
                workspace_inherited: false,
            });
        assert_eq!(planned_feature_selections(&app), FeatureSelection::BASELINES);
    }

    #[test]
    fn schedule_rejects_a_candidate_outside_the_configured_target_matrix() {
        let error = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "app")],
            &["default"],
            &[candidate("app", &["aarch64-unknown-linux-gnu"])],
        )
        .expect_err("unconfigured target must fail");
        assert!(error.to_string().contains("unconfigured target"), "{error}");
    }
}
