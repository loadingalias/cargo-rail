//! Pure Rust source-surface graph merge and reachability analysis.
//!
//! Compiler item identifiers join facts within one compiler view. Durable
//! surface identity comes from source-qualified physical declarations so the
//! same declaration merges across feature, target, and Cargo target views
//! without depending on a workspace's absolute root.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::compiler::facts::{
    CompilerFactDomain, CompilerFactEdgeKind, CompilerFactItemKind, CompilerFactMacroProvenance, CompilerFactNamespace,
    CompilerFactObject, CompilerFactPackage, CompilerFactRetentionReason, CompilerFactRole, CompilerFactSourcePath,
    CompilerFactSpan, CompilerFactTargetKind, CompilerFactVisibility, CompilerItemFact, CompilerItemId,
    ValidatedCompilerFactObject, required_compiler_fact_coverage,
};
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const SURFACE_ITEM_IDENTITY_PREFIX: &str = "surface-item-v1-sha256-";
const SURFACE_FINDING_IDENTITY_PREFIX: &str = "surface-finding-v1-sha256-";

/// Root-independent identity of source bytes used by a declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SurfaceSourceIdentity {
    path: CompilerFactSourcePath,
    content_digest: String,
    bytes: u64,
}

/// Exact source range after resolving a fragment-local source-table index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SurfaceSpan {
    source: SurfaceSourceIdentity,
    start: u64,
    end: u64,
}

/// Physical declaration identity shared by equivalent compiler views.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SurfaceItemKey {
    span: SurfaceSpan,
    source_context: String,
    namespace: CompilerFactNamespace,
    kind: CompilerFactItemKind,
    ordinal: u16,
}

/// One Rust privacy domain, normalized across feature and platform views.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SurfaceCrateIdentity {
    package: CompilerFactPackage,
    cargo_target: String,
    crate_name: String,
    target_kind: CompilerFactTargetKind,
    role: CompilerFactRole,
}

/// Cargo product kind used to select complete production roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SurfaceProductKind {
    Binary,
    Library,
}

/// Exact configured Cargo product root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SurfaceProductRoot {
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) kind: SurfaceProductKind,
}

impl From<&CompilerFactObject> for SurfaceCrateIdentity {
    fn from(object: &CompilerFactObject) -> Self {
        Self {
            package: object.unit.package.clone(),
            cargo_target: object.unit.cargo_target.clone(),
            crate_name: object.unit.crate_name.clone(),
            target_kind: object.unit.target_kind.clone(),
            role: object.unit.role,
        }
    }
}

