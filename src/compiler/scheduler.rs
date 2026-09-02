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
    pub(crate) fn platform(&self) -> &PlatformTarget {
        &self.platform
    }

    pub(crate) fn features(&self) -> &FeatureSelection {
        &self.features
    }

    pub(crate) fn packages(&self) -> &BTreeSet<String> {
        &self.packages
    }

    #[cfg(test)]
    pub(crate) fn fact_families(&self) -> &BTreeSet<CompilerFactFamily> {
        &self.fact_families
    }

    /// Root-independent typed-fact identity for the exact Cargo package set.
    ///
    /// Diagnostics are deliberately excluded: adding a stable diagnostic
    /// consumer cannot invalidate otherwise identical compiler-owned facts.
    #[cfg(test)]
    pub(crate) fn fact_cache_identity(
        &self,
        cargo_members: &[&str],
        typed_members: &BTreeSet<String>,
    ) -> RailResult<String> {
        let selected = cargo_members.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != 1
            || cargo_members.len() != 1
            || selected.iter().any(|member| !self.packages.contains(*member))
            || typed_members.is_empty()
            || typed_members.iter().any(|member| !selected.contains(member.as_str()))
        {
            return Err(RailError::message(
                "compiler fact cache identity requires one scheduled package",
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
    #[cfg(test)]
    pub(crate) fn cargo_arguments(&self, members: &[&str]) -> RailResult<Vec<OsString>> {
        let selected = members.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != 1 || members.len() != 1 || selected.iter().any(|member| !self.packages.contains(*member)) {
            return Err(RailError::message(
                "compiler analysis execution requires one scheduled package",
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

        // Cargo unifies dependency features across every selected package in one
        // invocation. A multi-package acquisition can therefore make one root's
        // `std` edge leak into another root's `alloc`-only view and authenticate
        // evidence for a feature shape Cargo never compiled in isolation.
        // Package identity is part of the acquisition key; diagnostics and typed
        // facts for the same package still share their exact Cargo run.
        let mut normalized =
            BTreeMap::<(AnalysisAcquisition, PlatformTarget, FeatureSelection, String), ViewAccumulator>::new();
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
                            member_name.to_string(),
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
                            package.clone(),
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
                                package.clone(),
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

macro_rules! acquisition_index {
    ($name:ident, $label:literal) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub(crate) struct $name(u32);

        impl $name {
            pub(crate) fn checked(index: usize) -> RailResult<Self> {
                u32::try_from(index).map(Self).map_err(|_| {
                    RailError::message(concat!(
                        "compiler acquisition plan exceeds the representable ",
                        $label,
                        " index space"
                    ))
                })
            }

            pub(crate) fn offset(self) -> usize {
                self.0 as usize
            }
        }
    };
}

acquisition_index!(PackageIx, "package");
acquisition_index!(TargetIx, "target");
acquisition_index!(FeatureIx, "feature");
acquisition_index!(FeatureSetIx, "feature-set");
acquisition_index!(CandidateNameIx, "candidate-name");
acquisition_index!(CandidateIx, "candidate");
acquisition_index!(CandidateSetIx, "candidate-set");
acquisition_index!(ViewIx, "view");

const COMPILER_ACQUISITION_PLAN_VERSION: u32 = 1;
const COMPILER_ACQUISITION_PLAN_IDENTITY_PREFIX: &str = "compiler-acquisition-plan-v1-sha256-";
const COMPILER_ACQUISITION_CANDIDATE_SET_IDENTITY_PREFIX: &str = "compiler-acquisition-candidates-v1-sha256-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerAcquisitionPlanIdentity(Box<str>);

impl CompilerAcquisitionPlanIdentity {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PackageSpec {
    name: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct TargetSpec {
    triple: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
enum FeatureSetMode {
    Default,
    DefaultWith,
    Selected,
    NoDefaultFeatures,
    AllFeatures,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct FeatureSetSpec {
    mode: FeatureSetMode,
    features: Box<[FeatureIx]>,
}

impl FeatureSetSpec {
    fn from_selection(selection: &FeatureSelection, features: &[Box<str>]) -> RailResult<Self> {
        let (mode, selected): (FeatureSetMode, &[String]) = match selection {
            FeatureSelection::Default => (FeatureSetMode::Default, &[]),
            FeatureSelection::DefaultWith(selected) => (FeatureSetMode::DefaultWith, selected),
            FeatureSelection::Selected(selected) => (FeatureSetMode::Selected, selected),
            FeatureSelection::NoDefaultFeatures => (FeatureSetMode::NoDefaultFeatures, &[]),
            FeatureSelection::AllFeatures => (FeatureSetMode::AllFeatures, &[]),
        };
        let mut selected = selected.iter().map(String::as_str).collect::<Vec<_>>();
        selected.sort_unstable();
        selected.dedup();
        let features = selected
            .into_iter()
            .map(|feature| {
                features
                    .binary_search_by(|candidate| candidate.as_ref().cmp(feature))
                    .map_err(|_| RailError::message("compiler acquisition feature is absent from its intern table"))
                    .and_then(FeatureIx::checked)
            })
            .collect::<RailResult<Box<[_]>>>()?;
        Ok(Self { mode, features })
    }

    fn to_selection(&self, features: &[Box<str>]) -> FeatureSelection {
        let selected = || {
            self.features
                .iter()
                .map(|index| features[index.offset()].to_string())
                .collect::<Vec<_>>()
        };
        match self.mode {
            FeatureSetMode::Default => FeatureSelection::Default,
            FeatureSetMode::DefaultWith => FeatureSelection::DefaultWith(selected()),
            FeatureSetMode::Selected => FeatureSelection::Selected(selected()),
            FeatureSetMode::NoDefaultFeatures => FeatureSelection::NoDefaultFeatures,
            FeatureSetMode::AllFeatures => FeatureSelection::AllFeatures,
        }
    }

    fn append_cargo_arguments(&self, features: &[Box<str>], package: &str, arguments: &mut Vec<OsString>) {
        match self.mode {
            FeatureSetMode::Default => {}
            FeatureSetMode::DefaultWith => {
                for feature in &self.features {
                    arguments.push("--features".into());
                    arguments.push(format!("{package}/{}", features[feature.offset()]).into());
                }
            }
            FeatureSetMode::Selected => {
                arguments.push("--no-default-features".into());
                for feature in &self.features {
                    arguments.push("--features".into());
                    arguments.push(format!("{package}/{}", features[feature.offset()]).into());
                }
            }
            FeatureSetMode::NoDefaultFeatures => arguments.push("--no-default-features".into()),
            FeatureSetMode::AllFeatures => arguments.push("--all-features".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CandidateSpec {
    package: PackageIx,
    name: CandidateNameIx,
    kind: DepKind,
    targets: Box<[TargetIx]>,
    required_features: Option<FeatureSetIx>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CandidateSet {
    members: Box<[CandidateIx]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct FactDomainRequirements(u8);

impl FactDomainRequirements {
    fn from_domains(domains: &BTreeSet<CompilerFactDomain>) -> Self {
        let mut bits = 0_u8;
        for domain in domains {
            bits |= match domain {
                CompilerFactDomain::Production => 1 << 0,
                CompilerFactDomain::NonProduction => 1 << 1,
                CompilerFactDomain::Doctest => 1 << 2,
                CompilerFactDomain::BuildScript => 1 << 3,
                CompilerFactDomain::ProcMacro => 1 << 4,
            };
        }
        Self(bits)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct EvidenceRequirements(u8);

impl EvidenceRequirements {
    fn from_families(families: &BTreeSet<CompilerFactFamily>) -> Self {
        let mut bits = 0_u8;
        for family in families {
            bits |= match family {
                CompilerFactFamily::StableDiagnostics => 1 << 0,
                CompilerFactFamily::TypedRustItems => 1 << 1,
            };
        }
        Self(bits)
    }

    fn contains(self, family: CompilerFactFamily) -> bool {
        let bit = match family {
            CompilerFactFamily::StableDiagnostics => 1 << 0,
            CompilerFactFamily::TypedRustItems => 1 << 1,
        };
        self.0 & bit != 0
    }

    fn to_families(self) -> BTreeSet<CompilerFactFamily> {
        [
            CompilerFactFamily::StableDiagnostics,
            CompilerFactFamily::TypedRustItems,
        ]
        .into_iter()
        .filter(|family| self.contains(*family))
        .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct ViewSpec {
    acquisition: AnalysisAcquisition,
    package: PackageIx,
    target: TargetIx,
    feature_set: FeatureSetIx,
    candidates: CandidateSetIx,
    admission: ViewAdmission,
    domains: FactDomainRequirements,
    evidence: EvidenceRequirements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
enum ViewAdmission {
    Required,
    Waiting(CandidateSetIx),
}

/// Dense, immutable command-lifetime input for compiler acquisition.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct CompilerAcquisitionPlan {
    identity: CompilerAcquisitionPlanIdentity,
    candidate_set_identity: Box<str>,
    packages: Box<[PackageSpec]>,
    targets: Box<[TargetSpec]>,
    features: Box<[Box<str>]>,
    feature_sets: Box<[FeatureSetSpec]>,
    candidate_names: Box<[Box<str>]>,
    candidates: Box<[CandidateSpec]>,
    candidate_sets: Box<[CandidateSet]>,
    views: Box<[ViewSpec]>,
    execution_order: Box<[ViewIx]>,
}

impl CompilerAcquisitionPlan {
    #[cfg(test)]
    pub(crate) fn journal_test_plan(packages: &[&str]) -> RailResult<Self> {
        let package_names = packages
            .iter()
            .map(|package| (*package).to_string())
            .collect::<BTreeSet<_>>();
        let schedule = AnalysisSchedule {
            packages: package_names.clone(),
            views: package_names
                .into_iter()
                .map(|package| AnalysisView {
                    acquisition: AnalysisAcquisition::CheckAllTargets,
                    platform: PlatformTarget::from("default"),
                    features: FeatureSelection::Default,
                    packages: BTreeSet::from([package]),
                    domains: BTreeSet::from([CompilerFactDomain::Production, CompilerFactDomain::NonProduction]),
                    fact_families: BTreeSet::from([CompilerFactFamily::TypedRustItems]),
                })
                .collect(),
        };
        Self::from_schedule(&schedule, &[], &["default"])
    }

    /// Freeze one typed schedule into canonical dense tables before any worker starts.
    pub(crate) fn from_schedule(
        schedule: &AnalysisSchedule,
        candidates: &[CompilerCandidate],
        compiler_targets: &[&str],
    ) -> RailResult<Self> {
        let packages = schedule
            .packages()
            .iter()
            .map(|name| PackageSpec {
                name: name.clone().into_boxed_str(),
            })
            .collect::<Box<[_]>>();
        ensure_indexable(packages.len(), "package")?;

        let configured_targets = compiler_targets.iter().copied().collect::<BTreeSet<_>>();
        let targets = schedule
            .views()
            .iter()
            .map(|view| view.platform().as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|triple| TargetSpec { triple: triple.into() })
            .collect::<Box<[_]>>();
        ensure_indexable(targets.len(), "target")?;
        if let Some(target) = targets
            .iter()
            .find(|target| !configured_targets.contains(target.triple.as_ref()))
        {
            return Err(RailError::message(format!(
                "compiler acquisition view names unconfigured target '{}'",
                target.triple
            )));
        }

        let features = schedule
            .views()
            .iter()
            .flat_map(|view| selected_feature_names(view.features()).iter().cloned())
            .chain(candidates.iter().flat_map(|candidate| {
                candidate
                    .required_features
                    .as_ref()
                    .into_iter()
                    .flat_map(|selection| selected_feature_names(selection).iter().cloned())
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(String::into_boxed_str)
            .collect::<Box<[_]>>();
        ensure_indexable(features.len(), "feature")?;

        let feature_sets = schedule
            .views()
            .iter()
            .map(AnalysisView::features)
            .chain(
                candidates
                    .iter()
                    .filter_map(|candidate| candidate.required_features.as_ref()),
            )
            .map(|selection| FeatureSetSpec::from_selection(selection, &features))
            .collect::<RailResult<BTreeSet<_>>>()?
            .into_iter()
            .collect::<Box<[_]>>();
        ensure_indexable(feature_sets.len(), "feature-set")?;

        let candidate_names = candidates
            .iter()
            .map(|candidate| candidate.crate_name.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(Into::into)
            .collect::<Box<[Box<str>]>>();
        ensure_indexable(candidate_names.len(), "candidate-name")?;

        let mut candidate_specs = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let package = table_index(
                &packages,
                candidate.member.as_str(),
                |package| package.name.as_ref(),
                "package",
            )?;
            let name = table_index(
                &candidate_names,
                candidate.crate_name.as_str(),
                |name| name.as_ref(),
                "candidate-name",
            )?;
            let targets_for_candidate = candidate
                .applicable_targets
                .iter()
                .map(|target| {
                    if !configured_targets.contains(target.as_str()) {
                        return Err(RailError::message(format!(
                            "compiler acquisition candidate for '{}' names unconfigured target '{target}'",
                            candidate.member
                        )));
                    }
                    table_index(&targets, target, |spec| spec.triple.as_ref(), "target")
                })
                .collect::<RailResult<Box<[_]>>>()?;
            let required_features = candidate
                .required_features
                .as_ref()
                .map(|selection| {
                    let spec = FeatureSetSpec::from_selection(selection, &features)?;
                    feature_sets
                        .binary_search(&spec)
                        .map_err(|_| {
                            RailError::message("compiler acquisition candidate feature set is absent from its table")
                        })
                        .and_then(FeatureSetIx::checked)
                })
                .transpose()?;
            candidate_specs.push(CandidateSpec {
                package,
                name,
                kind: candidate.kind,
                targets: targets_for_candidate,
                required_features,
            });
        }
        candidate_specs.sort();
        candidate_specs.dedup();
        ensure_indexable(candidate_specs.len(), "candidate")?;
        let candidates = candidate_specs.into_boxed_slice();

        let mut unresolved_views = Vec::with_capacity(schedule.views().len());
        for view in schedule.views() {
            if view.packages().len() != 1 {
                return Err(RailError::message(
                    "compiler acquisition plan requires exactly one root package per view",
                ));
            }
            let package_name = view
                .packages()
                .first()
                .ok_or_else(|| RailError::message("compiler acquisition view has no root package"))?;
            let package = table_index(&packages, package_name, |package| package.name.as_ref(), "package")?;
            let target = table_index(
                &targets,
                view.platform().as_str(),
                |target| target.triple.as_ref(),
                "target",
            )?;
            let feature_spec = FeatureSetSpec::from_selection(view.features(), &features)?;
            let feature_set = feature_sets
                .binary_search(&feature_spec)
                .map_err(|_| RailError::message("compiler acquisition view feature set is absent from its table"))
                .and_then(FeatureSetIx::checked)?;
            let candidate_members = candidates
                .iter()
                .zip(0_u32..)
                .filter(|(candidate, _)| {
                    candidate.package == package
                        && candidate.targets.binary_search(&target).is_ok()
                        && candidate
                            .required_features
                            .is_none_or(|required| required == feature_set)
                })
                .map(|(_, index)| CandidateIx(index))
                .collect::<Box<[_]>>();
            unresolved_views.push((
                ViewSpec {
                    acquisition: view.acquisition,
                    package,
                    target,
                    feature_set,
                    candidates: CandidateSetIx(0),
                    admission: ViewAdmission::Required,
                    domains: FactDomainRequirements::from_domains(&view.domains),
                    evidence: EvidenceRequirements::from_families(&view.fact_families),
                },
                candidate_members,
            ));
        }

        let candidate_sets = unresolved_views
            .iter()
            .map(|(_, members)| CandidateSet {
                members: members.clone(),
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Box<[_]>>();
        ensure_indexable(candidate_sets.len(), "candidate-set")?;
        let mut views = unresolved_views
            .into_iter()
            .map(|(mut view, members)| {
                view.candidates = candidate_sets
                    .binary_search(&CandidateSet { members })
                    .map_err(|_| RailError::message("compiler acquisition candidate set is absent from its table"))
                    .and_then(CandidateSetIx::checked)?;
                view.admission = if view.evidence.contains(CompilerFactFamily::TypedRustItems) {
                    ViewAdmission::Required
                } else {
                    ViewAdmission::Waiting(view.candidates)
                };
                Ok(view)
            })
            .collect::<RailResult<Vec<_>>>()?;
        views.sort();
        ensure_indexable(views.len(), "view")?;
        let views = views.into_boxed_slice();

        let mut execution_order = views
            .iter()
            .zip(0_u32..)
            .map(|(_, index)| ViewIx(index))
            .collect::<Vec<_>>();
        execution_order.sort_by_key(|index| {
            let view = views[index.offset()];
            (
                view.target,
                view.acquisition == AnalysisAcquisition::CompileDoctests,
                feature_execution_priority(&feature_sets[view.feature_set.offset()]),
                view.feature_set,
                *index,
            )
        });

        #[derive(Serialize)]
        struct CanonicalPlan<'a> {
            version: u32,
            packages: &'a [PackageSpec],
            targets: &'a [TargetSpec],
            features: &'a [Box<str>],
            feature_sets: &'a [FeatureSetSpec],
            candidate_names: &'a [Box<str>],
            candidates: &'a [CandidateSpec],
            candidate_sets: &'a [CandidateSet],
            views: &'a [ViewSpec],
        }
        let canonical_plan = CanonicalPlan {
            version: COMPILER_ACQUISITION_PLAN_VERSION,
            packages: &packages,
            targets: &targets,
            features: &features,
            feature_sets: &feature_sets,
            candidate_names: &candidate_names,
            candidates: &candidates,
            candidate_sets: &candidate_sets,
            views: &views,
        };
        let identity_bytes = serde_json::to_vec(&canonical_plan)?;
        let identity = CompilerAcquisitionPlanIdentity(
            format!(
                "{COMPILER_ACQUISITION_PLAN_IDENTITY_PREFIX}{}",
                crate::source::ContentDigest::sha256(&identity_bytes)
            )
            .into_boxed_str(),
        );
        let candidate_set_identity = format!(
            "{COMPILER_ACQUISITION_CANDIDATE_SET_IDENTITY_PREFIX}{}",
            crate::source::ContentDigest::sha256(&serde_json::to_vec(&(
                COMPILER_ACQUISITION_PLAN_VERSION,
                &candidate_names,
                &candidates,
                &candidate_sets,
            ))?)
        )
        .into_boxed_str();
        let plan = Self {
            identity,
            candidate_set_identity,
            packages,
            targets,
            features,
            feature_sets,
            candidate_names,
            candidates,
            candidate_sets,
            views,
            execution_order: execution_order.into_boxed_slice(),
        };
        plan.validate()?;
        Ok(plan)
    }

    pub(crate) fn identity(&self) -> &CompilerAcquisitionPlanIdentity {
        &self.identity
    }

    pub(crate) fn candidate_set_identity(&self) -> &str {
        &self.candidate_set_identity
    }

    pub(crate) fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub(crate) fn target_count(&self) -> usize {
        self.targets.len()
    }

    pub(crate) fn feature_count(&self) -> usize {
        self.features.len()
    }

    pub(crate) fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn view_count(&self) -> usize {
        self.views.len()
    }

    pub(crate) fn views(&self) -> impl Iterator<Item = CompilerAcquisitionView<'_>> {
        self.views
            .iter()
            .zip(0_u32..)
            .map(|(_, index)| CompilerAcquisitionView {
                plan: self,
                index: ViewIx(index),
            })
    }

    pub(crate) fn execution_order(&self) -> impl ExactSizeIterator<Item = CompilerAcquisitionView<'_>> {
        self.execution_order
            .iter()
            .copied()
            .map(|index| CompilerAcquisitionView { plan: self, index })
    }

    pub(crate) fn view(&self, index: ViewIx) -> RailResult<CompilerAcquisitionView<'_>> {
        self.views
            .get(index.offset())
            .map(|_| CompilerAcquisitionView { plan: self, index })
            .ok_or_else(|| RailError::message("compiler acquisition view index is out of bounds"))
    }

    fn validate(&self) -> RailResult<()> {
        validate_strictly_sorted(&self.packages, "package")?;
        validate_strictly_sorted(&self.targets, "target")?;
        validate_strictly_sorted(&self.features, "feature")?;
        validate_strictly_sorted(&self.feature_sets, "feature-set")?;
        validate_strictly_sorted(&self.candidate_names, "candidate-name")?;
        validate_strictly_sorted(&self.candidates, "candidate")?;
        validate_strictly_sorted(&self.candidate_sets, "candidate-set")?;
        validate_strictly_sorted(&self.views, "view")?;
        for feature_set in &self.feature_sets {
            validate_indices(&feature_set.features, self.features.len(), "feature")?;
        }
        for candidate in &self.candidates {
            validate_index(candidate.package, self.packages.len(), "candidate package")?;
            validate_index(candidate.name, self.candidate_names.len(), "candidate name")?;
            validate_indices(&candidate.targets, self.targets.len(), "candidate target")?;
            if let Some(features) = candidate.required_features {
                validate_index(features, self.feature_sets.len(), "candidate feature-set")?;
            }
        }
        for set in &self.candidate_sets {
            validate_indices(&set.members, self.candidates.len(), "candidate-set member")?;
        }
        for view in &self.views {
            validate_index(view.package, self.packages.len(), "view package")?;
            validate_index(view.target, self.targets.len(), "view target")?;
            validate_index(view.feature_set, self.feature_sets.len(), "view feature-set")?;
            validate_index(view.candidates, self.candidate_sets.len(), "view candidate-set")?;
            if let ViewAdmission::Waiting(condition) = view.admission {
                validate_index(condition, self.candidate_sets.len(), "view admission condition")?;
                if condition != view.candidates || view.evidence.contains(CompilerFactFamily::TypedRustItems) {
                    return Err(RailError::message(
                        "compiler acquisition conditional admission disagrees with its candidate authority",
                    ));
                }
            } else if !view.evidence.contains(CompilerFactFamily::TypedRustItems) {
                return Err(RailError::message(
                    "compiler acquisition required admission has no typed evidence requirement",
                ));
            }
            if view.domains.0 == 0 || view.evidence.0 == 0 {
                return Err(RailError::message(
                    "compiler acquisition view has no compiler domain or evidence requirement",
                ));
            }
            let candidate_set = &self.candidate_sets[view.candidates.offset()];
            for candidate in &candidate_set.members {
                let candidate = &self.candidates[candidate.offset()];
                if candidate.package != view.package
                    || candidate.targets.binary_search(&view.target).is_err()
                    || candidate
                        .required_features
                        .is_some_and(|required| required != view.feature_set)
                {
                    return Err(RailError::message(
                        "compiler acquisition candidate set escapes its view authority",
                    ));
                }
            }
        }
        for index in &self.execution_order {
            validate_index(*index, self.views.len(), "execution-order view")?;
        }
        if self.execution_order.len() != self.views.len()
            || self.execution_order.iter().copied().collect::<BTreeSet<_>>().len() != self.views.len()
        {
            return Err(RailError::message(
                "compiler acquisition execution order is duplicate or incomplete",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompilerAcquisitionView<'a> {
    plan: &'a CompilerAcquisitionPlan,
    index: ViewIx,
}

impl<'plan> CompilerAcquisitionView<'plan> {
    pub(crate) fn index(self) -> ViewIx {
        self.index
    }

    fn spec(self) -> ViewSpec {
        self.plan.views[self.index.offset()]
    }

    pub(crate) fn command_class(self) -> &'static str {
        match self.spec().acquisition {
            AnalysisAcquisition::CheckAllTargets => "cargo-check-all-targets",
            AnalysisAcquisition::CompileDoctests => "cargo-test-doc-no-run",
        }
    }

    pub(crate) fn package(self) -> &'plan str {
        self.plan.packages[self.spec().package.offset()].name.as_ref()
    }

    pub(crate) fn platform(self) -> &'plan str {
        self.plan.targets[self.spec().target.offset()].triple.as_ref()
    }

    pub(crate) fn features(self) -> FeatureSelection {
        self.plan.feature_sets[self.spec().feature_set.offset()].to_selection(&self.plan.features)
    }

    pub(crate) fn requires(self, family: CompilerFactFamily) -> bool {
        self.spec().evidence.contains(family)
    }

    pub(crate) fn fact_families(self) -> BTreeSet<CompilerFactFamily> {
        self.spec().evidence.to_families()
    }

    pub(crate) fn compiles_doctests(self) -> bool {
        self.spec().acquisition == AnalysisAcquisition::CompileDoctests
    }

    pub(crate) fn candidates(self) -> impl ExactSizeIterator<Item = CandidateIx> + 'plan {
        self.plan.candidate_sets[self.spec().candidates.offset()]
            .members
            .iter()
            .copied()
    }

    pub(crate) fn cargo_arguments(self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = match self.spec().acquisition {
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
        self.plan.feature_sets[self.spec().feature_set.offset()].append_cargo_arguments(
            &self.plan.features,
            self.package(),
            &mut arguments,
        );
        arguments.push("--package".into());
        arguments.push(self.package().into());
        if self.platform() != "default" {
            arguments.push("--target".into());
            arguments.push(self.platform().into());
        }
        arguments
    }

    pub(crate) fn fact_cache_identity(self, typed_package: &str) -> RailResult<String> {
        if typed_package != self.package() {
            return Err(RailError::message(
                "compiler fact cache identity requires the acquisition view's root package",
            ));
        }
        let package = BTreeSet::from([self.package()]);
        let typed = BTreeSet::from([typed_package]);
        let bytes = serde_json::to_vec(&(
            self.spec().acquisition,
            PlatformTarget::from(self.platform()),
            self.features(),
            package,
            typed,
        ))?;
        Ok(format!(
            "{}{}",
            crate::compiler::facts::VIEW_IDENTITY_PREFIX,
            crate::source::ContentDigest::sha256(&bytes)
        ))
    }
}

fn selected_feature_names(selection: &FeatureSelection) -> &[String] {
    match selection {
        FeatureSelection::DefaultWith(features) | FeatureSelection::Selected(features) => features,
        FeatureSelection::Default | FeatureSelection::NoDefaultFeatures | FeatureSelection::AllFeatures => &[],
    }
}

fn feature_execution_priority(features: &FeatureSetSpec) -> u8 {
    match features.mode {
        FeatureSetMode::Default => 0,
        FeatureSetMode::NoDefaultFeatures => 1,
        FeatureSetMode::DefaultWith => 2,
        FeatureSetMode::Selected => 3,
        FeatureSetMode::AllFeatures => 4,
    }
}

fn ensure_indexable(len: usize, label: &'static str) -> RailResult<()> {
    u32::try_from(len).map(|_| ()).map_err(|_| {
        RailError::message(format!(
            "compiler acquisition plan exceeds the representable {label} index space"
        ))
    })
}

fn table_index<T, I: acquisition_index::Sealed>(
    table: &[T],
    needle: &str,
    value: impl Fn(&T) -> &str,
    label: &'static str,
) -> RailResult<I> {
    table
        .binary_search_by(|candidate| value(candidate).cmp(needle))
        .map_err(|_| RailError::message(format!("compiler acquisition {label} is absent from its intern table")))
        .and_then(I::checked)
}

mod acquisition_index {
    use super::*;

    pub(super) trait Sealed: Copy {
        fn checked(index: usize) -> RailResult<Self>;
        fn offset(self) -> usize;
    }

    macro_rules! seal {
        ($($index:ty),+ $(,)?) => {$(
            impl Sealed for $index {
                fn checked(index: usize) -> RailResult<Self> {
                    <$index>::checked(index)
                }

                fn offset(self) -> usize {
                    <$index>::offset(self)
                }
            }
        )+};
    }

    seal!(
        PackageIx,
        TargetIx,
        FeatureIx,
        FeatureSetIx,
        CandidateNameIx,
        CandidateIx,
        CandidateSetIx,
        ViewIx,
    );
}

fn validate_index<I: acquisition_index::Sealed>(index: I, len: usize, label: &'static str) -> RailResult<()> {
    if index.offset() < len {
        return Ok(());
    }
    Err(RailError::message(format!(
        "compiler acquisition {label} index is out of bounds"
    )))
}

fn validate_indices<I: acquisition_index::Sealed + Ord>(
    indices: &[I],
    len: usize,
    label: &'static str,
) -> RailResult<()> {
    if indices.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(RailError::message(format!(
            "compiler acquisition {label} indices are duplicate or unsorted"
        )));
    }
    for index in indices {
        validate_index(*index, len, label)?;
    }
    Ok(())
}

fn validate_strictly_sorted<T: Ord>(values: &[T], label: &'static str) -> RailResult<()> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(RailError::message(format!(
            "compiler acquisition {label} table is duplicate or unsorted"
        )));
    }
    Ok(())
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

    let mut grouped = BTreeMap::<(PlatformTarget, FeatureSelection, String), BTreeSet<String>>::new();
    for target in targets.iter().copied().collect::<BTreeSet<_>>() {
        let cfg_set = target_cfg_sets.get(target);
        for manifest in manifests {
            for selection in coverage_feature_selections(manifest, cfg_set) {
                grouped
                    .entry((PlatformTarget::from(target), selection, manifest.package_name.clone()))
                    .or_default()
                    .insert(manifest.package_name.clone());
            }
        }
    }

    Ok(grouped
        .into_iter()
        .map(|((target, features, _package), packages)| CoverageView::new(target, features, packages))
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
            packages: BTreeSet::from(["app".to_string()]),
            domains: BTreeSet::from([CompilerFactDomain::Production]),
            fact_families: BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
        };

        let arguments = view.cargo_arguments(&["app"]).expect("scheduled package");
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
                "--package",
                "app",
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
        assert_eq!(left.len(), 10);
        assert!(left.iter().all(|view| view.packages.len() == 1));
        let selected = left
      .iter()
      .find(
        |view| view.packages == ["app".to_string()]
          && matches!(view.features, FeatureSelection::Selected(ref features) if features == &["backend", "common"]),
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
                "--package",
                "app",
                "--target",
                "x86_64-unknown-linux-gnu",
            ]
        );
    }

    #[test]
    fn combined_schedule_shares_same_package_facts_but_isolates_package_features() {
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

        assert_eq!(schedule.views().len(), 15);
        let check_views = schedule
            .views()
            .iter()
            .filter(|view| view.acquisition == AnalysisAcquisition::CheckAllTargets)
            .collect::<Vec<_>>();
        assert_eq!(
            check_views.len(),
            10,
            "diagnostics and typed facts must share each package-local check view"
        );
        assert!(check_views.iter().all(|view| view.packages().len() == 1));
        assert!(
            check_views
                .iter()
                .filter(|view| view.packages().contains("app"))
                .all(|view| {
                    view.fact_families()
                        == &BTreeSet::from([
                            CompilerFactFamily::StableDiagnostics,
                            CompilerFactFamily::TypedRustItems,
                        ])
                })
        );
        assert!(
            check_views
                .iter()
                .filter(|view| view.packages().contains("worker"))
                .all(|view| view.fact_families() == &BTreeSet::from([CompilerFactFamily::TypedRustItems]))
        );

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

        assert_eq!(schedule.views().len(), 9);
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
            .find(|view| {
                view.packages() == &BTreeSet::from(["worker".to_string()])
                    && view.features() == &FeatureSelection::DefaultWith(vec!["backend".to_string()])
            })
            .expect("default-plus-selected view");
        let arguments = default_with
            .cargo_arguments(&["worker"])
            .expect("default-plus-selected argv");
        assert!(!arguments.contains(&OsString::from("--no-default-features")));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--features", "worker/backend"])
        );
        assert!(!arguments.contains(&OsString::from("app/backend")));
        assert_eq!(arguments.iter().filter(|argument| *argument == "--package").count(), 1);
    }

    #[test]
    fn package_local_view_rejects_multi_package_execution_and_cache_identity() {
        let view = AnalysisView {
            acquisition: AnalysisAcquisition::CheckAllTargets,
            platform: PlatformTarget::from("default"),
            features: FeatureSelection::Selected(vec!["alloc".to_string()]),
            packages: BTreeSet::from(["alloc-root".to_string()]),
            domains: BTreeSet::from([CompilerFactDomain::Production]),
            fact_families: BTreeSet::from([CompilerFactFamily::TypedRustItems]),
        };

        view.cargo_arguments(&["std-root", "alloc-root"])
            .expect_err("package-local view must reject multiple roots");
        view.fact_cache_identity(&["std-root", "alloc-root"], &BTreeSet::from(["alloc-root".to_string()]))
            .expect_err("package-local fact identity must reject multiple roots");
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

    #[test]
    fn acquisition_plan_identity_is_canonical_across_discovery_order() {
        let targets = ["default", "x86_64-unknown-linux-gnu"];
        let mut normal = candidate("app", &targets);
        normal.crate_name = "normal_dep".to_string();
        let mut development = candidate("app", &targets);
        development.crate_name = "dev_dep".to_string();
        development.kind = DepKind::Dev;
        let left_candidates = [normal.clone(), development.clone(), normal.clone()];
        let right_candidates = [development, normal];
        let left_schedule =
            AnalysisSchedule::for_diagnostics(&[manifest("app", "first/root/app")], &targets, &left_candidates)
                .expect("left schedule");
        let right_schedule = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "another/root/app")],
            &[targets[1], targets[0]],
            &right_candidates,
        )
        .expect("right schedule");

        let left =
            CompilerAcquisitionPlan::from_schedule(&left_schedule, &left_candidates, &targets).expect("left plan");
        let right =
            CompilerAcquisitionPlan::from_schedule(&right_schedule, &right_candidates, &[targets[1], targets[0]])
                .expect("right plan");

        assert_eq!(left, right);
        assert_eq!(left.identity(), right.identity());
        assert_eq!(left.package_count(), 1);
        assert_eq!(left.target_count(), 2);
        assert_eq!(left.candidate_count(), 2, "duplicate requirements must intern once");
        assert!(left.views().all(|view| view.package() == "app"));
    }

    #[test]
    fn acquisition_plan_preserves_serial_argv_and_fact_identity() {
        let typed_packages = BTreeSet::from(["app".to_string()]);
        let schedule = AnalysisSchedule::for_combined(
            &[manifest("app", "app")],
            &["default"],
            &[],
            &typed_packages,
            &BTreeSet::new(),
        )
        .expect("schedule");
        let plan = CompilerAcquisitionPlan::from_schedule(&schedule, &[], &["default"]).expect("plan");
        let scheduled = schedule
            .views()
            .iter()
            .find(|view| view.features() == &FeatureSelection::Default)
            .expect("default scheduled view");
        let planned = plan
            .views()
            .find(|view| view.features() == FeatureSelection::Default)
            .expect("default planned view");

        assert_eq!(
            planned.cargo_arguments(),
            scheduled.cargo_arguments(&["app"]).expect("legacy argv")
        );
        assert_eq!(
            planned.fact_cache_identity("app").expect("planned identity"),
            scheduled
                .fact_cache_identity(&["app"], &typed_packages)
                .expect("scheduled identity")
        );
    }

    #[test]
    fn acquisition_plan_rejects_multi_root_views_before_execution() {
        let view = AnalysisView {
            acquisition: AnalysisAcquisition::CheckAllTargets,
            platform: PlatformTarget::from("default"),
            features: FeatureSelection::Default,
            packages: BTreeSet::from(["app".to_string(), "worker".to_string()]),
            domains: BTreeSet::from([CompilerFactDomain::Production]),
            fact_families: BTreeSet::from([CompilerFactFamily::StableDiagnostics]),
        };
        let schedule = AnalysisSchedule {
            packages: view.packages.clone(),
            views: vec![view],
        };

        let error = CompilerAcquisitionPlan::from_schedule(&schedule, &[], &["default"])
            .expect_err("multi-root view must fail");
        assert!(error.to_string().contains("exactly one root package"), "{error}");
    }

    #[test]
    fn acquisition_plan_validation_rejects_corrupt_indices_and_order() {
        let schedule = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "app")],
            &["default"],
            &[candidate("app", &["default"])],
        )
        .expect("schedule");
        let plan = CompilerAcquisitionPlan::from_schedule(&schedule, &[candidate("app", &["default"])], &["default"])
            .expect("plan");

        let mut out_of_range = plan.clone();
        out_of_range.views.last_mut().expect("non-empty plan").target = TargetIx(u32::MAX);
        let error = out_of_range.validate().expect_err("out-of-range target must fail");
        assert!(
            error.to_string().contains("view target index is out of bounds"),
            "{error}"
        );

        let mut duplicate_order = plan;
        duplicate_order.execution_order[1] = duplicate_order.execution_order[0];
        let error = duplicate_order
            .validate()
            .expect_err("duplicate execution order must fail");
        assert!(error.to_string().contains("duplicate or incomplete"), "{error}");
    }

    #[test]
    fn acquisition_plan_checks_index_width_without_truncation() {
        if let Ok(oversized) = usize::try_from(u64::from(u32::MAX) + 1) {
            let error = PackageIx::checked(oversized).expect_err("oversized index must fail");
            assert!(
                error.to_string().contains("representable package index space"),
                "{error}"
            );
        }
    }

    #[test]
    fn acquisition_plan_priority_preserves_membership_and_cheap_failure_order() {
        let schedule = AnalysisSchedule::for_diagnostics(
            &[manifest("app", "app")],
            &["default"],
            &[candidate("app", &["default"])],
        )
        .expect("schedule");
        let plan = CompilerAcquisitionPlan::from_schedule(&schedule, &[candidate("app", &["default"])], &["default"])
            .expect("plan");
        let canonical = plan
            .views()
            .map(CompilerAcquisitionView::index)
            .collect::<BTreeSet<_>>();
        let prioritized = plan
            .execution_order()
            .map(CompilerAcquisitionView::index)
            .collect::<BTreeSet<_>>();
        let features = plan
            .execution_order()
            .map(CompilerAcquisitionView::features)
            .collect::<Vec<_>>();

        assert_eq!(canonical, prioritized, "priority may not alter required membership");
        assert_eq!(features.first(), Some(&FeatureSelection::Default));
        assert_eq!(features.get(1), Some(&FeatureSelection::NoDefaultFeatures));
        assert_eq!(features.last(), Some(&FeatureSelection::AllFeatures));
        assert_eq!(
            plan.candidate_sets.len(),
            1,
            "identical candidate memberships must intern once"
        );
    }

    #[test]
    fn acquisition_plan_rejects_candidates_outside_its_compiler_targets() {
        let typed = BTreeSet::from(["app".to_string()]);
        let schedule =
            AnalysisSchedule::for_combined(&[manifest("app", "app")], &["default"], &[], &typed, &BTreeSet::new())
                .expect("typed schedule");
        let error = CompilerAcquisitionPlan::from_schedule(
            &schedule,
            &[candidate("app", &["aarch64-unknown-linux-gnu"])],
            &["default"],
        )
        .expect_err("candidate outside compiler targets must fail");
        assert!(error.to_string().contains("unconfigured target"), "{error}");
    }
}
