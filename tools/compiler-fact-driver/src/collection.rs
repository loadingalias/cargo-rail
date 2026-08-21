//! Exact rustc-owned fact collection.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rustc_hir as hir;
use rustc_hir::Node;
use rustc_hir::def::{CtorOf, DefKind, Res};
use rustc_hir::def_id::{CRATE_DEF_ID, DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_lint_defs::builtin::DEAD_CODE;
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrFlags;
use rustc_middle::middle::privacy::Level as PrivacyLevel;
use rustc_middle::ty::{self, TyCtxt};
use rustc_session::config::CrateType;
use rustc_span::def_id::LOCAL_CRATE;
use rustc_span::{FileName, Pos};
use sha2::{Digest as _, Sha256};

use crate::fact_protocol::{
    COMPILER_FACT_PROTOCOL_VERSION, CompilerFactCompletion, CompilerFactCoverage, CompilerFactEdge,
    CompilerFactEdgeKind, CompilerFactEntryPoint, CompilerFactEntryPointKind, CompilerFactInvocation,
    CompilerFactItemKind, CompilerFactMacroProvenance, CompilerFactNamespace, CompilerFactObject,
    CompilerFactPhysicalIdentity, CompilerFactRetention, CompilerFactRetentionReason, CompilerFactSource,
    CompilerFactSourcePath, CompilerFactSpan, CompilerFactStringId, CompilerFactVisibility, CompilerItemFact,
    CompilerItemId,
};

struct RawSpan {
    path: CompilerFactSourcePath,
    start: u64,
    end: u64,
}

struct RawItem {
    id: CompilerItemId,
    span: RawSpan,
    source_context: String,
    namespace: CompilerFactNamespace,
    kind: CompilerFactItemKind,
    name: String,
    diagnostic_path: String,
    parent: Option<CompilerItemId>,
    written_visibility: CompilerFactVisibility,
    written_visibility_complete: bool,
    visibility_span: Option<RawSpan>,
    effective_visibility: CompilerFactVisibility,
    macro_provenance: RawMacroProvenance,
}

enum RawMacroProvenance {
    Written,
    Expansion(Option<RawSpan>),
    Generated,
}

struct SourceData {
    path: CompilerFactSourcePath,
    content_digest: String,
    bytes: u64,
}

pub(crate) fn collect(tcx: TyCtxt<'_>, invocation: &CompilerFactInvocation) -> Result<CompilerFactObject, String> {
    if tcx.crate_name(LOCAL_CRATE).as_str() != invocation.unit.crate_name {
        return Err("authorized compilation unit does not match rustc's crate name".to_string());
    }
    let source_root = fs::canonicalize(&invocation.source_root)
        .map_err(|error| format!("resolve authorized source root: {error}"))?;
    let generated_roots = invocation
        .generated_roots
        .iter()
        .map(|root| {
            fs::canonicalize(root).map_err(|error| format!("resolve authorized generated root '{root}': {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut collector = Collector {
        tcx,
        source_root,
        generated_roots,
        sources: BTreeMap::new(),
    };
    collector.collect(invocation)
}

struct Collector<'tcx> {
    tcx: TyCtxt<'tcx>,
    source_root: PathBuf,
    generated_roots: Vec<PathBuf>,
    sources: BTreeMap<CompilerFactSourcePath, SourceData>,
}

impl<'tcx> Collector<'tcx> {
    fn collect(&mut self, invocation: &CompilerFactInvocation) -> Result<CompilerFactObject, String> {
        let crate_items = self.tcx.hir_crate_items(());
        let mut definitions = HashSet::new();
        for owner in crate_items.owners() {
            if fact_kind(self.tcx, owner.def_id).is_some() && collectible_definition(self.tcx, owner.def_id) {
                definitions.insert(owner.def_id);
            }
        }
        for item_id in crate_items.free_items() {
            let item = self.tcx.hir_item(item_id);
            match item.kind {
                hir::ItemKind::Struct(_, _, data) | hir::ItemKind::Union(_, _, data) => {
                    definitions.extend(data.fields().iter().map(|field| field.def_id));
                }
                hir::ItemKind::Enum(_, _, enumeration) => {
                    for variant in enumeration.variants {
                        definitions.insert(variant.def_id);
                        definitions.extend(variant.data.fields().iter().map(|field| field.def_id));
                    }
                }
                _ => {}
            }
        }

        let defined_ids = definitions
            .iter()
            .map(|def_id| (*def_id, item_id(self.tcx, def_id.to_def_id())))
            .collect::<HashMap<_, _>>();
        let mut raw_items = Vec::with_capacity(definitions.len());
        for def_id in &definitions {
            raw_items.push(self.raw_item(*def_id, &defined_ids)?);
        }

        let mut edges = Vec::new();
        let mut externally_addressed = HashSet::new();
        self.collect_edges(&definitions, &mut edges, &mut externally_addressed);
        edges.sort();
        edges.dedup();

        let mut entry_points = Vec::new();
        if let Some((def_id, _)) = self.tcx.entry_fn(()) {
            let id = item_id(self.tcx, def_id);
            if defined_ids.values().any(|candidate| *candidate == id) {
                entry_points.push(CompilerFactEntryPoint {
                    item: id,
                    kind: entry_point_kind(invocation),
                });
            }
        }
        entry_points.sort();
        entry_points.dedup();

        let mut retentions = self.collect_retentions(&definitions, &externally_addressed);
        retentions.extend(
            raw_items
                .iter()
                .filter(|item| !item.written_visibility_complete)
                .map(|item| CompilerFactRetention {
                    item: item.id,
                    reason: CompilerFactRetentionReason::IncompleteProvenance,
                }),
        );
        retentions.extend(generated_registration_retentions(
            &raw_items,
            &edges,
            invocation.unit.domain,
        ));
        retentions.sort();
        retentions.dedup();

        let mut strings = raw_items
            .iter()
            .flat_map(|item| {
                [
                    item.source_context.clone(),
                    item.name.clone(),
                    item.diagnostic_path.clone(),
                ]
            })
            .collect::<Vec<_>>();
        strings.sort();
        strings.dedup();
        let string_ids = strings
            .iter()
            .enumerate()
            .map(|(index, value)| (value.as_str(), CompilerFactStringId(index as u32)))
            .collect::<HashMap<_, _>>();

        let sources = self
            .sources
            .values()
            .map(|source| CompilerFactSource {
                path: source.path.clone(),
                content_digest: source.content_digest.clone(),
                bytes: source.bytes,
            })
            .collect::<Vec<_>>();
        let source_ids = sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.path.clone(), index as u32))
            .collect::<BTreeMap<_, _>>();

        raw_items.sort_by(|left, right| {
            (
                &left.span.path,
                left.span.start,
                left.span.end,
                &left.source_context,
                left.namespace,
                left.kind,
                left.id,
            )
                .cmp(&(
                    &right.span.path,
                    right.span.start,
                    right.span.end,
                    &right.source_context,
                    right.namespace,
                    right.kind,
                    right.id,
                ))
        });
        let mut ordinal = 0_u16;
        let mut previous = None;
        let mut items = Vec::with_capacity(raw_items.len());
        for raw in raw_items {
            let physical_key = (
                raw.span.path.clone(),
                raw.span.start,
                raw.span.end,
                raw.source_context.clone(),
                raw.namespace,
                raw.kind,
            );
            if previous.as_ref() == Some(&physical_key) {
                ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| "too many definitions share one physical identity".to_string())?;
            } else {
                ordinal = 0;
                previous = Some(physical_key);
            }
            let source_context = string_ids[raw.source_context.as_str()];
            let physical = CompilerFactPhysicalIdentity {
                span: fact_span(&raw.span, &source_ids)?,
                source_context,
                namespace: raw.namespace,
                kind: raw.kind,
                ordinal,
            };
            items.push(CompilerItemFact {
                id: raw.id,
                physical,
                name: string_ids[raw.name.as_str()],
                diagnostic_path: string_ids[raw.diagnostic_path.as_str()],
                parent: raw.parent,
                written_visibility: raw.written_visibility,
                visibility_span: raw
                    .visibility_span
                    .as_ref()
                    .map(|span| fact_span(span, &source_ids))
                    .transpose()?,
                effective_visibility: raw.effective_visibility,
                macro_provenance: match raw.macro_provenance {
                    RawMacroProvenance::Written => CompilerFactMacroProvenance::Written,
                    RawMacroProvenance::Expansion(call_site) => CompilerFactMacroProvenance::Expansion(
                        call_site
                            .as_ref()
                            .map(|span| fact_span(span, &source_ids))
                            .transpose()?,
                    ),
                    RawMacroProvenance::Generated => CompilerFactMacroProvenance::Generated,
                },
            });
        }
        items.sort();

        let coverage = BTreeSet::from([
            CompilerFactCoverage::Definitions,
            CompilerFactCoverage::Visibility,
            CompilerFactCoverage::ExactSpans,
            CompilerFactCoverage::MacroProvenance,
            CompilerFactCoverage::BodyEdges,
            CompilerFactCoverage::InterfaceEdges,
            CompilerFactCoverage::ReexportEdges,
            CompilerFactCoverage::PrivacyEdges,
            CompilerFactCoverage::TraitDispatch,
            CompilerFactCoverage::ForeignExports,
            CompilerFactCoverage::GeneratedSources,
            CompilerFactCoverage::EntryPoints,
            CompilerFactCoverage::ConservativeRetention,
        ]);
        if !invocation.required_coverage.is_subset(&coverage) {
            return Err("matched driver cannot prove every requested compiler fact facet".to_string());
        }
        let completion = CompilerFactCompletion {
            complete: true,
            coverage,
            strings: strings.len() as u64,
            sources: sources.len() as u64,
            items: items.len() as u64,
            edges: edges.len() as u64,
            entry_points: entry_points.len() as u64,
            retentions: retentions.len() as u64,
        };
        Ok(CompilerFactObject {
            version: COMPILER_FACT_PROTOCOL_VERSION,
            producer_authority: invocation.producer_authority.clone(),
            unit: invocation.unit.clone(),
            strings,
            sources,
            items,
            edges,
            entry_points,
            retentions,
            completion,
        })
    }

    fn raw_item(
        &mut self,
        def_id: LocalDefId,
        defined_ids: &HashMap<LocalDefId, CompilerItemId>,
    ) -> Result<RawItem, String> {
        let span = node_span(self.tcx, def_id);
        let mut raw_span = self
            .capture_span(span)
            .map_err(|error| format!("{}: {error}", self.tcx.def_path_str(def_id.to_def_id())))?;
        let visibility_span = visibility_span(self.tcx, def_id).filter(|span| span.lo() < span.hi());
        let (written_visibility, written_visibility_complete) = written_visibility(self.tcx, def_id, visibility_span);
        let raw_visibility_span = visibility_span.map(|span| self.capture_span(span)).transpose()?;
        if let Some(visibility) = &raw_visibility_span {
            if visibility.path != raw_span.path {
                return Err("declaration and written visibility span different source files".to_string());
            }
            raw_span.start = raw_span.start.min(visibility.start);
            raw_span.end = raw_span.end.max(visibility.end);
        }
        let effective_visibility = self
            .tcx
            .effective_visibilities(())
            .effective_vis(def_id)
            .map(|visibility| fact_visibility(self.tcx, *visibility.at_level(PrivacyLevel::Reachable)))
            .unwrap_or(CompilerFactVisibility::Private);
        let kind = fact_kind(self.tcx, def_id).ok_or_else(|| "definition kind disappeared".to_string())?;
        let diagnostic_path = self.tcx.def_path_str(def_id.to_def_id());
        let name = self
            .tcx
            .hir_node_by_def_id(def_id)
            .ident()
            .map(|ident| ident.to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| diagnostic_path.clone());
        let parent = self
            .tcx
            .opt_local_parent(def_id)
            .and_then(|parent| defined_ids.get(&parent).copied());
        let repository_source = matches!(raw_span.path, CompilerFactSourcePath::Repository(_));
        let macro_provenance = if span.from_expansion() {
            let call_site = span.source_callsite();
            RawMacroProvenance::Expansion(
                (!call_site.is_dummy())
                    .then(|| self.capture_span(call_site))
                    .transpose()?,
            )
        } else if repository_source {
            RawMacroProvenance::Written
        } else {
            RawMacroProvenance::Generated
        };
        Ok(RawItem {
            id: defined_ids[&def_id],
            span: raw_span,
            source_context: format!("hygiene:{:?}", span.ctxt()),
            namespace: namespace(kind),
            kind,
            name,
            diagnostic_path,
            parent,
            written_visibility,
            written_visibility_complete,
            visibility_span: raw_visibility_span,
            effective_visibility,
            macro_provenance,
        })
    }

    fn capture_span(&mut self, span: rustc_span::Span) -> Result<RawSpan, String> {
        if span.is_dummy() || span.lo() >= span.hi() {
            return Err("rustc returned an empty definition span".to_string());
        }
        let source_file = self.tcx.sess.source_map().lookup_source_file(span.lo());
        if self.tcx.sess.source_map().lookup_source_file(span.hi()).start_pos != source_file.start_pos {
            return Err("rustc definition span crosses source files".to_string());
        }
        let physical = source_file_path(self.tcx, &source_file.name);
        let (path, bytes) = if let Some(physical) = physical {
            let physical = fs::canonicalize(&physical).unwrap_or(physical);
            let bytes = fs::read(&physical).or_else(|_| {
                source_file
                    .src
                    .as_deref()
                    .map(|source| source.as_bytes().to_vec())
                    .ok_or_else(|| std::io::Error::other("source bytes unavailable"))
            });
            let bytes = bytes.map_err(|error| format!("read source '{}': {error}", physical.display()))?;
            let generated = self.generated_roots.iter().any(|root| physical.starts_with(root));
            let path = if !generated && let Ok(relative) = physical.strip_prefix(&self.source_root) {
                CompilerFactSourcePath::Repository(protocol_path(relative))
            } else {
                generated_source_path(&bytes)
            };
            (path, bytes)
        } else {
            let bytes = source_file
                .src
                .as_deref()
                .map(|source| source.as_bytes().to_vec())
                .ok_or_else(|| "compiler-generated source bytes are unavailable".to_string())?;
            (generated_source_path(&bytes), bytes)
        };
        self.sources.entry(path.clone()).or_insert_with(|| SourceData {
            path: path.clone(),
            content_digest: format!("sha256:{}", hex_digest(&bytes)),
            bytes: bytes.len() as u64,
        });
        Ok(RawSpan {
            path,
            start: source_file.original_relative_byte_pos(span.lo()).to_u32() as u64,
            end: source_file.original_relative_byte_pos(span.hi()).to_u32() as u64,
        })
    }

    fn collect_edges(
        &self,
        definitions: &HashSet<LocalDefId>,
        edges: &mut Vec<CompilerFactEdge>,
        externally_addressed: &mut HashSet<CompilerItemId>,
    ) {
        for def_id in self.tcx.hir_body_owners().filter(|def_id| definitions.contains(def_id)) {
            let body = self.tcx.hir_body_owned_by(def_id);
            let mut visitor = ReferenceVisitor::new(
                self.tcx,
                def_id.to_def_id(),
                CompilerFactEdgeKind::Body,
                Some(self.tcx.typeck_body(body.id())),
                true,
            );
            visitor.visit_body(body);
            if visitor.finish(edges) {
                externally_addressed.insert(item_id(self.tcx, def_id.to_def_id()));
            }
        }
        for def_id in definitions {
            let mut visitor = ReferenceVisitor::new(
                self.tcx,
                def_id.to_def_id(),
                if self.tcx.def_kind(*def_id) == DefKind::Use {
                    CompilerFactEdgeKind::Reexport
                } else {
                    CompilerFactEdgeKind::Interface
                },
                None,
                false,
            );
            visitor.visit_node(self.tcx.hir_node_by_def_id(*def_id));
            let _ = visitor.finish(edges);
            if let Some(parent) = self.tcx.opt_local_parent(*def_id)
                && definitions.contains(&parent)
            {
                edges.push(CompilerFactEdge {
                    source: item_id(self.tcx, def_id.to_def_id()),
                    target: item_id(self.tcx, parent.to_def_id()),
                    kind: CompilerFactEdgeKind::VisibilityParent,
                });
            }
            if let Some(trait_item) = self.tcx.trait_item_of(def_id.to_def_id())
                && let Some(trait_id) = self.tcx.trait_of_assoc(trait_item)
            {
                edges.push(CompilerFactEdge {
                    source: item_id(self.tcx, def_id.to_def_id()),
                    target: item_id(self.tcx, trait_id),
                    kind: CompilerFactEdgeKind::VisibilityRequirement,
                });
            }
        }
    }

    fn collect_retentions(
        &self,
        definitions: &HashSet<LocalDefId>,
        externally_addressed: &HashSet<CompilerItemId>,
    ) -> Vec<CompilerFactRetention> {
        let proc_macro = self.tcx.crate_types().contains(&CrateType::ProcMacro);
        let incomplete_global_asm = self
            .tcx
            .hir_crate_items(())
            .owners()
            .any(|owner| self.tcx.def_kind(owner.def_id) == DefKind::GlobalAsm);
        let mut retentions = Vec::new();
        for def_id in definitions {
            let item = item_id(self.tcx, def_id.to_def_id());
            let parent_kind = self
                .tcx
                .opt_local_parent(*def_id)
                .map(|parent| self.tcx.def_kind(parent));
            if matches!(parent_kind, Some(DefKind::Trait | DefKind::Impl { of_trait: true })) {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::UnresolvedTraitDispatch,
                });
            }
            if matches!(
                fact_kind(self.tcx, *def_id),
                Some(CompilerFactItemKind::ForeignFunction | CompilerFactItemKind::ForeignStatic)
            ) {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::ExternallyAddressed,
                });
            }
            if externally_addressed.contains(&item) {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::ExternallyAddressed,
                });
            }
            if incomplete_global_asm {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::IncompleteProvenance,
                });
            }
            if self
                .tcx
                .lint_level_spec_at_node(DEAD_CODE, self.tcx.local_def_id_to_hir_id(*def_id))
                .level()
                == rustc_session::lint::Level::Allow
            {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::AllowDeadCode,
                });
            }
            if proc_macro && self.tcx.local_visibility(*def_id).is_public() {
                retentions.push(CompilerFactRetention {
                    item,
                    reason: CompilerFactRetentionReason::ProcMacro,
                });
            }
            if matches!(
                self.tcx.def_kind(*def_id),
                DefKind::Fn | DefKind::AssocFn | DefKind::Static { .. }
            ) {
                let attrs = self.tcx.codegen_fn_attrs(def_id.to_def_id());
                if attrs.flags.contains(CodegenFnAttrFlags::NO_MANGLE) {
                    retentions.push(CompilerFactRetention {
                        item,
                        reason: CompilerFactRetentionReason::NoMangle,
                    });
                }
                if attrs
                    .flags
                    .intersects(CodegenFnAttrFlags::USED_COMPILER | CodegenFnAttrFlags::USED_LINKER)
                {
                    retentions.push(CompilerFactRetention {
                        item,
                        reason: CompilerFactRetentionReason::Used,
                    });
                }
                if attrs.symbol_name.is_some() {
                    retentions.push(CompilerFactRetention {
                        item,
                        reason: CompilerFactRetentionReason::ExportName,
                    });
                }
            }
        }
        retentions
    }
}