impl SurfaceItemKey {
    fn identity(&self) -> RailResult<String> {
        Ok(format!(
            "{SURFACE_ITEM_IDENTITY_PREFIX}{}",
            ContentDigest::sha256(&serde_json::to_vec(self)?)
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SurfaceVisibility {
    Private,
    Restricted,
    Crate,
    Public,
}

impl From<&CompilerFactVisibility> for SurfaceVisibility {
    fn from(visibility: &CompilerFactVisibility) -> Self {
        match visibility {
            CompilerFactVisibility::Private => Self::Private,
            CompilerFactVisibility::Restricted(_) | CompilerFactVisibility::RestrictedCrateRoot => Self::Restricted,
            CompilerFactVisibility::Crate => Self::Crate,
            CompilerFactVisibility::Public => Self::Public,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SurfaceMacroProvenance {
    Written,
    Expansion,
    Generated,
}

impl From<&CompilerFactMacroProvenance> for SurfaceMacroProvenance {
    fn from(provenance: &CompilerFactMacroProvenance) -> Self {
        match provenance {
            CompilerFactMacroProvenance::Written => Self::Written,
            CompilerFactMacroProvenance::Expansion(_) => Self::Expansion,
            CompilerFactMacroProvenance::Generated => Self::Generated,
        }
    }
}

/// Stable report code for one compiler-owned conservative retention reason.
pub(crate) fn retention_reason_code(object: &CompilerFactObject, reason: &CompilerFactRetentionReason) -> String {
    match reason {
        CompilerFactRetentionReason::AllowDeadCode => "allow-dead-code".to_string(),
        CompilerFactRetentionReason::ForeignExport => "foreign-export".to_string(),
        CompilerFactRetentionReason::NoMangle => "no-mangle".to_string(),
        CompilerFactRetentionReason::ExportName => "export-name".to_string(),
        CompilerFactRetentionReason::Used => "used".to_string(),
        CompilerFactRetentionReason::ProcMacro => "proc-macro".to_string(),
        CompilerFactRetentionReason::UnresolvedTraitDispatch => "unresolved-trait-dispatch".to_string(),
        CompilerFactRetentionReason::RequiredImplementationInterface => "required-implementation-interface".to_string(),
        CompilerFactRetentionReason::GeneratedRegistration => "generated-registration".to_string(),
        CompilerFactRetentionReason::IncompleteProvenance => "incomplete-provenance".to_string(),
        CompilerFactRetentionReason::ExternallyAddressed => "externally-addressed".to_string(),
        CompilerFactRetentionReason::Other(detail) => format!("other:{}", object.strings[detail.0 as usize]),
    }
}

#[derive(Debug)]
struct SurfaceItem {
    identity: String,
    names: BTreeSet<String>,
    diagnostic_paths: BTreeSet<String>,
    packages: BTreeSet<CompilerFactPackage>,
    compiler_crates: BTreeSet<SurfaceCrateIdentity>,
    parent_observations: BTreeSet<Option<SurfaceItemKey>>,
    written_visibilities: BTreeSet<SurfaceVisibility>,
    visibility_spans: BTreeSet<Option<SurfaceSpan>>,
    macro_provenance: BTreeSet<SurfaceMacroProvenance>,
    retentions: BTreeSet<String>,
}

impl SurfaceItem {
    fn new(identity: String) -> Self {
        Self {
            identity,
            names: BTreeSet::new(),
            diagnostic_paths: BTreeSet::new(),
            packages: BTreeSet::new(),
            compiler_crates: BTreeSet::new(),
            parent_observations: BTreeSet::new(),
            written_visibilities: BTreeSet::new(),
            visibility_spans: BTreeSet::new(),
            macro_provenance: BTreeSet::new(),
            retentions: BTreeSet::new(),
        }
    }

    fn classification_is_supported(&self) -> bool {
        self.retentions.is_empty()
            && self.names.len() == 1
            && self.written_visibilities.len() == 1
            && self.visibility_spans.len() == 1
            && self.visibility_spans.first().is_some_and(Option::is_some)
            && self.macro_provenance == BTreeSet::from([SurfaceMacroProvenance::Written])
    }

    fn written_visibility(&self) -> Option<SurfaceVisibility> {
        (self.written_visibilities.len() == 1)
            .then(|| self.written_visibilities.first().copied())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SurfaceEdge {
    source: SurfaceItemKey,
    target: SurfaceItemKey,
    kind: CompilerFactEdgeKind,
}

/// Closed-world authority supplied by the policy layer.
#[derive(Debug, Clone, Default)]
pub(crate) struct SurfacePolicy {
    closed_world_packages: BTreeSet<CompilerFactPackage>,
    unnecessary_crate_visibility: bool,
    preserve_uniform_fields: bool,
    products: BTreeSet<SurfaceProductRoot>,
}

impl SurfacePolicy {
    pub(crate) fn new(
        closed_world_packages: BTreeSet<CompilerFactPackage>,
        unnecessary_crate_visibility: bool,
    ) -> Self {
        Self {
            closed_world_packages,
            unnecessary_crate_visibility,
            preserve_uniform_fields: false,
            products: BTreeSet::new(),
        }
    }

    pub(crate) fn with_products(mut self, products: BTreeSet<SurfaceProductRoot>) -> Self {
        self.products = products;
        self
    }

    pub(crate) fn preserving_uniform_fields(mut self, preserve: bool) -> Self {
        self.preserve_uniform_fields = preserve;
        self
    }
}

/// One enabled source-surface conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SurfaceFindingKind {
    DeadPublic,
    UnnecessaryPublic,
    UnnecessaryRestrictedVisibility,
    UnnecessaryCrateVisibility,
}

impl SurfaceFindingKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DeadPublic => "dead-public",
            Self::UnnecessaryPublic => "unnecessary-public",
            Self::UnnecessaryRestrictedVisibility => "unnecessary-restricted-visibility",
            Self::UnnecessaryCrateVisibility => "unnecessary-crate-visibility",
        }
    }
}

fn item_kind_name(kind: CompilerFactItemKind) -> &'static str {
    match kind {
        CompilerFactItemKind::Function => "function",
        CompilerFactItemKind::Method => "method",
        CompilerFactItemKind::AssociatedConstant => "associated-constant",
        CompilerFactItemKind::Trait => "trait",
        CompilerFactItemKind::Struct => "struct",
        CompilerFactItemKind::Enum => "enum",
        CompilerFactItemKind::Union => "union",
        CompilerFactItemKind::TypeAlias => "type-alias",
        CompilerFactItemKind::AssociatedType => "associated-type",
        CompilerFactItemKind::Constant => "constant",
        CompilerFactItemKind::Static => "static",
        CompilerFactItemKind::Field => "field",
        CompilerFactItemKind::Variant => "variant",
        CompilerFactItemKind::Reexport => "reexport",
        CompilerFactItemKind::Module => "module",
        CompilerFactItemKind::Impl => "impl",
        CompilerFactItemKind::Macro => "macro",
        CompilerFactItemKind::ForeignFunction => "foreign-function",
        CompilerFactItemKind::ForeignStatic => "foreign-static",
    }
}

/// Deterministic finding produced from one physical declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SurfaceFinding {
    pub(crate) identity: String,
    pub(crate) item_identity: String,
    pub(crate) kind: SurfaceFindingKind,
    pub(crate) name: String,
    pub(crate) item_kind: &'static str,
    pub(crate) packages: Vec<String>,
    pub(crate) diagnostic_paths: Vec<String>,
    pub(crate) source: String,
    pub(crate) source_generated: bool,
    pub(crate) declaration_start: u64,
    pub(crate) declaration_end: u64,
    pub(crate) visibility_start: u64,
    pub(crate) visibility_end: u64,
    pub(crate) replacement: Option<&'static str>,
    pub(crate) production_live: bool,
    pub(crate) non_production_live: bool,
    pub(crate) required_public: bool,
    pub(crate) reasons: Vec<&'static str>,
}

/// Reachability state for one physical declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SurfaceItemAnalysis {
    pub(crate) identity: String,
    pub(crate) item_kind: &'static str,
    pub(crate) packages: Vec<String>,
    pub(crate) diagnostic_paths: Vec<String>,
    pub(crate) source: String,
    pub(crate) source_generated: bool,
    pub(crate) production_live: bool,
    pub(crate) non_production_live: bool,
    pub(crate) required_public: bool,
    pub(crate) retained: bool,
}

/// Pure graph result consumed by future report and mutation layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SurfaceAnalysis {
    pub(crate) items: Vec<SurfaceItemAnalysis>,
    pub(crate) findings: Vec<SurfaceFinding>,
    pub(crate) metrics: SurfaceGraphMetrics,
}

/// Exact amount of merged graph work performed by the three required closures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SurfaceGraphMetrics {
    pub(crate) nodes: usize,
    pub(crate) edges: usize,
    pub(crate) traversals: usize,
    pub(crate) edge_visits: usize,
}

/// Merged physical graph across every compiler view in one surface analysis.
pub(crate) struct SurfaceGraph {
    items: BTreeMap<SurfaceItemKey, SurfaceItem>,
    adjacency: BTreeMap<SurfaceItemKey, BTreeSet<(CompilerFactEdgeKind, SurfaceItemKey)>>,
    incoming: BTreeMap<SurfaceItemKey, BTreeSet<SurfaceItemKey>>,
    product_roots: BTreeMap<SurfaceProductRoot, BTreeSet<SurfaceItemKey>>,
    production_retention_roots: BTreeSet<SurfaceItemKey>,
    non_production_roots: BTreeSet<SurfaceItemKey>,
    required_public_roots: BTreeSet<SurfaceItemKey>,
}

impl SurfaceGraph {
    /// Merge authenticated, complete compiler-fact objects without reacquiring workspace state.
    pub(crate) fn from_compiler_facts(objects: &[ValidatedCompilerFactObject]) -> RailResult<Self> {
        let objects = objects
            .iter()
            .map(ValidatedCompilerFactObject::object)
            .collect::<Vec<_>>();
        Self::from_objects(&objects)
    }

