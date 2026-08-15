//! Compiler-independent wire model shared with the matched fact driver.
//!
//! Keep this module free of cargo-rail runtime types and compiler-private APIs.
//! Release manufacturing compiles the exact same source into the isolated
//! companion, leaving one serialization authority across the process boundary.

#![allow(
  dead_code,
  reason = "the shared wire model has disjoint producers and consumers in the main crate and isolated companion"
)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub(crate) const COMPILER_FACT_PROTOCOL_VERSION: u32 = 3;
pub(crate) const COMPILER_FACT_ANNOUNCEMENT_CODE: &str = "cargo_rail_compiler_fact_v1";
pub(crate) const COMPILER_FACT_ANNOUNCEMENT_PREFIX: &str = "cargo-rail-compiler-fact-v1:";
pub(crate) const COMPILER_FACT_INVOCATION_ENV: &str = "CARGO_RAIL_COMPILER_FACT_INVOCATION";

pub(crate) const RUN_IDENTITY_PREFIX: &str = "compiler-fact-run-v1-sha256-";
pub(crate) const VIEW_IDENTITY_PREFIX: &str = "compiler-fact-view-v1-sha256-";
pub(crate) const COMPILER_IDENTITY_PREFIX: &str = "compiler-fact-compiler-v1-sha256-";
pub(crate) const DRIVER_IDENTITY_PREFIX: &str = "compiler-fact-driver-v1-sha256-";
pub(crate) const INVOCATION_IDENTITY_PREFIX: &str = "compiler-fact-invocation-v1-sha256-";
pub(crate) const UNIT_IDENTITY_PREFIX: &str = "compiler-fact-unit-v1-sha256-";
pub(crate) const FRAGMENT_OBJECT_IDENTITY_PREFIX: &str = "compiler-fact-object-v1-sha256-";

/// Small content-addressed sidecar announcement carried by Cargo JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactAnnouncement {
  pub(crate) version: u32,
  pub(crate) run_authority: CompilerFactRunAuthority,
  pub(crate) producer_authority: CompilerFactProducerAuthority,
  pub(crate) unit_identity: String,
  pub(crate) object_identity: String,
  pub(crate) content_digest: String,
  pub(crate) bytes: u64,
}

/// One-shot run and analysis-view authority that every fragment must echo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactRunAuthority {
  pub(crate) run_identity: String,
  pub(crate) view_identity: String,
}

/// Exact compiler and driver authority retained by a reusable fact object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactProducerAuthority {
  pub(crate) compiler_identity: String,
  pub(crate) driver_identity: String,
}

/// Per-rustc capability written by the stable wrapper for the matched driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactInvocation {
  pub(crate) version: u32,
  pub(crate) observation_directory: String,
  pub(crate) source_root: String,
  pub(crate) generated_roots: Vec<String>,
  pub(crate) run_authority: CompilerFactRunAuthority,
  pub(crate) producer_authority: CompilerFactProducerAuthority,
  pub(crate) unit: CompilerFactUnit,
  pub(crate) required_coverage: BTreeSet<CompilerFactCoverage>,
}

/// One exact Cargo/rustc compilation domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactDomain {
  Production,
  NonProduction,
  Doctest,
  BuildScript,
  ProcMacro,
}

/// Whether a unit executes on the compiler host or produces target code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactRole {
  Host,
  Target,
}

/// Typed Cargo target kind without depending on compiler-private types.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub(crate) enum CompilerFactTargetKind {
  Library,
  Binary,
  Test,
  Example,
  Benchmark,
  Documentation,
  ProcMacro,
  BuildScript,
  Other(String),
}

/// Exact logical identity of the compiler invocation that emitted a fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactUnit {
  pub(crate) identity: String,
  pub(crate) invocation_identity: String,
  pub(crate) package: CompilerFactPackage,
  pub(crate) cargo_target: String,
  pub(crate) crate_name: String,
  pub(crate) target_kind: CompilerFactTargetKind,
  pub(crate) domain: CompilerFactDomain,
  pub(crate) role: CompilerFactRole,
  pub(crate) platform: String,
  pub(crate) features: Vec<String>,
  pub(crate) cfg: Vec<String>,
}

/// Root-independent Cargo package identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactPackage {
  pub(crate) name: String,
  pub(crate) version: String,
  pub(crate) source: Option<String>,
}

/// Opaque rustc item identity scoped to one exact compiler configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CompilerItemId(pub(crate) [u64; 2]);

/// Index into the fragment's sorted, unique string table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CompilerFactStringId(pub(crate) u32);

/// A repository source or a root-independent compiler-generated source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "root", content = "path", rename_all = "snake_case")]
pub(crate) enum CompilerFactSourcePath {
  Repository(String),
  Generated(String),
}

/// Exact source bytes named by spans in this fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactSource {
  pub(crate) path: CompilerFactSourcePath,
  pub(crate) content_digest: String,
  pub(crate) bytes: u64,
}

/// UTF-8 byte range in one source-table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactSpan {
  pub(crate) source: u32,
  pub(crate) start: u64,
  pub(crate) end: u64,
}