fn generated_registration_retentions(
    items: &[RawItem],
    edges: &[CompilerFactEdge],
    domain: crate::fact_protocol::CompilerFactDomain,
) -> Vec<CompilerFactRetention> {
    if domain != crate::fact_protocol::CompilerFactDomain::NonProduction {
        return Vec::new();
    }
    let by_id = items.iter().map(|item| (item.id, item)).collect::<HashMap<_, _>>();
    items
        .iter()
        .filter(|item| {
            item.kind == CompilerFactItemKind::Constant
                && matches!(item.macro_provenance, RawMacroProvenance::Expansion(_))
                && edges.iter().any(|edge| {
                    edge.source == item.id
                        && edge.kind == CompilerFactEdgeKind::Body
                        && by_id.get(&edge.target).is_some_and(|target| {
                            target.kind == CompilerFactItemKind::Function
                                && matches!(target.macro_provenance, RawMacroProvenance::Written)
                                && target.diagnostic_path == item.diagnostic_path
                        })
                })
        })
        .map(|item| CompilerFactRetention {
            item: item.id,
            reason: CompilerFactRetentionReason::GeneratedRegistration,
        })
        .collect()
}

fn fact_span(span: &RawSpan, sources: &BTreeMap<CompilerFactSourcePath, u32>) -> Result<CompilerFactSpan, String> {
    Ok(CompilerFactSpan {
        source: *sources
            .get(&span.path)
            .ok_or_else(|| "definition names an unrecorded source".to_string())?,
        start: span.start,
        end: span.end,
    })
}