    fn from_objects(objects: &[&CompilerFactObject]) -> RailResult<Self> {
        let required_coverage = required_compiler_fact_coverage();
        let mut producer = None;
        let mut sources = BTreeMap::<CompilerFactSourcePath, (String, u64)>::new();
        for object in objects {
            if !required_coverage.is_subset(&object.completion.coverage) {
                return Err(RailError::message(
                    "surface analysis received compiler facts without complete required coverage",
                ));
            }
            if let Some(expected) = producer {
                if expected != &object.producer_authority {
                    return Err(RailError::message(
                        "surface analysis cannot merge compiler facts from different compiler producers",
                    ));
                }
            } else {
                producer = Some(&object.producer_authority);
            }
            for source in &object.sources {
                let authority = (source.content_digest.clone(), source.bytes);
                if sources
                    .insert(source.path.clone(), authority.clone())
                    .is_some_and(|previous| previous != authority)
                {
                    return Err(RailError::message(
                        "surface analysis cannot merge conflicting bytes for one source path",
                    ));
                }
            }
        }

        let mut items = BTreeMap::<SurfaceItemKey, SurfaceItem>::new();
        let mut local_items = Vec::with_capacity(objects.len());
        let mut compiler_items = BTreeMap::<CompilerItemId, BTreeSet<SurfaceItemKey>>::new();
        for object in objects {
            let mut local = BTreeMap::new();
            for item in &object.items {
                let key = item_key(object, item)?;
                local.insert(item.id, key.clone());
                compiler_items.entry(item.id).or_default().insert(key.clone());
                let identity = key.identity()?;
                let merged = items.entry(key.clone()).or_insert_with(|| SurfaceItem::new(identity));
                merged.names.insert(object.strings[item.name.0 as usize].clone());
                merged
                    .diagnostic_paths
                    .insert(object.strings[item.diagnostic_path.0 as usize].clone());
                merged.packages.insert(object.unit.package.clone());
                merged.compiler_crates.insert(SurfaceCrateIdentity::from(*object));
                merged
                    .written_visibilities
                    .insert(SurfaceVisibility::from(&item.written_visibility));
                merged.visibility_spans.insert(
                    item.visibility_span
                        .map(|span| surface_span(object, span))
                        .transpose()?,
                );
                merged
                    .macro_provenance
                    .insert(SurfaceMacroProvenance::from(&item.macro_provenance));
            }
            local_items.push(local);
        }

        let mut edges = BTreeSet::new();
        let mut product_roots = BTreeMap::<SurfaceProductRoot, BTreeSet<SurfaceItemKey>>::new();
        let mut production_retention_roots = BTreeSet::new();
        let mut non_production_roots = BTreeSet::new();
        let mut required_public_roots = BTreeSet::new();
        for (object_index, object) in objects.iter().enumerate() {
            let local = &local_items[object_index];
            let product = surface_product(object);
            if let Some(product) = &product {
                let roots = product_roots.entry(product.clone()).or_default();
                if product.kind == SurfaceProductKind::Library {
                    roots.extend(
                        object
                            .items
                            .iter()
                            .filter(|item| item.effective_visibility == CompilerFactVisibility::Public)
                            .map(|item| local[&item.id].clone()),
                    );
                }
            }
            for item in &object.items {
                let key = &local[&item.id];
                let parent = item.parent.as_ref().map(|parent| local[parent].clone());
                items
                    .get_mut(key)
                    .ok_or_else(|| RailError::message("surface item disappeared during graph merge"))?
                    .parent_observations
                    .insert(parent);
            }
            for entry in &object.entry_points {
                let root = local[&entry.item].clone();
                if object.unit.domain == CompilerFactDomain::Production {
                    if let Some(product) = &product {
                        product_roots.entry(product.clone()).or_default().insert(root);
                    }
                } else {
                    non_production_roots.insert(root);
                }
            }
            for retention in &object.retentions {
                let root = local[&retention.item].clone();
                items
                    .get_mut(&root)
                    .ok_or_else(|| RailError::message("surface retention root disappeared during graph merge"))?
                    .retentions
                    .insert(retention_reason_code(object, &retention.reason));
                if object.unit.domain == CompilerFactDomain::Production {
                    production_retention_roots.insert(root);
                } else {
                    non_production_roots.insert(root);
                }
            }
            for edge in &object.edges {
                let source = local[&edge.source].clone();
                let targets = local
                    .get(&edge.target)
                    .map(|target| BTreeSet::from([target.clone()]))
                    .or_else(|| compiler_items.get(&edge.target).cloned())
                    .unwrap_or_default();
                for target in targets {
                    edges.insert(SurfaceEdge {
                        source: source.clone(),
                        target: target.clone(),
                        kind: edge.kind,
                    });
                    if edge.source.0[0] != edge.target.0[0] {
                        required_public_roots.insert(target);
                    }
                }
            }
        }

        let mut adjacency = BTreeMap::<_, BTreeSet<_>>::new();
        let mut incoming = BTreeMap::<_, BTreeSet<_>>::new();
        for edge in edges {
            adjacency
                .entry(edge.source.clone())
                .or_default()
                .insert((edge.kind, edge.target.clone()));
            incoming.entry(edge.target).or_default().insert(edge.source);
        }
        Ok(Self {
            items,
            adjacency,
            incoming,
            product_roots,
            production_retention_roots,
            non_production_roots,
            required_public_roots,
        })
    }