/// Rust namespace occupied by a source-written declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactNamespace {
  Type,
  Value,
  Macro,
}

/// Definition kind retained by the typed-fact substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactItemKind {
  Function,
  Method,
  AssociatedConstant,
  Trait,
  Struct,
  Enum,
  Union,
  TypeAlias,
  AssociatedType,
  Constant,
  Static,
  Field,
  Variant,
  Reexport,
  Module,
  Impl,
  Macro,
  ForeignFunction,
  ForeignStatic,
}

/// Source-qualified identity used to merge equivalent views safely.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactPhysicalIdentity {
  pub(crate) span: CompilerFactSpan,
  pub(crate) source_context: CompilerFactStringId,
  pub(crate) namespace: CompilerFactNamespace,
  pub(crate) kind: CompilerFactItemKind,
  pub(crate) ordinal: u16,
}

/// Written or compiler-effective Rust visibility.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "level", content = "scope", rename_all = "snake_case")]
pub(crate) enum CompilerFactVisibility {
  Private,
  Restricted(CompilerItemId),
  RestrictedCrateRoot,
  Crate,
  Public,
}

/// Whether the named declaration is source-written or expansion-generated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "call_site", rename_all = "snake_case")]
pub(crate) enum CompilerFactMacroProvenance {
  Written,
  Expansion(Option<CompilerFactSpan>),
  Generated,
}

/// One typed definition. Item strings are interned in the fragment string table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerItemFact {
  pub(crate) id: CompilerItemId,
  pub(crate) physical: CompilerFactPhysicalIdentity,
  pub(crate) name: CompilerFactStringId,
  pub(crate) diagnostic_path: CompilerFactStringId,
  pub(crate) parent: Option<CompilerItemId>,
  pub(crate) written_visibility: CompilerFactVisibility,
  pub(crate) visibility_span: Option<CompilerFactSpan>,
  pub(crate) effective_visibility: CompilerFactVisibility,
  pub(crate) macro_provenance: CompilerFactMacroProvenance,
}

/// Typed relationship between source declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactEdgeKind {
  Body,
  Interface,
  Reexport,
  VisibilityParent,
  VisibilityRequirement,
}

/// One directed typed relationship. Targets may belong to another fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactEdge {
  pub(crate) source: CompilerItemId,
  pub(crate) target: CompilerItemId,
  pub(crate) kind: CompilerFactEdgeKind,
}

/// Compiler-owned entry-point class for one compiled target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactEntryPointKind {
  Main,
  TestHarness,
  BenchmarkHarness,
  Doctest,
  BuildScript,
  ProcMacro,
}

/// One concrete entry point from which its compilation domain is reachable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactEntryPoint {
  pub(crate) item: CompilerItemId,
  pub(crate) kind: CompilerFactEntryPointKind,
}

/// Named reason that prevents an aggressive reachability conclusion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub(crate) enum CompilerFactRetentionReason {
  AllowDeadCode,
  ForeignExport,
  NoMangle,
  ExportName,
  Used,
  ProcMacro,
  UnresolvedTraitDispatch,
  RequiredImplementationInterface,
  GeneratedRegistration,
  IncompleteProvenance,
  ExternallyAddressed,
  Other(CompilerFactStringId),
}

/// Conservative root attached to one definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactRetention {
  pub(crate) item: CompilerItemId,
  pub(crate) reason: CompilerFactRetentionReason,
}

/// Independently claimed completeness facets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerFactCoverage {
  Definitions,
  Visibility,
  ExactSpans,
  MacroProvenance,
  BodyEdges,
  InterfaceEdges,
  ReexportEdges,
  PrivacyEdges,
  TraitDispatch,
  ForeignExports,
  GeneratedSources,
  EntryPoints,
  ConservativeRetention,
}

/// Completion marker written only after the driver has finished one unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactCompletion {
  pub(crate) complete: bool,
  pub(crate) coverage: BTreeSet<CompilerFactCoverage>,
  pub(crate) strings: u64,
  pub(crate) sources: u64,
  pub(crate) items: u64,
  pub(crate) edges: u64,
  pub(crate) entry_points: u64,
  pub(crate) retentions: u64,
}

/// One deterministic fragment emitted atomically by a matched driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactFragment {
  pub(crate) version: u32,
  pub(crate) run_authority: CompilerFactRunAuthority,
  pub(crate) object: CompilerFactObject,
}

/// Run-independent immutable fact content suitable for exact CAS reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactObject {
  pub(crate) version: u32,
  pub(crate) producer_authority: CompilerFactProducerAuthority,
  pub(crate) unit: CompilerFactUnit,
  pub(crate) strings: Vec<String>,
  pub(crate) sources: Vec<CompilerFactSource>,
  pub(crate) items: Vec<CompilerItemFact>,
  pub(crate) edges: Vec<CompilerFactEdge>,
  pub(crate) entry_points: Vec<CompilerFactEntryPoint>,
  pub(crate) retentions: Vec<CompilerFactRetention>,
  pub(crate) completion: CompilerFactCompletion,
}