fn source_file_path(tcx: TyCtxt<'_>, name: &FileName) -> Option<PathBuf> {
    let FileName::Real(name) = name else {
        return None;
    };
    let path = name.local_path()?;
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        let working_directory = tcx.sess.opts.working_dir.local_path().unwrap_or(Path::new(""));
        Some(working_directory.join(path))
    }
}

fn protocol_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn generated_source_path(bytes: &[u8]) -> CompilerFactSourcePath {
    let root = if cfg!(windows) {
        "C:/cargo-rail/generated"
    } else {
        "/cargo-rail/generated"
    };
    CompilerFactSourcePath::Generated(format!("{root}/{}.rs", hex_digest(bytes)))
}

fn item_id(tcx: TyCtxt<'_>, def_id: DefId) -> CompilerItemId {
    let hash = tcx.def_path_hash(def_id);
    CompilerItemId([hash.stable_crate_id().as_u64(), hash.local_hash().as_u64()])
}

fn fact_kind(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<CompilerFactItemKind> {
    let foreign = tcx
        .opt_local_parent(def_id)
        .is_some_and(|parent| tcx.def_kind(parent) == DefKind::ForeignMod);
    Some(match tcx.def_kind(def_id) {
        DefKind::Mod if def_id != CRATE_DEF_ID => CompilerFactItemKind::Module,
        DefKind::Fn if foreign => CompilerFactItemKind::ForeignFunction,
        DefKind::Fn => CompilerFactItemKind::Function,
        DefKind::AssocFn => CompilerFactItemKind::Method,
        DefKind::AssocConst { .. } => CompilerFactItemKind::AssociatedConstant,
        DefKind::AssocTy => CompilerFactItemKind::AssociatedType,
        DefKind::Trait => CompilerFactItemKind::Trait,
        DefKind::Struct => CompilerFactItemKind::Struct,
        DefKind::Enum => CompilerFactItemKind::Enum,
        DefKind::Union => CompilerFactItemKind::Union,
        DefKind::TyAlias => CompilerFactItemKind::TypeAlias,
        DefKind::Const { .. } => CompilerFactItemKind::Constant,
        DefKind::Static { .. } if foreign => CompilerFactItemKind::ForeignStatic,
        DefKind::Static { .. } => CompilerFactItemKind::Static,
        DefKind::Field => CompilerFactItemKind::Field,
        DefKind::Variant => CompilerFactItemKind::Variant,
        DefKind::Use => CompilerFactItemKind::Reexport,
        DefKind::Impl { .. } => CompilerFactItemKind::Impl,
        DefKind::Macro(_) => CompilerFactItemKind::Macro,
        _ => return None,
    })
}

fn collectible_definition(tcx: TyCtxt<'_>, def_id: LocalDefId) -> bool {
    let span = node_span(tcx, def_id);
    if span.is_dummy() || span.lo() >= span.hi() {
        return false;
    }
    !matches!(
      tcx.hir_node_by_def_id(def_id),
      Node::Item(item) if matches!(item.kind, hir::ItemKind::Use(_, kind) if !matches!(kind, hir::UseKind::Single(_)))
    )
}

fn namespace(kind: CompilerFactItemKind) -> CompilerFactNamespace {
    match kind {
        CompilerFactItemKind::Trait
        | CompilerFactItemKind::Struct
        | CompilerFactItemKind::Enum
        | CompilerFactItemKind::Union
        | CompilerFactItemKind::TypeAlias
        | CompilerFactItemKind::AssociatedType
        | CompilerFactItemKind::Module
        | CompilerFactItemKind::Impl => CompilerFactNamespace::Type,
        CompilerFactItemKind::Macro => CompilerFactNamespace::Macro,
        _ => CompilerFactNamespace::Value,
    }
}

fn node_span(tcx: TyCtxt<'_>, def_id: LocalDefId) -> rustc_span::Span {
    match tcx.hir_node_by_def_id(def_id) {
        Node::Item(item) => item.span,
        Node::TraitItem(item) => item.span,
        Node::ImplItem(item) => item.span,
        Node::ForeignItem(item) => item.span,
        Node::Variant(variant) => variant.span,
        Node::Field(field) => field.span,
        _ => tcx.def_span(def_id),
    }
}

fn visibility_span(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<rustc_span::Span> {
    match tcx.hir_node_by_def_id(def_id) {
        Node::Item(item) => Some(item.vis_span),
        Node::ImplItem(item) => item.vis_span(),
        Node::Field(field) => Some(field.vis_span),
        _ => None,
    }
}

fn written_visibility(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    span: Option<rustc_span::Span>,
) -> (CompilerFactVisibility, bool) {
    let Some(span) = span else {
        return (CompilerFactVisibility::Private, true);
    };
    let semantic = fact_visibility(tcx, tcx.local_visibility(def_id));
    let Some(compact) = tcx.sess.source_map().span_to_snippet(span).ok().map(|source| {
        source
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>()
    }) else {
        return (semantic, false);
    };
    match compact.as_str() {
        "pub" if semantic == CompilerFactVisibility::Public => (CompilerFactVisibility::Public, true),
        "pub(crate)" => (CompilerFactVisibility::Crate, true),
        visibility if visibility.starts_with("pub(") && visibility.ends_with(')') => match tcx.local_visibility(def_id)
        {
            ty::Visibility::Restricted(scope) if scope == CRATE_DEF_ID => {
                (CompilerFactVisibility::RestrictedCrateRoot, true)
            }
            ty::Visibility::Restricted(scope) => (
                CompilerFactVisibility::Restricted(item_id(tcx, scope.to_def_id())),
                true,
            ),
            ty::Visibility::Public => (semantic, false),
        },
        _ => (semantic, false),
    }
}

fn fact_visibility(tcx: TyCtxt<'_>, visibility: ty::Visibility) -> CompilerFactVisibility {
    match visibility {
        ty::Visibility::Public => CompilerFactVisibility::Public,
        ty::Visibility::Restricted(scope) if scope == CRATE_DEF_ID => CompilerFactVisibility::Crate,
        ty::Visibility::Restricted(scope) => CompilerFactVisibility::Restricted(item_id(tcx, scope.to_def_id())),
    }
}

fn entry_point_kind(invocation: &CompilerFactInvocation) -> CompilerFactEntryPointKind {
    use crate::fact_protocol::CompilerFactDomain;
    match invocation.unit.domain {
        CompilerFactDomain::Doctest => CompilerFactEntryPointKind::Doctest,
        CompilerFactDomain::BuildScript => CompilerFactEntryPointKind::BuildScript,
        CompilerFactDomain::ProcMacro => CompilerFactEntryPointKind::ProcMacro,
        CompilerFactDomain::NonProduction
            if invocation.unit.target_kind == crate::fact_protocol::CompilerFactTargetKind::Benchmark =>
        {
            CompilerFactEntryPointKind::BenchmarkHarness
        }
        CompilerFactDomain::NonProduction => CompilerFactEntryPointKind::TestHarness,
        CompilerFactDomain::Production => CompilerFactEntryPointKind::Main,
    }
}

struct ReferenceVisitor<'tcx> {
    tcx: TyCtxt<'tcx>,
    source: DefId,
    edge_kind: CompilerFactEdgeKind,
    typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
    traverse_bodies: bool,
    targets: HashSet<DefId>,
    opaque_external_reference: bool,
}

impl<'tcx> ReferenceVisitor<'tcx> {
    fn new(
        tcx: TyCtxt<'tcx>,
        source: DefId,
        edge_kind: CompilerFactEdgeKind,
        typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
        traverse_bodies: bool,
    ) -> Self {
        Self {
            tcx,
            source,
            edge_kind,
            typeck_results,
            traverse_bodies,
            targets: HashSet::new(),
            opaque_external_reference: false,
        }
    }

    fn finish(self, edges: &mut Vec<CompilerFactEdge>) -> bool {
        let source = item_id(self.tcx, self.source);
        edges.extend(self.targets.into_iter().map(|target| CompilerFactEdge {
            source,
            target: item_id(self.tcx, target),
            kind: self.edge_kind,
        }));
        self.opaque_external_reference
    }

    fn record(&mut self, resolution: Res) {
        match resolution {
            Res::Def(DefKind::Ctor(CtorOf::Struct, ..), constructor) => {
                let adt = self.tcx.parent(constructor);
                self.targets.insert(adt);
                self.targets.extend(
                    self.tcx
                        .adt_def(adt)
                        .non_enum_variant()
                        .fields
                        .iter()
                        .map(|field| field.did),
                );
            }
            Res::Def(DefKind::Ctor(CtorOf::Variant, ..), constructor) => {
                self.targets.insert(self.tcx.parent(constructor));
            }
            Res::Def(_, def_id) | Res::SelfTyParam { trait_: def_id } | Res::SelfTyAlias { alias_to: def_id, .. } => {
                self.targets.insert(def_id);
            }
            _ => {}
        }
    }

    fn visit_node(&mut self, node: Node<'tcx>) {
        match node {
            Node::Item(item) => self.visit_item(item),
            Node::ImplItem(item) => self.visit_impl_item(item),
            Node::TraitItem(item) => self.visit_trait_item(item),
            Node::ForeignItem(item) => self.visit_foreign_item(item),
            Node::Field(field) => self.visit_field_def(field),
            _ => {}
        }
    }
}

impl<'tcx> Visitor<'tcx> for ReferenceVisitor<'tcx> {
    fn visit_inline_asm(&mut self, assembly: &'tcx hir::InlineAsm<'tcx>, hir_id: hir::HirId) {
        self.opaque_external_reference = true;
        intravisit::walk_inline_asm(self, assembly, hir_id);
    }

    fn visit_nested_body(&mut self, body_id: hir::BodyId) {
        if self.traverse_bodies {
            let previous = self.typeck_results.replace(self.tcx.typeck_body(body_id));
            self.visit_body(self.tcx.hir_body(body_id));
            self.typeck_results = previous;
        }
    }

    fn visit_path(&mut self, path: &hir::Path<'tcx>, hir_id: hir::HirId) {
        self.record(path.res);
        intravisit::walk_path(self, path);
        let _ = hir_id;
    }

    fn visit_expr(&mut self, expression: &'tcx hir::Expr<'tcx>) {
        if let Some(typeck) = self.typeck_results {
            match expression.kind {
                hir::ExprKind::Path(ref path @ hir::QPath::TypeRelative(..)) => {
                    self.record(typeck.qpath_res(path, expression.hir_id));
                }
                hir::ExprKind::MethodCall(..) => {
                    if let Some(def_id) = typeck.type_dependent_def_id(expression.hir_id) {
                        self.targets.insert(def_id);
                    }
                }
                hir::ExprKind::Field(base, _) => {
                    if let Some(adt) = typeck.expr_ty_adjusted(base).ty_adt_def()
                        && !adt.is_enum()
                        && let Some(index) = typeck.opt_field_index(expression.hir_id)
                    {
                        self.targets.insert(adt.non_enum_variant().fields[index].did);
                    }
                }
                _ => {}
            }
        }
        intravisit::walk_expr(self, expression);
    }

    fn visit_pat(&mut self, pattern: &'tcx hir::Pat<'tcx>) {
        if let Some(typeck) = self.typeck_results
            && let hir::PatKind::Struct(path, fields, _) = pattern.kind
        {
            if matches!(path, hir::QPath::TypeRelative(..)) {
                self.record(typeck.qpath_res(&path, pattern.hir_id));
            }
            if let Some(adt) = typeck.pat_ty(pattern).ty_adt_def()
                && !adt.is_enum()
            {
                for field in fields {
                    if let Some(index) = typeck.opt_field_index(field.hir_id) {
                        self.targets.insert(adt.non_enum_variant().fields[index].did);
                    }
                }
            }
        }
        intravisit::walk_pat(self, pattern);
    }
}