    /// Compute the three closures and classify only declarations under explicit closed-world authority.
    pub(crate) fn analyze(&self, policy: &SurfacePolicy) -> RailResult<SurfaceAnalysis> {
        let mut production_roots = self.production_retention_roots.clone();
        let mut required_public_roots = self.required_public_roots.clone();
        if policy.products.is_empty() {
            for (product, roots) in &self.product_roots {
                if product.kind == SurfaceProductKind::Binary {
                    production_roots.extend(roots.iter().cloned());
                }
            }
        } else {
            for product in &policy.products {
                let roots = self.product_roots.get(product).ok_or_else(|| {
                    RailError::message(format!(
                        "configured surface product '{}:{}' was not compiled",
                        product.package, product.target
                    ))
                })?;
                production_roots.extend(roots.iter().cloned());
                if product.kind == SurfaceProductKind::Library {
                    required_public_roots.extend(roots.iter().cloned());
                }
            }
        }
        let (production_live, production_edge_visits) = self.closure(&production_roots, |_| true);
        let (non_production_live, non_production_edge_visits) = self.closure(&self.non_production_roots, |_| true);
        let (required_public, required_public_edge_visits) = self.closure(&required_public_roots, |kind| {
            matches!(
                kind,
                CompilerFactEdgeKind::Interface
                    | CompilerFactEdgeKind::Reexport
                    | CompilerFactEdgeKind::VisibilityParent
                    | CompilerFactEdgeKind::VisibilityRequirement
            )
        });
        let mut analyzed_items = Vec::with_capacity(self.items.len());
        let mut findings = Vec::new();
        for (key, item) in &self.items {
            let (source, source_generated) = match &key.span.source.path {
                CompilerFactSourcePath::Repository(path) => (path.clone(), false),
                CompilerFactSourcePath::Generated(path) => (path.clone(), true),
            };
            let state = SurfaceItemAnalysis {
                identity: item.identity.clone(),
                item_kind: item_kind_name(key.kind),
                packages: item.packages.iter().map(|package| package.name.clone()).collect(),
                diagnostic_paths: item.diagnostic_paths.iter().cloned().collect(),
                source: source.clone(),
                source_generated,
                production_live: production_live.contains(key),
                non_production_live: non_production_live.contains(key),
                required_public: required_public.contains(key),
                retained: !item.retentions.is_empty(),
            };
            if self.item_has_closed_world_authority(item, policy)
                && item.classification_is_supported()
                && let Some(kind) = self.classify_item(key, item, &state, policy)
            {
                let identity = finding_identity(key, kind)?;
                let visibility_span = item
                    .visibility_spans
                    .first()
                    .and_then(Option::as_ref)
                    .ok_or_else(|| RailError::message("classified surface item has no exact visibility span"))?;
                findings.push(SurfaceFinding {
                    identity,
                    item_identity: item.identity.clone(),
                    kind,
                    name: item.names.first().cloned().unwrap_or_default(),
                    item_kind: item_kind_name(key.kind),
                    packages: item.packages.iter().map(|package| package.name.clone()).collect(),
                    diagnostic_paths: item.diagnostic_paths.iter().cloned().collect(),
                    source,
                    source_generated,
                    declaration_start: key.span.start,
                    declaration_end: key.span.end,
                    visibility_start: visibility_span.start,
                    visibility_end: visibility_span.end,
                    replacement: self.replacement(key, kind, policy),
                    production_live: state.production_live,
                    non_production_live: state.non_production_live,
                    required_public: state.required_public,
                    reasons: finding_reasons(kind, &state),
                });
            }
            analyzed_items.push(state);
        }
        if policy.preserve_uniform_fields {
            self.preserve_uniform_field_findings(&mut findings);
        }
        Ok(SurfaceAnalysis {
            items: analyzed_items,
            findings,
            metrics: SurfaceGraphMetrics {
                nodes: self.items.len(),
                edges: self.adjacency.values().map(BTreeSet::len).sum(),
                traversals: 3,
                edge_visits: production_edge_visits + non_production_edge_visits + required_public_edge_visits,
            },
        })
    }

    fn closure(
        &self,
        roots: &BTreeSet<SurfaceItemKey>,
        follows: impl Fn(CompilerFactEdgeKind) -> bool,
    ) -> (BTreeSet<SurfaceItemKey>, usize) {
        let mut reached = roots.clone();
        let mut pending = roots.iter().cloned().collect::<Vec<_>>();
        let mut edge_visits = 0;
        while let Some(source) = pending.pop() {
            let Some(edges) = self.adjacency.get(&source) else {
                continue;
            };
            for (kind, target) in edges {
                edge_visits += 1;
                if follows(*kind) && reached.insert(target.clone()) {
                    pending.push(target.clone());
                }
            }
        }
        (reached, edge_visits)
    }

    fn item_has_closed_world_authority(&self, item: &SurfaceItem, policy: &SurfacePolicy) -> bool {
        !item.packages.is_empty()
            && item
                .packages
                .iter()
                .all(|package| policy.closed_world_packages.contains(package))
    }

    fn preserve_uniform_field_findings(&self, findings: &mut Vec<SurfaceFinding>) {
        let finding_kinds = findings
            .iter()
            .filter(|finding| finding.item_kind == "field")
            .map(|finding| (finding.item_identity.as_str(), finding.kind))
            .collect::<BTreeMap<_, _>>();
        let mut fields_by_parent = BTreeMap::<SurfaceItemKey, Vec<&str>>::new();
        for (key, item) in &self.items {
            if key.kind != CompilerFactItemKind::Field || item.parent_observations.len() != 1 {
                continue;
            }
            let Some(parent) = item.parent_observations.first().and_then(Option::as_ref) else {
                continue;
            };
            fields_by_parent.entry(parent.clone()).or_default().push(&item.identity);
        }
        let mut allowed = BTreeSet::new();
        for fields in fields_by_parent.values() {
            let kinds = fields
                .iter()
                .filter_map(|field| finding_kinds.get(field).copied())
                .collect::<BTreeSet<_>>();
            if kinds.len() == 1 && fields.iter().all(|field| finding_kinds.contains_key(field)) {
                allowed.extend(fields.iter().copied());
            }
        }
        findings.retain(|finding| finding.item_kind != "field" || allowed.contains(finding.item_identity.as_str()));
    }

    fn classify_item(
        &self,
        key: &SurfaceItemKey,
        item: &SurfaceItem,
        state: &SurfaceItemAnalysis,
        policy: &SurfacePolicy,
    ) -> Option<SurfaceFindingKind> {
        let visibility = item.written_visibility()?;
        match visibility {
            SurfaceVisibility::Public if state.required_public => None,
            SurfaceVisibility::Public if !state.production_live && !state.non_production_live => {
                Some(SurfaceFindingKind::DeadPublic)
            }
            SurfaceVisibility::Public => Some(SurfaceFindingKind::UnnecessaryPublic),
            SurfaceVisibility::Restricted
                if !state.required_public
                    && self
                        .defining_scope(key)
                        .is_some_and(|scope| self.all_uses_fit(key, &scope)) =>
            {
                Some(SurfaceFindingKind::UnnecessaryRestrictedVisibility)
            }
            SurfaceVisibility::Crate if !state.required_public && policy.unnecessary_crate_visibility => {
                let parent = self.defining_scope(key).and_then(|scope| self.parent_scope(&scope));
                parent
                    .filter(|scope| self.all_uses_fit(key, scope))
                    .map(|_| SurfaceFindingKind::UnnecessaryCrateVisibility)
            }
            SurfaceVisibility::Private | SurfaceVisibility::Restricted | SurfaceVisibility::Crate => None,
        }
    }

    fn replacement(
        &self,
        key: &SurfaceItemKey,
        kind: SurfaceFindingKind,
        policy: &SurfacePolicy,
    ) -> Option<&'static str> {
        match kind {
            SurfaceFindingKind::DeadPublic => None,
            SurfaceFindingKind::UnnecessaryRestrictedVisibility => Some("private"),
            SurfaceFindingKind::UnnecessaryPublic if !policy.unnecessary_crate_visibility => Some("pub(crate)"),
            SurfaceFindingKind::UnnecessaryPublic | SurfaceFindingKind::UnnecessaryCrateVisibility => {
                let defining_scope = self.defining_scope(key)?;
                if self.all_uses_fit(key, &defining_scope) {
                    return Some("private");
                }
                if self
                    .parent_scope(&defining_scope)
                    .is_some_and(|parent| self.all_uses_fit(key, &parent))
                {
                    return Some("pub(super)");
                }
                (kind == SurfaceFindingKind::UnnecessaryPublic).then_some("pub(crate)")
            }
        }
    }

    fn all_uses_fit(&self, target: &SurfaceItemKey, scope: &SurfaceScope) -> bool {
        self.incoming
            .get(target)
            .is_none_or(|sources| sources.iter().all(|source| self.item_is_within(source, scope)))
    }

    fn defining_scope(&self, item: &SurfaceItemKey) -> Option<SurfaceScope> {
        let compiler_crate = exactly_one(&self.items.get(item)?.compiler_crates)?;
        let mut current = item;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current) {
                return None;
            }
            let node = self.items.get(current)?;
            if node.compiler_crates.len() != 1
                || node.compiler_crates.first() != Some(&compiler_crate)
                || node.parent_observations.len() != 1
            {
                return None;
            }
            let parent = node.parent_observations.first()?.as_ref();
            let Some(parent) = parent else {
                return Some(SurfaceScope::Crate(compiler_crate));
            };
            if parent.kind == CompilerFactItemKind::Module {
                return Some(SurfaceScope::Module(parent.clone(), compiler_crate));
            }
            current = parent;
        }
    }

    fn parent_scope(&self, scope: &SurfaceScope) -> Option<SurfaceScope> {
        match scope {
            SurfaceScope::Crate(_) => None,
            SurfaceScope::Module(module, _) => self.defining_scope(module),
        }
    }

    fn item_is_within(&self, item: &SurfaceItemKey, scope: &SurfaceScope) -> bool {
        let required_crate = scope.compiler_crate();
        let mut current = item;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current) {
                return false;
            }
            let Some(node) = self.items.get(current) else {
                return false;
            };
            if node.compiler_crates.len() != 1 || node.compiler_crates.first() != Some(required_crate) {
                return false;
            }
            if matches!(scope, SurfaceScope::Module(module, _) if module == current) {
                return true;
            }
            if node.parent_observations.len() != 1 {
                return false;
            }
            let Some(parent) = node.parent_observations.first().and_then(Option::as_ref) else {
                return matches!(scope, SurfaceScope::Crate(_));
            };
            current = parent;
        }
    }
}

fn finding_reasons(kind: SurfaceFindingKind, state: &SurfaceItemAnalysis) -> Vec<&'static str> {
    match kind {
        SurfaceFindingKind::DeadPublic => vec!["not-production-live", "not-non-production-live", "not-required-public"],
        SurfaceFindingKind::UnnecessaryPublic => {
            let mut reasons = Vec::with_capacity(3);
            if state.production_live {
                reasons.push("production-live");
            }
            if state.non_production_live {
                reasons.push("non-production-live");
            }
            reasons.push("not-required-public");
            reasons
        }
        SurfaceFindingKind::UnnecessaryRestrictedVisibility => {
            vec!["all-compiled-uses-within-defining-module"]
        }
        SurfaceFindingKind::UnnecessaryCrateVisibility => vec!["all-compiled-uses-within-parent-module"],
    }
}

#[derive(Debug)]
enum SurfaceScope {
    Crate(SurfaceCrateIdentity),
    Module(SurfaceItemKey, SurfaceCrateIdentity),
}

impl SurfaceScope {
    fn compiler_crate(&self) -> &SurfaceCrateIdentity {
        match self {
            Self::Crate(compiler_crate) | Self::Module(_, compiler_crate) => compiler_crate,
        }
    }
}

fn exactly_one<T: Clone + Ord>(values: &BTreeSet<T>) -> Option<T> {
    (values.len() == 1).then(|| values.first().cloned()).flatten()
}

fn surface_product(object: &CompilerFactObject) -> Option<SurfaceProductRoot> {
    if object.unit.domain != CompilerFactDomain::Production {
        return None;
    }
    let kind = match object.unit.target_kind {
        CompilerFactTargetKind::Binary => SurfaceProductKind::Binary,
        CompilerFactTargetKind::Library => SurfaceProductKind::Library,
        _ => return None,
    };
    Some(SurfaceProductRoot {
        package: object.unit.package.name.clone(),
        target: object.unit.cargo_target.clone(),
        kind,
    })
}

fn item_key(object: &CompilerFactObject, item: &CompilerItemFact) -> RailResult<SurfaceItemKey> {
    Ok(SurfaceItemKey {
        span: surface_span(object, item.physical.span)?,
        source_context: object.strings[item.physical.source_context.0 as usize].clone(),
        namespace: item.physical.namespace,
        kind: item.physical.kind,
        ordinal: item.physical.ordinal,
    })
}

fn surface_span(object: &CompilerFactObject, span: CompilerFactSpan) -> RailResult<SurfaceSpan> {
    let source = object
        .sources
        .get(span.source as usize)
        .ok_or_else(|| RailError::message("surface span names a missing compiler-fact source"))?;
    Ok(SurfaceSpan {
        source: SurfaceSourceIdentity {
            path: source.path.clone(),
            content_digest: source.content_digest.clone(),
            bytes: source.bytes,
        },
        start: span.start,
        end: span.end,
    })
}

fn finding_identity(key: &SurfaceItemKey, kind: SurfaceFindingKind) -> RailResult<String> {
    Ok(format!(
        "{SURFACE_FINDING_IDENTITY_PREFIX}{}",
        ContentDigest::sha256(&serde_json::to_vec(&(key, kind))?)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::facts::{
        COMPILER_FACT_PROTOCOL_VERSION, CompilerFactCompletion, CompilerFactEdge, CompilerFactEntryPoint,
        CompilerFactEntryPointKind, CompilerFactPhysicalIdentity, CompilerFactProducerAuthority, CompilerFactRetention,
        CompilerFactSource, CompilerFactStringId, CompilerFactUnit,
    };

    const CRATE_A: u64 = 10;
    const CRATE_B: u64 = 20;

    fn package(name: &str) -> CompilerFactPackage {
        CompilerFactPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            source: None,
        }
    }

    fn item(
        id: (u64, u64),
        span: (u64, u64),
        kind: CompilerFactItemKind,
        visibility: CompilerFactVisibility,
    ) -> CompilerItemFact {
        let public = !matches!(visibility, CompilerFactVisibility::Private);
        CompilerItemFact {
            id: CompilerItemId([id.0, id.1]),
            physical: CompilerFactPhysicalIdentity {
                span: CompilerFactSpan {
                    source: 0,
                    start: span.0,
                    end: span.1,
                },
                source_context: CompilerFactStringId(0),
                namespace: if kind == CompilerFactItemKind::Module {
                    CompilerFactNamespace::Type
                } else {
                    CompilerFactNamespace::Value
                },
                kind,
                ordinal: 0,
            },
            name: CompilerFactStringId(1),
            diagnostic_path: CompilerFactStringId(2),
            parent: None,
            written_visibility: visibility.clone(),
            visibility_span: public.then_some(CompilerFactSpan {
                source: 0,
                start: span.0,
                end: span.0 + 3,
            }),
            effective_visibility: visibility,
            macro_provenance: CompilerFactMacroProvenance::Written,
        }
    }

    fn object(
        package_name: &str,
        domain: CompilerFactDomain,
        items: Vec<CompilerItemFact>,
        edges: Vec<CompilerFactEdge>,
        entries: Vec<CompilerItemId>,
        retentions: Vec<(CompilerItemId, CompilerFactRetentionReason)>,
    ) -> CompilerFactObject {
        let coverage = required_compiler_fact_coverage();
        CompilerFactObject {
            version: COMPILER_FACT_PROTOCOL_VERSION,
            producer_authority: CompilerFactProducerAuthority {
                compiler_identity: "compiler".to_string(),
                driver_identity: "driver".to_string(),
            },
            unit: CompilerFactUnit {
                identity: format!("unit-{package_name}-{domain:?}"),
                invocation_identity: "invocation".to_string(),
                package: package(package_name),
                cargo_target: package_name.to_string(),
                crate_name: package_name.replace('-', "_"),
                target_kind: if domain == CompilerFactDomain::Production && !entries.is_empty() {
                    CompilerFactTargetKind::Binary
                } else {
                    CompilerFactTargetKind::Library
                },
                domain,
                role: CompilerFactRole::Target,
                platform: "host".to_string(),
                features: Vec::new(),
                cfg: Vec::new(),
            },
            strings: vec![
                "hygiene:root".to_string(),
                "item".to_string(),
                format!("{package_name}::item"),
            ],
            sources: vec![CompilerFactSource {
                path: CompilerFactSourcePath::Repository(format!("crates/{package_name}/src/lib.rs")),
                content_digest: format!("sha256:{package_name}"),
                bytes: 1_000,
            }],
            completion: CompilerFactCompletion {
                complete: true,
                coverage,
                strings: 3,
                sources: 1,
                items: items.len() as u64,
                edges: edges.len() as u64,
                entry_points: entries.len() as u64,
                retentions: retentions.len() as u64,
            },
            items,
            edges,
            entry_points: entries
                .into_iter()
                .map(|item| CompilerFactEntryPoint {
                    item,
                    kind: if domain == CompilerFactDomain::Production {
                        CompilerFactEntryPointKind::Main
                    } else {
                        CompilerFactEntryPointKind::TestHarness
                    },
                })
                .collect(),
            retentions: retentions
                .into_iter()
                .map(|(item, reason)| CompilerFactRetention { item, reason })
                .collect(),
        }
    }

    fn body(source: (u64, u64), target: (u64, u64)) -> CompilerFactEdge {
        CompilerFactEdge {
            source: CompilerItemId([source.0, source.1]),
            target: CompilerItemId([target.0, target.1]),
            kind: CompilerFactEdgeKind::Body,
        }
    }

    fn policy(packages: &[&str], crate_visibility: bool) -> SurfacePolicy {
        SurfacePolicy::new(packages.iter().map(|name| package(name)).collect(), crate_visibility)
    }

    #[test]
    fn production_non_production_and_dead_public_classify_independently() {
        let root = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        let production = item(
            (CRATE_A, 2),
            (20, 30),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let dead = item(
            (CRATE_A, 3),
            (40, 50),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let production_object = object(
            "app",
            CompilerFactDomain::Production,
            vec![root.clone(), production.clone(), dead.clone()],
            vec![body((CRATE_A, 1), (CRATE_A, 2))],
            vec![root.id],
            Vec::new(),
        );
        let test_root = item(
            (CRATE_A, 4),
            (60, 70),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        let non_production = item(
            (CRATE_A, 5),
            (80, 90),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let test_object = object(
            "app",
            CompilerFactDomain::NonProduction,
            vec![test_root.clone(), production, dead, non_production],
            vec![body((CRATE_A, 4), (CRATE_A, 5))],
            vec![test_root.id],
            Vec::new(),
        );

        let analysis = SurfaceGraph::from_objects(&[&production_object, &test_object])
            .expect("merge graph")
            .analyze(&policy(&["app"], false))
            .expect("analyze graph");
        assert_eq!(
            analysis.findings.iter().map(|finding| finding.kind).collect::<Vec<_>>(),
            vec![
                SurfaceFindingKind::UnnecessaryPublic,
                SurfaceFindingKind::DeadPublic,
                SurfaceFindingKind::UnnecessaryPublic,
            ]
        );
        let states = analysis
            .items
            .iter()
            .map(|state| (state.production_live, state.non_production_live, state.required_public))
            .collect::<Vec<_>>();
        assert!(states.contains(&(true, false, false)));
        assert!(states.contains(&(false, true, false)));
        assert!(states.contains(&(false, false, false)));
    }

    #[test]
    fn cross_crate_reference_requires_public_and_propagates_interface_requirements() {
        let consumer = item(
            (CRATE_B, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        let consumer_object = object(
            "consumer",
            CompilerFactDomain::Production,
            vec![consumer.clone()],
            vec![body((CRATE_B, 1), (CRATE_A, 1))],
            vec![consumer.id],
            Vec::new(),
        );
        let api = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let interface = item(
            (CRATE_A, 2),
            (20, 30),
            CompilerFactItemKind::Struct,
            CompilerFactVisibility::Public,
        );
        let provider_object = object(
            "provider",
            CompilerFactDomain::Production,
            vec![api, interface],
            vec![CompilerFactEdge {
                source: CompilerItemId([CRATE_A, 1]),
                target: CompilerItemId([CRATE_A, 2]),
                kind: CompilerFactEdgeKind::Interface,
            }],
            Vec::new(),
            Vec::new(),
        );

        let analysis = SurfaceGraph::from_objects(&[&consumer_object, &provider_object])
            .expect("merge graph")
            .analyze(&policy(&["consumer", "provider"], false))
            .expect("analyze graph");
        assert!(analysis.findings.is_empty());
        assert_eq!(analysis.items.iter().filter(|item| item.required_public).count(), 2);
    }

    #[test]
    fn conservative_retention_keeps_an_item_without_creating_a_reduction() {
        let retained = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let retained_id = retained.id;
        let facts = object(
            "app",
            CompilerFactDomain::Production,
            vec![retained],
            Vec::new(),
            Vec::new(),
            vec![(retained_id, CompilerFactRetentionReason::IncompleteProvenance)],
        );

        let analysis = SurfaceGraph::from_objects(&[&facts])
            .expect("merge graph")
            .analyze(&policy(&["app"], false))
            .expect("analyze graph");
        assert!(analysis.findings.is_empty());
        assert!(analysis.items[0].production_live);
        assert!(analysis.items[0].retained);
    }

    #[test]
    fn open_world_packages_never_receive_closed_world_findings() {
        let dead = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let facts = object(
            "library",
            CompilerFactDomain::Production,
            vec![dead],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let analysis = SurfaceGraph::from_objects(&[&facts])
            .expect("merge graph")
            .analyze(&SurfacePolicy::default())
            .expect("analyze graph");
        assert!(analysis.findings.is_empty());
    }

    #[test]
    fn restricted_visibility_reduces_only_when_every_use_is_inside_the_defining_module() {
        let module = item(
            (CRATE_A, 1),
            (0, 100),
            CompilerFactItemKind::Module,
            CompilerFactVisibility::Private,
        );
        let mut local_user = item(
            (CRATE_A, 2),
            (10, 20),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        local_user.parent = Some(module.id);
        let mut restricted = item(
            (CRATE_A, 3),
            (30, 40),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Restricted(module.id),
        );
        restricted.parent = Some(module.id);
        let local = object(
            "app",
            CompilerFactDomain::Production,
            vec![module.clone(), local_user.clone(), restricted.clone()],
            vec![body((CRATE_A, 2), (CRATE_A, 3))],
            vec![local_user.id],
            Vec::new(),
        );
        let local_analysis = SurfaceGraph::from_objects(&[&local])
            .expect("merge graph")
            .analyze(&policy(&["app"], false))
            .expect("analyze graph");
        assert_eq!(
            local_analysis
                .findings
                .iter()
                .map(|finding| finding.kind)
                .collect::<Vec<_>>(),
            vec![SurfaceFindingKind::UnnecessaryRestrictedVisibility]
        );
        assert_eq!(local_analysis.findings[0].replacement, Some("private"));

        let outside = item(
            (CRATE_A, 4),
            (110, 120),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        let external_use = object(
            "app",
            CompilerFactDomain::Production,
            vec![module, local_user, restricted, outside.clone()],
            vec![body((CRATE_A, 4), (CRATE_A, 3))],
            vec![outside.id],
            Vec::new(),
        );
        let external_analysis = SurfaceGraph::from_objects(&[&external_use])
            .expect("merge graph")
            .analyze(&policy(&["app"], false))
            .expect("analyze graph");
        assert!(external_analysis.findings.is_empty());
    }

    #[test]
    fn crate_visibility_reduction_is_opt_in_and_requires_a_parent_scope() {
        let parent = item(
            (CRATE_A, 1),
            (0, 200),
            CompilerFactItemKind::Module,
            CompilerFactVisibility::Private,
        );
        let mut module = item(
            (CRATE_A, 2),
            (10, 100),
            CompilerFactItemKind::Module,
            CompilerFactVisibility::Private,
        );
        module.parent = Some(parent.id);
        let mut user = item(
            (CRATE_A, 3),
            (20, 30),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        user.parent = Some(parent.id);
        let mut crate_visible = item(
            (CRATE_A, 4),
            (40, 50),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Crate,
        );
        crate_visible.parent = Some(module.id);
        let facts = object(
            "app",
            CompilerFactDomain::Production,
            vec![parent, module, user.clone(), crate_visible],
            vec![body((CRATE_A, 3), (CRATE_A, 4))],
            vec![user.id],
            Vec::new(),
        );
        let graph = SurfaceGraph::from_objects(&[&facts]).expect("merge graph");
        assert!(
            graph
                .analyze(&policy(&["app"], false))
                .expect("disabled lint")
                .findings
                .is_empty()
        );
        assert_eq!(
            graph
                .analyze(&policy(&["app"], true))
                .expect("enabled lint")
                .findings
                .iter()
                .map(|finding| finding.kind)
                .collect::<Vec<_>>(),
            vec![SurfaceFindingKind::UnnecessaryCrateVisibility]
        );
        assert_eq!(
            graph.analyze(&policy(&["app"], true)).expect("enabled lint").findings[0].replacement,
            Some("pub(super)")
        );
    }

    #[test]
    fn public_fix_reaches_the_narrowest_enabled_visibility_in_one_plan() {
        let root = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Private,
        );
        let live_public = item(
            (CRATE_A, 2),
            (20, 30),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let facts = object(
            "app",
            CompilerFactDomain::Production,
            vec![root.clone(), live_public],
            vec![body((CRATE_A, 1), (CRATE_A, 2))],
            vec![root.id],
            Vec::new(),
        );
        let graph = SurfaceGraph::from_objects(&[&facts]).expect("merge graph");

        assert_eq!(
            graph
                .analyze(&policy(&["app"], false))
                .expect("preserved crate visibility")
                .findings[0]
                .replacement,
            Some("pub(crate)")
        );
        assert_eq!(
            graph
                .analyze(&policy(&["app"], true))
                .expect("narrowest visibility")
                .findings[0]
                .replacement,
            Some("private")
        );
    }

    #[test]
    fn equivalent_views_merge_while_distinct_physical_declarations_remain_distinct() {
        let first = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let second = item(
            (CRATE_A, 2),
            (20, 30),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let production = object(
            "app",
            CompilerFactDomain::Production,
            vec![first.clone(), second.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let another_view = object(
            "app",
            CompilerFactDomain::NonProduction,
            vec![
                item(
                    (CRATE_B, 1),
                    (first.physical.span.start, first.physical.span.end),
                    CompilerFactItemKind::Function,
                    CompilerFactVisibility::Public,
                ),
                item(
                    (CRATE_B, 2),
                    (second.physical.span.start, second.physical.span.end),
                    CompilerFactItemKind::Function,
                    CompilerFactVisibility::Public,
                ),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let graph = SurfaceGraph::from_objects(&[&production, &another_view]).expect("merge graph");
        assert_eq!(graph.items.len(), 2);
        let analysis = graph.analyze(&policy(&["app"], false)).expect("analyze graph");
        assert_eq!(analysis.findings.len(), 2);
        assert_ne!(analysis.findings[0].item_identity, analysis.findings[1].item_identity);
        let reversed = SurfaceGraph::from_objects(&[&another_view, &production])
            .expect("merge reversed graph")
            .analyze(&policy(&["app"], false))
            .expect("analyze reversed graph");
        assert_eq!(analysis, reversed);
    }

    #[test]
    fn conflicting_source_bytes_cannot_be_merged() {
        let definition = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let first = object(
            "app",
            CompilerFactDomain::Production,
            vec![definition.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let mut second = object(
            "app",
            CompilerFactDomain::NonProduction,
            vec![definition],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        second.sources[0].content_digest = "sha256:changed".to_string();

        let error = SurfaceGraph::from_objects(&[&first, &second])
            .err()
            .expect("conflicting sources must fail");
        assert!(error.to_string().contains("conflicting bytes"));
    }

    #[test]
    fn configured_products_replace_the_implicit_all_binary_root_set() {
        let first_root = item(
            (CRATE_A, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let second_root = item(
            (CRATE_B, 1),
            (0, 10),
            CompilerFactItemKind::Function,
            CompilerFactVisibility::Public,
        );
        let first = object(
            "first",
            CompilerFactDomain::Production,
            vec![first_root.clone()],
            Vec::new(),
            vec![first_root.id],
            Vec::new(),
        );
        let second = object(
            "second",
            CompilerFactDomain::Production,
            vec![second_root.clone()],
            Vec::new(),
            vec![second_root.id],
            Vec::new(),
        );
        let graph = SurfaceGraph::from_objects(&[&first, &second]).expect("merge products");
        let implicit = graph
            .analyze(&policy(&["first", "second"], false))
            .expect("analyze implicit products");
        assert!(
            implicit
                .findings
                .iter()
                .all(|finding| finding.kind == SurfaceFindingKind::UnnecessaryPublic)
        );

        let selected = policy(&["first", "second"], false).with_products(BTreeSet::from([SurfaceProductRoot {
            package: "first".to_string(),
            target: "first".to_string(),
            kind: SurfaceProductKind::Binary,
        }]));
        let selected = graph.analyze(&selected).expect("analyze selected product");
        assert_eq!(
            selected.findings.iter().map(|finding| finding.kind).collect::<Vec<_>>(),
            vec![SurfaceFindingKind::UnnecessaryPublic, SurfaceFindingKind::DeadPublic]
        );
    }

    #[test]
    fn every_conservative_retention_reason_suppresses_reduction() {
        let reasons = [
            CompilerFactRetentionReason::AllowDeadCode,
            CompilerFactRetentionReason::ForeignExport,
            CompilerFactRetentionReason::NoMangle,
            CompilerFactRetentionReason::ExportName,
            CompilerFactRetentionReason::Used,
            CompilerFactRetentionReason::ProcMacro,
            CompilerFactRetentionReason::UnresolvedTraitDispatch,
            CompilerFactRetentionReason::RequiredImplementationInterface,
            CompilerFactRetentionReason::GeneratedRegistration,
            CompilerFactRetentionReason::IncompleteProvenance,
            CompilerFactRetentionReason::ExternallyAddressed,
            CompilerFactRetentionReason::Other(CompilerFactStringId(1)),
        ];
        for reason in reasons {
            let retained = item(
                (CRATE_A, 1),
                (0, 10),
                CompilerFactItemKind::Function,
                CompilerFactVisibility::Public,
            );
            let retained_id = retained.id;
            let facts = object(
                "app",
                CompilerFactDomain::Production,
                vec![retained],
                Vec::new(),
                Vec::new(),
                vec![(retained_id, reason)],
            );
            let analysis = SurfaceGraph::from_objects(&[&facts])
                .expect("merge retained item")
                .analyze(&policy(&["app"], false))
                .expect("analyze retained item");
            assert!(analysis.findings.is_empty());
            assert!(analysis.items[0].retained);
        }
    }

    #[test]
    fn generated_and_macro_expansion_declarations_never_produce_findings() {
        for provenance in [
            CompilerFactMacroProvenance::Generated,
            CompilerFactMacroProvenance::Expansion(Some(CompilerFactSpan {
                source: 0,
                start: 0,
                end: 10,
            })),
        ] {
            let mut declaration = item(
                (CRATE_A, 1),
                (0, 10),
                CompilerFactItemKind::Function,
                CompilerFactVisibility::Public,
            );
            declaration.macro_provenance = provenance;
            let facts = object(
                "app",
                CompilerFactDomain::Production,
                vec![declaration],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            assert!(
                SurfaceGraph::from_objects(&[&facts])
                    .expect("merge unsupported provenance")
                    .analyze(&policy(&["app"], false))
                    .expect("analyze unsupported provenance")
                    .findings
                    .is_empty()
            );
        }
    }

    #[test]
    fn uniform_field_policy_suppresses_partial_struct_field_rewrites() {
        let parent = item(
            (CRATE_A, 1),
            (0, 100),
            CompilerFactItemKind::Struct,
            CompilerFactVisibility::Private,
        );
        let mut public_field = item(
            (CRATE_A, 2),
            (10, 20),
            CompilerFactItemKind::Field,
            CompilerFactVisibility::Public,
        );
        public_field.parent = Some(parent.id);
        let mut private_field = item(
            (CRATE_A, 3),
            (30, 40),
            CompilerFactItemKind::Field,
            CompilerFactVisibility::Private,
        );
        private_field.parent = Some(parent.id);
        let facts = object(
            "app",
            CompilerFactDomain::Production,
            vec![parent, public_field, private_field],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let graph = SurfaceGraph::from_objects(&[&facts]).expect("merge fields");
        assert_eq!(
            graph
                .analyze(&policy(&["app"], false))
                .expect("ordinary field policy")
                .findings
                .iter()
                .filter(|finding| finding.item_kind == "field")
                .count(),
            1
        );
        assert_eq!(
            graph
                .analyze(&policy(&["app"], false).preserving_uniform_fields(true))
                .expect("uniform field policy")
                .findings
                .iter()
                .filter(|finding| finding.item_kind == "field")
                .count(),
            0
        );
    }
}
