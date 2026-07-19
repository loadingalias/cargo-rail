//! `cargo rail unify` - Workspace dependency unification
//!
//! Implements resolution-based unification:
//! 1. Multi-target metadata for versions
//! 2. Manifest parsing for minimal features
//! 3. Intersection-based feature unification

use crate::cargo::{ManifestWriter, UnifyAnalyzer, UnifyReport};
use crate::commands::common::{UnifyOutputFormat, format_preview_list};
use crate::error::{RailError, RailResult};
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::progress;
use crate::workspace::WorkspaceContext;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

struct UnifyTextSink {
  buffer: Option<String>,
}

impl UnifyTextSink {
  fn new(capture: bool) -> Self {
    Self {
      buffer: capture.then_some(String::new()),
    }
  }

  fn push_line(&mut self, args: std::fmt::Arguments<'_>) {
    if let Some(ref mut buf) = self.buffer {
      use std::fmt::Write as _;
      let _ = buf.write_fmt(args);
      buf.push('\n');
    } else {
      println!("{}", args);
    }
  }

  fn finish(self) -> Option<String> {
    self.buffer
  }
}

macro_rules! outln {
  ($sink:expr $(,)?) => {{
    $sink.push_line(format_args!(""));
  }};
  ($sink:expr, $($arg:tt)*) => {{
    $sink.push_line(format_args!($($arg)*));
  }};
}

fn write_output(content: &str, output_file: Option<&PathBuf>) -> RailResult<()> {
  use std::io::Write as _;

  let needs_trailing_newline = !content.ends_with('\n');
  match output_file {
    Some(path) => {
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| RailError::message(format!("failed to open '{}': {}", path.display(), e)))?;
      file
        .write_all(content.as_bytes())
        .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      if needs_trailing_newline {
        file
          .write_all(b"\n")
          .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      }
      progress!("output: {}", path.display());
      Ok(())
    }
    None => {
      print!("{}", content);
      if needs_trailing_newline {
        println!();
      }
      Ok(())
    }
  }
}

fn format_quoted_list(values: &[impl AsRef<str>]) -> String {
  let mut out = String::with_capacity(values.iter().map(|v| v.as_ref().len()).sum::<usize>() + values.len() * 4 + 2);
  out.push('[');
  for (idx, value) in values.iter().enumerate() {
    if idx > 0 {
      out.push_str(", ");
    }
    out.push('"');
    out.push_str(value.as_ref());
    out.push('"');
  }
  out.push(']');
  out
}

fn dependency_section<'a>(dep_kind: &crate::cargo::DepKind, target: Option<&'a str>) -> std::borrow::Cow<'a, str> {
  use std::borrow::Cow;

  match (dep_kind, target) {
    (crate::cargo::DepKind::Dev, None) => Cow::Borrowed("[dev-dependencies]"),
    (crate::cargo::DepKind::Build, None) => Cow::Borrowed("[build-dependencies]"),
    (_, None) => Cow::Borrowed("[dependencies]"),
    (crate::cargo::DepKind::Dev, Some(t)) => Cow::Owned(format!("[target.'{}'.dev-dependencies]", t)),
    (crate::cargo::DepKind::Build, Some(t)) => Cow::Owned(format!("[target.'{}'.build-dependencies]", t)),
    (_, Some(t)) => Cow::Owned(format!("[target.'{}'.dependencies]", t)),
  }
}

fn mutation_targets(
  plan: &crate::cargo::UnificationPlan,
  workspace_root: &std::path::Path,
  msrv_write_needed: bool,
) -> Vec<String> {
  let mut targets = Vec::new();
  if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() || msrv_write_needed {
    targets.push("Cargo.toml".to_string());
  }

  let mut members: Vec<String> = plan
    .member_edits
    .keys()
    .filter_map(|member| {
      plan.member_paths.get(member).map(|path| {
        let relative = if path.is_absolute() {
          path.strip_prefix(workspace_root).unwrap_or(path)
        } else {
          path.strip_prefix(std::path::Path::new(".")).unwrap_or(path)
        };
        crate::utils::path_to_git_format(relative)
      })
    })
    .collect();
  members.sort();
  targets.extend(members);
  targets
}

fn blocked_issue_lines(plan: &crate::cargo::UnificationPlan) -> Vec<String> {
  plan
    .issues
    .iter()
    .filter(|issue| issue.severity == crate::cargo::IssueSeverity::Error)
    .map(|issue| format!("{}: {}", issue.dep_name, issue.message))
    .collect()
}

fn write_compact_summary(
  sink: &mut UnifyTextSink,
  plan: &crate::cargo::UnificationPlan,
  workspace_root: &std::path::Path,
  msrv_write_needed: bool,
  has_changes: bool,
) {
  outln!(sink, "unify");
  outln!(sink);
  outln!(sink, "changed:");

  let workspace_dep_names: Vec<String> = plan.workspace_deps.iter().map(|dep| dep.name.to_string()).collect();
  if workspace_dep_names.is_empty()
    && plan.member_edit_count() == 0
    && plan.transitive_pins.is_empty()
    && !msrv_write_needed
    && plan.pruned_features.is_empty()
    && plan.unused_deps.is_empty()
  {
    outln!(sink, "  none");
  } else {
    if !workspace_dep_names.is_empty() {
      outln!(
        sink,
        "  workspace deps ({}): {}",
        workspace_dep_names.len(),
        format_preview_list(&workspace_dep_names, 8)
      );
    }

    let members_affected = plan.member_edits.len();
    if plan.member_edit_count() > 0 {
      outln!(
        sink,
        "  member edits: {} across {} crate(s)",
        plan.member_edit_count(),
        members_affected
      );
    }

    if !plan.transitive_pins.is_empty() {
      let pin_names: Vec<String> = plan.transitive_pins.iter().map(|pin| pin.name.to_string()).collect();
      outln!(
        sink,
        "  transitive pins ({}): {}",
        pin_names.len(),
        format_preview_list(&pin_names, 8)
      );
    }

    let undeclared_fix_count: usize = plan
      .member_edits
      .values()
      .flat_map(|edits| edits.iter())
      .filter_map(|edit| match edit {
        crate::cargo::MemberEdit::AddFeatures { features_to_add, .. } => Some(features_to_add.len()),
        _ => None,
      })
      .sum();
    if undeclared_fix_count > 0 {
      let crates_fixed = plan
        .member_edits
        .values()
        .filter(|edits| {
          edits
            .iter()
            .any(|edit| matches!(edit, crate::cargo::MemberEdit::AddFeatures { .. }))
        })
        .count();
      outln!(
        sink,
        "  undeclared-feature fixes: {} feature(s) across {} crate(s)",
        undeclared_fix_count,
        crates_fixed
      );
    }

    if !plan.unused_deps.is_empty() {
      outln!(sink, "  unused deps removed: {}", plan.unused_deps.len());
    }
    if !plan.pruned_features.is_empty() {
      outln!(sink, "  dead features pruned: {}", plan.pruned_features.len());
    }
    if msrv_write_needed && let Some(msrv) = plan.computed_msrv.as_ref() {
      outln!(
        sink,
        "  rust-version: {}.{}.{}",
        msrv.version.major,
        msrv.version.minor,
        msrv.version.patch
      );
    }
  }

  outln!(sink);
  outln!(sink, "will mutate:");
  let targets = mutation_targets(plan, workspace_root, msrv_write_needed);
  if targets.is_empty() {
    outln!(sink, "  none");
  } else {
    outln!(sink, "  {}", format_preview_list(&targets, 8));
  }

  outln!(sink);
  outln!(sink, "blocked:");
  let blocked = blocked_issue_lines(plan);
  if blocked.is_empty() {
    outln!(sink, "  none");
  } else {
    for line in blocked.iter().take(5) {
      outln!(sink, "  {}", line);
    }
    if blocked.len() > 5 {
      outln!(sink, "  ... +{} more", blocked.len() - 5);
    }
  }

  outln!(sink);
  outln!(sink, "next:");
  if !blocked.is_empty() {
    outln!(sink, "  cargo rail unify --check --explain");
  } else if has_changes {
    outln!(sink, "  cargo rail unify");
    outln!(sink, "  cargo rail unify --check --show-diff");
    outln!(sink, "  cargo rail unify --check --explain");
  } else {
    outln!(sink, "  cargo rail unify --check --explain");
  }
}

fn decision_reasons_to_json(reasons: &[crate::cargo::UnifyDecisionReason]) -> Vec<serde_json::Value> {
  reasons
    .iter()
    .map(|reason| {
      serde_json::json!({
        "code": reason.code.as_str(),
        "summary": &*reason.summary,
        "features": reason.features.iter().map(|value| &**value).collect::<Vec<_>>(),
        "members": reason.members.iter().map(|value| &**value).collect::<Vec<_>>(),
        "borrowed_from": reason.borrowed_from.iter().map(|value| &**value).collect::<Vec<_>>(),
        "feature_paths": reason.feature_paths.iter().map(|path| serde_json::json!({
          "member": &*path.member,
          "alias": &*path.alias,
          "dependency_kind": path.dependency_kind.as_str(),
          "target": path.target.as_deref(),
          "features": path.features.iter().map(|feature| &**feature).collect::<Vec<_>>(),
          "default_features": path.default_features,
          "optional": path.optional,
        })).collect::<Vec<_>>(),
      })
    })
    .collect()
}

fn dependency_decisions_to_json(plan: &crate::cargo::UnificationPlan) -> Vec<serde_json::Value> {
  plan
    .dependency_decisions
    .iter()
    .map(|decision| {
      serde_json::json!({
        "dep_name": &*decision.dep_name,
        "subject": decision.subject.as_str(),
        "member": decision.member.as_deref(),
        "target": decision.target.as_deref(),
        "reasons": decision_reasons_to_json(&decision.reasons),
      })
    })
    .collect()
}

fn feature_reachability_to_json(plan: &crate::cargo::UnificationPlan) -> Vec<serde_json::Value> {
  plan
    .reachable_features
    .iter()
    .map(|feature| {
      serde_json::json!({
        "member": &*feature.crate_name,
        "feature": &*feature.feature_name,
        "root_kind": feature.root_kind,
        "path": feature.path.iter().map(|item| &**item).collect::<Vec<_>>(),
      })
    })
    .collect()
}

fn evidence_cache_to_json(plan: &crate::cargo::UnificationPlan) -> Vec<serde_json::Value> {
  plan
    .unused_deps
    .iter()
    .map(|unused| {
      serde_json::json!({
        "member": &*unused.member,
        "dependency": &*unused.dep_name,
        "hits": unused.proof.cache_hits,
        "misses": unused.proof.cache_misses,
        "miss_reasons": unused.proof.cache_miss_reasons.iter().map(|value| &**value).collect::<Vec<_>>(),
      })
    })
    .collect()
}

fn proof_certificates_to_json(plan: &crate::cargo::UnificationPlan, msrv_write_needed: bool) -> Vec<serde_json::Value> {
  let mut certificates =
    Vec::with_capacity(plan.unused_deps.len() + plan.pruned_features.len() + plan.undeclared_features.len());
  certificates.extend(plan.unused_deps.iter().map(|unused| {
    serde_json::json!({
      "schema_version": unused.proof.schema_version,
      "member": &*unused.member,
      "subject": {
        "kind": "dependency",
        "declaration": &*unused.dep_name,
        "package_ids": unused.proof.package_ids.iter().map(|value| &**value).collect::<Vec<_>>(),
        "crate_names": unused.proof.crate_names.iter().map(|value| &**value).collect::<Vec<_>>(),
        "dependency_kind": unused.kind.as_str(),
        "target": unused.target.as_deref(),
      },
      "decision": "remove",
      "evidence_source": unused.proof.evidence_source,
      "applicable_configurations": unused.proof.applicable_configurations,
      "complete_configurations": unused.proof.complete_configurations,
      "used_observations": unused.proof.used_observations,
      "unused_observations": unused.proof.unused_observations,
      "incomplete_observations": unused.proof.incomplete_observations,
      "uncertainties": unused.proof.uncertainties.iter().map(|value| &**value).collect::<Vec<_>>(),
    })
  }));
  certificates.extend(plan.pruned_features.iter().map(|feature| {
    serde_json::json!({
      "schema_version": 1,
      "member": &*feature.crate_name,
      "subject": { "kind": "feature", "declaration": &*feature.feature_name },
      "decision": "remove",
      "evidence_source": "feature_reachability",
      "roots_checked": [
        "published_api", "default", "resolved", "workspace_consumer",
        "source_cfg", "cargo_target_required", "preservation_policy"
      ],
      "declared_edges": feature.declared_edges.iter().map(|edge| &**edge).collect::<Vec<_>>(),
      "reachable_path": [],
      "package_visibility": "publish_false",
      "consumer_scope": "workspace",
      "uncertainties": [],
    })
  }));
  certificates.extend(plan.undeclared_features.iter().map(|feature| {
    serde_json::json!({
      "schema_version": 1,
      "member": &*feature.member,
      "subject": {
        "kind": "dependency_features",
        "declaration": &*feature.dep_name,
        "dependency_kind": feature.dep_kind.as_str(),
        "target": feature.target.as_deref(),
      },
      "decision": "add_features",
      "features": feature.undeclared_features.iter().map(|value| &**value).collect::<Vec<_>>(),
      "evidence_source": "standalone_compiler_causality",
      "borrowed_from": feature.borrowed_from.iter().map(|value| &**value).collect::<Vec<_>>(),
      "required_by": feature.required_by.iter().map(|value| &**value).collect::<Vec<_>>(),
      "uncertainties": [],
    })
  }));
  certificates.extend(plan.workspace_deps.iter().map(|dependency| {
    serde_json::json!({
      "schema_version": 1,
      "member": serde_json::Value::Null,
      "subject": { "kind": "workspace_dependency", "declaration": &*dependency.name },
      "decision": "unify",
      "evidence_source": "cargo_resolve_and_manifest_intersection",
      "version_requirement": dependency.version_req.to_string(),
      "features": dependency.features.iter().map(|feature| &**feature).collect::<Vec<_>>(),
      "default_features": dependency.default_features,
      "used_by": dependency.used_by.iter().map(|member| &**member).collect::<Vec<_>>(),
      "target": dependency.target.as_deref(),
      "path_dependency": dependency.path.is_some(),
      "uncertainties": [],
    })
  }));
  for (member, edits) in &plan.member_edits {
    certificates.extend(edits.iter().filter_map(|edit| match edit {
      crate::cargo::MemberEdit::UseWorkspace {
        dep_name,
        dep_kind,
        target,
        local_features,
        is_optional,
      } => Some(serde_json::json!({
        "schema_version": 1,
        "member": &**member,
        "subject": {
          "kind": "dependency_declaration",
          "declaration": &**dep_name,
          "dependency_kind": dep_kind.as_str(),
          "target": target.as_deref(),
        },
        "decision": "use_workspace",
        "evidence_source": "cargo_resolve_and_manifest_identity",
        "local_features": local_features.iter().map(|feature| &**feature).collect::<Vec<_>>(),
        "optional": is_optional,
        "uncertainties": [],
      })),
      crate::cargo::MemberEdit::EnforceMsrvInheritance => Some(serde_json::json!({
        "schema_version": 1,
        "member": &**member,
        "subject": { "kind": "package_msrv", "declaration": "package.rust-version" },
        "decision": "inherit_workspace",
        "evidence_source": "computed_workspace_msrv",
        "uncertainties": [],
      })),
      crate::cargo::MemberEdit::RemoveDep { .. }
      | crate::cargo::MemberEdit::RemoveFeature { .. }
      | crate::cargo::MemberEdit::AddFeatures { .. } => None,
    }));
  }
  certificates.extend(plan.transitive_pins.iter().map(|dependency| {
    serde_json::json!({
      "schema_version": 1,
      "member": serde_json::Value::Null,
      "subject": { "kind": "transitive_dependency", "declaration": &*dependency.name },
      "decision": "pin",
      "evidence_source": "resolved_fragmented_feature_sets",
      "version": dependency.version.to_string(),
      "features": dependency.features.iter().map(|feature| &**feature).collect::<Vec<_>>(),
      "uncertainties": [],
    })
  }));
  if msrv_write_needed && let Some(msrv) = &plan.computed_msrv {
    certificates.push(serde_json::json!({
      "schema_version": 1,
      "member": serde_json::Value::Null,
      "subject": { "kind": "workspace_msrv", "declaration": "workspace.package.rust-version" },
      "decision": "write",
      "evidence_source": "resolved_dependency_msrv",
      "version": msrv.version.to_string(),
      "contributors": msrv.contributors,
      "uncertainties": [],
    }));
  }
  certificates.sort_by(|left, right| {
    left["member"]
      .as_str()
      .cmp(&right["member"].as_str())
      .then_with(|| left["subject"]["kind"].as_str().cmp(&right["subject"]["kind"].as_str()))
      .then_with(|| {
        left["subject"]["declaration"]
          .as_str()
          .cmp(&right["subject"]["declaration"].as_str())
      })
      .then_with(|| left.to_string().cmp(&right.to_string()))
  });
  certificates
}

fn proof_set_fingerprint(certificates: &[serde_json::Value]) -> String {
  let encoded = serde_json::Value::Array(certificates.to_vec()).to_string();
  sha256_fingerprint(encoded.as_bytes())
}

fn sha256_fingerprint(bytes: &[u8]) -> String {
  crate::instrumentation::record_hash(bytes.len());
  let digest = Sha256::digest(bytes);
  let mut fingerprint = String::with_capacity("sha256:".len() + digest.len() * 2);
  fingerprint.push_str("sha256:");
  for byte in digest {
    let _ = write!(fingerprint, "{byte:02x}");
  }
  fingerprint
}

fn root_manifest_value(ctx: &WorkspaceContext) -> RailResult<serde_json::Value> {
  let mut path = ctx.workspace_prefix().unwrap_or_default();
  path.push("Cargo.toml");
  let manifest = ctx
    .snapshot()?
    .manifests()
    .iter()
    .find(|manifest| manifest.path().as_path() == path)
    .ok_or_else(|| RailError::message("workspace root Cargo.toml is missing from the authoritative snapshot"))?;
  let text = std::str::from_utf8(manifest.bytes())
    .map_err(|_| RailError::message("workspace root Cargo.toml is not valid UTF-8"))?;
  toml_edit::de::from_str(text)
    .map_err(|error| RailError::message(format!("failed to parse workspace resolver: {error}")))
}

fn resolver_diagnostic(root: &serde_json::Value) -> (String, String) {
  let explicit = root
    .get("workspace")
    .and_then(|workspace| workspace.get("resolver"))
    .or_else(|| root.get("package").and_then(|package| package.get("resolver")));
  if let Some(resolver) =
    explicit.and_then(|value| value.as_str().map(str::to_string).or_else(|| Some(value.to_string())))
  {
    return (resolver.trim_matches('"').to_string(), "manifest".to_string());
  }
  let Some(package) = root.get("package") else {
    return ("1".to_string(), "virtual-workspace default".to_string());
  };
  let edition = package
    .get("edition")
    .and_then(serde_json::Value::as_str)
    .unwrap_or("2015");
  let resolver = match edition {
    "2024" => "3",
    "2021" => "2",
    _ => "1",
  };
  (resolver.to_string(), format!("inferred from edition {edition}"))
}

fn cargo_source_overrides(root: &serde_json::Value, cargo_config: &serde_json::Value) -> Vec<String> {
  let mut overrides = Vec::new();
  if let Some(patches) = root.get("patch").and_then(serde_json::Value::as_object) {
    for (source, entries) in patches {
      let source = if crate::cargo::resolution::credential_bearing_url(source) {
        "<credential-bearing-url>"
      } else {
        source
      };
      if let Some(entries) = entries.as_object() {
        overrides.extend(entries.keys().map(|name| format!("patch.{source}.{name}")));
      }
    }
  }
  if let Some(replacements) = root.get("replace").and_then(serde_json::Value::as_object) {
    overrides.extend(replacements.keys().map(|name| format!("replace.{name}")));
  }
  if let Some(sources) = cargo_config.get("source").and_then(serde_json::Value::as_object) {
    for (name, source) in sources {
      if source.as_object().is_some_and(|source| {
        ["replace-with", "registry", "local-registry", "directory"]
          .iter()
          .any(|key| source.contains_key(*key))
      }) {
        overrides.push(format!("source.{name}"));
      }
    }
  }
  overrides.sort();
  overrides.dedup();
  overrides
}

fn unify_policy_overrides(ctx: &WorkspaceContext) -> RailResult<Vec<String>> {
  let actual = serde_json::to_value(ctx.config().map(|config| &config.unify).cloned().unwrap_or_default())?;
  let defaults = serde_json::to_value(crate::config::UnifyConfig::default())?;
  let Some(actual) = actual.as_object() else {
    return Err(RailError::message("effective unify configuration is not an object"));
  };
  let Some(defaults) = defaults.as_object() else {
    return Err(RailError::message("default unify configuration is not an object"));
  };
  Ok(
    actual
      .iter()
      .filter(|(key, value)| defaults.get(*key) != Some(*value))
      .map(|(key, _)| key.clone())
      .collect(),
  )
}

fn cargo_release_and_channel(verbose_version: &str) -> (String, &'static str) {
  let release = verbose_version
    .lines()
    .find_map(|line| line.strip_prefix("release: "))
    .or_else(|| {
      verbose_version
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    })
    .unwrap_or("unknown")
    .to_string();
  let channel = if release.contains("nightly") {
    "nightly"
  } else if release.contains("beta") {
    "beta"
  } else if release == "unknown" {
    "unknown"
  } else {
    "stable"
  };
  (release, channel)
}

/// Inspect the exact Cargo resolution domains used by dependency unification.
pub fn run_unify_doctor(ctx: &WorkspaceContext, format: UnifyOutputFormat) -> RailResult<()> {
  let snapshot = ctx.snapshot()?;
  let root = root_manifest_value(ctx)?;
  let (resolver, resolver_source) = resolver_diagnostic(&root);
  let (cargo_version, cargo_channel) = cargo_release_and_channel(snapshot.toolchain().cargo_verbose_version());
  let cargo_overrides = cargo_source_overrides(&root, snapshot.cargo_config().effective_file_settings());
  let policy_overrides = unify_policy_overrides(ctx)?;
  let ambiguous_aliases = ctx.graph().ambiguous_aliases();

  // MultiTargetMetadata is deliberately backed by the same exact ResolutionView
  // cache as planning. Doctor observes that model instead of reconstructing it.
  let metadata = ctx.multi_target_metadata()?;
  let host = snapshot.toolchain().host_target();
  let metadata_targets = metadata.targets();
  let mut target_domains = Vec::with_capacity(metadata_targets.len());
  for target in metadata_targets {
    let resolved_nodes = metadata
      .get(target)
      .and_then(|metadata| metadata.resolve.as_ref())
      .map_or(0, |resolve| resolve.nodes.len());
    target_domains.push(serde_json::json!({
      "target": target,
      "role": if target == "default" { "unfiltered" } else if target == host { "host" } else { "target" },
      "feature_mode": "default",
      "resolved_node_count": resolved_nodes,
    }));
  }

  let aliases = ambiguous_aliases
    .iter()
    .map(|alias| {
      serde_json::json!({
        "member": alias.member_name,
        "member_package_id": alias.member_id.to_string(),
        "alias": alias.alias,
        "candidates": alias.candidates.iter().map(|candidate| serde_json::json!({
          "package": candidate.package_name,
          "package_id": candidate.package_id.to_string(),
          "domains": candidate.domains,
        })).collect::<Vec<_>>(),
      })
    })
    .collect::<Vec<_>>();
  let (recommendation_code, recommendation) = if !aliases.is_empty() {
    (
      "disambiguate_aliases",
      "make each dependency alias resolve to one PackageId per workspace member and selected target",
    )
  } else if resolver == "1" {
    (
      "upgrade_resolver",
      "set workspace.resolver to the resolver required by the workspace edition before unifying features",
    )
  } else if !cargo_overrides.is_empty() {
    (
      "review_source_overrides",
      "review active Cargo source overrides, then run cargo rail unify --check --explain",
    )
  } else {
    ("check", "run cargo rail unify --check --explain")
  };

  if format.is_json() {
    crate::output::set_json_mode(true);
    let value = crate::output::machine_json_envelope(
      "unify",
      "doctor",
      "success",
      0,
      serde_json::json!({
        "cargo": { "version": cargo_version, "channel": cargo_channel },
        "resolver": { "version": resolver, "source": resolver_source },
        "feature_mode": { "packages": "workspace", "features": "default" },
        "effective_overrides": {
          "cargo_sources": cargo_overrides,
          "unify_policy": policy_overrides,
        },
        "target_domains": target_domains,
        "ambiguous_aliases": aliases,
        "recommended_action": {
          "code": recommendation_code,
          "message": recommendation,
        },
      }),
    );
    let rendered = serde_json::to_string_pretty(&value)?;
    return write_output(&rendered, None);
  }

  let mut rendered = String::with_capacity(512);
  writeln!(rendered, "unify doctor\n").ok();
  writeln!(rendered, "cargo: {cargo_version} ({cargo_channel})").ok();
  writeln!(rendered, "resolver: {resolver} ({resolver_source})").ok();
  writeln!(rendered, "features: workspace/default").ok();
  writeln!(rendered, "target domains: {}", target_domains.len()).ok();
  for target in &target_domains {
    writeln!(
      rendered,
      "  {} ({}, {} resolved nodes)",
      target["target"].as_str().unwrap_or("unknown"),
      target["role"].as_str().unwrap_or("target"),
      target["resolved_node_count"].as_u64().unwrap_or(0),
    )
    .ok();
  }
  writeln!(
    rendered,
    "cargo source overrides: {}",
    format_preview_list(&cargo_overrides, 8)
  )
  .ok();
  writeln!(
    rendered,
    "unify policy overrides: {}",
    format_preview_list(&policy_overrides, 8)
  )
  .ok();
  writeln!(rendered, "ambiguous aliases: {}", aliases.len()).ok();
  for alias in &aliases {
    writeln!(
      rendered,
      "  {}:{} -> {} candidates",
      alias["member"].as_str().unwrap_or("unknown"),
      alias["alias"].as_str().unwrap_or("unknown"),
      alias["candidates"].as_array().map_or(0, Vec::len),
    )
    .ok();
  }
  writeln!(rendered, "recommended: {recommendation}").ok();
  write_output(&rendered, None)
}

/// Analyze workspace dependencies (check mode)
pub fn run_unify_analyze(
  ctx: &WorkspaceContext,
  show_diff: bool,
  explain: bool,
  format: UnifyOutputFormat,
  output: Option<&PathBuf>,
) -> RailResult<()> {
  ctx.snapshot()?;
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  // Create analyzer (config comes from rail.toml via ctx)
  let analyzer = UnifyAnalyzer::new(ctx)?;

  // Run analysis
  let plan = analyzer.analyze()?;
  let msrv_write_needed = if let Some(msrv) = plan.computed_msrv.as_ref() {
    workspace_msrv_write_needed(ctx.workspace_root(), &msrv.version)?
  } else {
    false
  };

  let has_changes = plan.has_planned_changes(msrv_write_needed);

  // JSON output mode (but still honor exit codes)
  if json {
    let proof_certificates = proof_certificates_to_json(&plan, msrv_write_needed);
    let proof_fingerprint = proof_set_fingerprint(&proof_certificates);
    let (actions, risks, trace) = build_unify_mutation_parts(&plan, msrv_write_needed, false, true, output);
    let mutation_plan = if ctx.has_git() {
      Some(mutation::build_plan(
        ctx,
        "unify",
        actions.clone(),
        risks.clone(),
        trace.clone(),
      )?)
    } else {
      None
    };

    let mut canonical_actions = actions;
    canonical_actions.sort_by(|a, b| {
      a.code
        .cmp(&b.code)
        .then_with(|| a.target.cmp(&b.target))
        .then_with(|| a.detail.cmp(&b.detail))
    });
    let mut reason_codes: Vec<String> = mutation_plan
      .as_ref()
      .map(|plan| {
        plan
          .trace
          .iter()
          .map(|t| t.code.clone())
          .chain(plan.risks.iter().map(|r| r.code.clone()))
          .collect()
      })
      .unwrap_or_else(|| {
        trace
          .iter()
          .map(|t| t.code.clone())
          .chain(risks.iter().map(|r| r.code.clone()))
          .collect()
      });
    reason_codes.sort();
    reason_codes.dedup();

    let payload = serde_json::json!({
      "command": "unify",
      "check": true,
      "msrv_write_needed": msrv_write_needed,
      "has_changes": has_changes,
      "workspace_deps": plan.workspace_deps.iter().map(|d| {
        let features: Vec<&str> = d.features.iter().map(|f| &**f).collect();
        serde_json::json!({
          "name": &*d.name,
          "version": d.version_req,
          "features": features,
        })
      }).collect::<Vec<_>>(),
      "summary": {
        "workspace_deps_count": plan.workspace_deps.len(),
        "member_edits_count": plan.member_edit_count(),
        "members_affected": plan.member_edits.len(),
        "transitive_pins_count": plan.transitive_pins.len(),
        "duplicates_unified": plan.duplicates_cleaned.len(),
        "dead_features_pruned": plan.pruned_features.len(),
        "optional_features_detected": plan.optional_features.len(),
        "version_mismatches": plan.version_mismatches.len(),
        "unused_deps": plan.unused_deps.len(),
      },
      "has_blocking_issues": plan.has_blocking_issues(),
      "issues": plan.issues.iter().map(|i| serde_json::json!({
        "kind": format!("{:?}", i.kind),
        "dep_name": &*i.dep_name,
        "severity": format!("{:?}", i.severity),
        "message": &*i.message,
      })).collect::<Vec<_>>(),
      "dependency_decisions": dependency_decisions_to_json(&plan),
      "feature_reachability": feature_reachability_to_json(&plan),
      "evidence_cache": evidence_cache_to_json(&plan),
      "proof_fingerprint": proof_fingerprint,
      "proof_certificates": proof_certificates,
      "action_plan": canonical_actions,
      "reason_codes": reason_codes,
      "mutation_plan_available": mutation_plan.is_some(),
      "mutation_plan": mutation_plan,
    });

    let exit_code = if plan.has_blocking_issues() {
      2
    } else if has_changes {
      1
    } else {
      0
    };
    let result = if plan.has_blocking_issues() {
      "failed"
    } else if has_changes {
      "pending_changes"
    } else {
      "success"
    };
    let output_json = crate::output::machine_json_envelope("unify", "check", result, exit_code, payload);
    let rendered =
      serde_json::to_string_pretty(&output_json).map_err(|e| RailError::message(format!("JSON error: {}", e)))?;
    write_output(&rendered, output)?;

    if plan.has_blocking_issues() {
      return Err(RailError::message("blocking issues prevent unification"));
    }
    if has_changes {
      return Err(RailError::CheckHasPendingChanges);
    }
    return Ok(());
  }

  let mut sink = UnifyTextSink::new(output.is_some());

  // Default output stays terse; detailed reasoning belongs behind --explain or JSON.
  write_compact_summary(&mut sink, &plan, ctx.workspace_root(), msrv_write_needed, has_changes);

  // Show explain output if requested
  if explain {
    display_explain(&mut sink, &plan);
  }

  // Show diff if requested
  if show_diff && has_changes {
    outln!(sink);
    outln!(sink, "planned changes:");
    outln!(sink);

    // Show MSRV update first (workspace manifest)
    if let Some(msrv) = plan.computed_msrv.as_ref()
      && msrv_write_needed
    {
      outln!(sink, "[workspace.package]:");
      outln!(
        sink,
        "  rust-version = \"{}.{}.{}\"",
        msrv.version.major,
        msrv.version.minor,
        msrv.version.patch
      );
      outln!(sink);
    }

    // Show workspace deps that will be added
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() {
      outln!(sink, "[workspace.dependencies]:");
      for dep in &plan.workspace_deps {
        outln!(sink, "  + {} = \"{}\"", dep.name, dep.version_req);
        if !dep.features.is_empty() {
          let mut features = dep.features.clone();
          features.sort();
          outln!(sink, "      features = {}", format_quoted_list(&features));
        }
      }

      // Show transitive pins (also written to workspace.dependencies)
      for pin in &plan.transitive_pins {
        outln!(sink, "  + {} = \"{}\"  # transitive pin", pin.name, pin.version);
        if !pin.features.is_empty() {
          let mut features = pin.features.clone();
          features.sort();
          outln!(sink, "      features = {}", format_quoted_list(&features));
        }
      }
      outln!(sink);
    }

    // Show member edits
    let mut members: Vec<_> = plan.member_edits.keys().collect();
    members.sort();
    for member in members {
      let edits = &plan.member_edits[member];
      if edits.is_empty() {
        continue;
      }
      let path = plan
        .member_paths
        .get(member)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| member.to_string());
      outln!(sink, "{}:", path);
      for edit in edits {
        match edit {
          crate::cargo::MemberEdit::UseWorkspace {
            dep_name,
            dep_kind,
            target,
            local_features,
            is_optional,
          } => {
            let section = dependency_section(dep_kind, target.as_deref());
            let mut line = String::with_capacity(64 + section.len() + dep_name.len());
            let _ = write!(line, "  {} {} -> workspace = true", section, dep_name);
            if !local_features.is_empty() {
              let mut features = local_features.clone();
              features.sort();
              let _ = write!(line, ", features = {}", format_quoted_list(&features));
            }
            if *is_optional {
              line.push_str(", optional = true");
            }
            outln!(sink, "{}", line);
          }
          crate::cargo::MemberEdit::RemoveDep {
            dep_name,
            dep_kind,
            target,
          } => {
            let section = dependency_section(dep_kind, target.as_deref());
            outln!(sink, "  {} {} -> REMOVE (unused)", section, dep_name);
          }
          crate::cargo::MemberEdit::RemoveFeature { feature_name } => {
            outln!(sink, "  [features] {} -> REMOVE (dead/empty)", feature_name);
          }
          crate::cargo::MemberEdit::AddFeatures {
            dep_name,
            dep_kind,
            target,
            features_to_add,
          } => {
            let section = dependency_section(dep_kind, target.as_deref());
            let mut sorted_features = features_to_add.clone();
            sorted_features.sort();
            outln!(
              sink,
              "  {} {} -> ADD features {}",
              section,
              dep_name,
              format_quoted_list(&sorted_features)
            );
          }
          crate::cargo::MemberEdit::EnforceMsrvInheritance => {
            outln!(sink, "  [package] rust-version = {{ workspace = true }}");
          }
        }
      }
      outln!(sink);
    }
  }

  // Show validation results if any failed
  let failed_validations: Vec<_> = plan.validation_results.iter().filter(|v| !v.success).collect();

  if !failed_validations.is_empty() {
    eprintln!();
    crate::error!("validation errors:");
    for val in failed_validations {
      eprintln!("  {}: {}", val.target, val.error.as_deref().unwrap_or("unknown"));
    }
  }

  // Final message and exit code
  if plan.has_blocking_issues() {
    eprintln!();
    crate::error!("blocking issues prevent unification");
    if let Some(content) = sink.finish() {
      write_output(&content, output)?;
    }
    return Err(RailError::message("blocking issues prevent unification"));
  } else if has_changes {
    if let Some(content) = sink.finish() {
      write_output(&content, output)?;
    }
    return Err(RailError::CheckHasPendingChanges);
  } else {
    outln!(sink);
    outln!(sink, "status: no changes");
  }

  if let Some(content) = sink.finish() {
    write_output(&content, output)?;
  }

  Ok(())
}

/// Execute dependency unification
struct ManifestTransaction {
  originals: Vec<(std::path::PathBuf, Vec<u8>)>,
}

#[derive(Debug, Default)]
struct ResolvedGraphSnapshot {
  facts: std::collections::BTreeMap<String, BTreeSet<String>>,
  adjacency: std::collections::BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug)]
struct VerifiedGraphDelta {
  added: usize,
  removed: usize,
  fingerprint: String,
  facts: Vec<String>,
}

fn resolved_graph_snapshot(metadata: &cargo_metadata::Metadata) -> RailResult<ResolvedGraphSnapshot> {
  let workspace_ids: std::collections::HashSet<_> = metadata.workspace_members.iter().collect();
  let workspace_root = metadata.workspace_root.as_std_path();
  let mut package_by_id = std::collections::HashMap::with_capacity(metadata.packages.len());
  let mut snapshot = ResolvedGraphSnapshot::default();
  for package in &metadata.packages {
    let identity = if workspace_ids.contains(&package.id) {
      format!("workspace:{}@{}", package.name, package.version)
    } else if let Some(source) = &package.source {
      format!("{}#{}@{}", source, package.name, package.version)
    } else {
      let package_dir = package
        .manifest_path
        .parent()
        .map(|path| path.as_std_path())
        .unwrap_or_else(|| package.manifest_path.as_std_path());
      let portable_path = relative_path(workspace_root, package_dir)
        .map(|path| crate::utils::path_to_git_format(&path))
        .unwrap_or_else(|| crate::utils::file_fingerprint(package.manifest_path.as_std_path()));
      format!("path:{portable_path}#{}@{}", package.name, package.version)
    };
    package_by_id.insert(package.id.to_string(), (identity.clone(), package.name.to_string()));
    snapshot.facts.insert(
      format!("package|{identity}"),
      BTreeSet::from([package.name.to_string()]),
    );
  }

  let resolve = metadata
    .resolve
    .as_ref()
    .ok_or_else(|| RailError::message("Cargo returned no resolve graph".to_string()))?;
  for node in &resolve.nodes {
    let Some((from_identity, from_name)) = package_by_id.get(&node.id.to_string()) else {
      continue;
    };
    for feature in &node.features {
      snapshot.facts.insert(
        format!("feature|{from_identity}|{feature}"),
        BTreeSet::from([from_name.clone()]),
      );
    }
    for dependency in &node.deps {
      let Some((to_identity, to_name)) = package_by_id.get(&dependency.pkg.to_string()) else {
        continue;
      };
      let mut domains: Vec<_> = dependency
        .dep_kinds
        .iter()
        .map(|domain| {
          format!(
            "{:?}:{}",
            domain.kind,
            domain.target.as_ref().map(ToString::to_string).unwrap_or_default()
          )
        })
        .collect();
      domains.sort();
      snapshot.facts.insert(
        format!(
          "edge|{from_identity}|{}|{to_identity}|{}",
          dependency.name,
          domains.join(",")
        ),
        BTreeSet::from([from_name.clone(), to_name.clone(), dependency.name.clone()]),
      );
      snapshot
        .adjacency
        .entry(from_name.clone())
        .or_default()
        .insert(to_name.clone());
    }
  }
  Ok(snapshot)
}

fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
  use std::path::Component;

  let base: Vec<_> = base.components().collect();
  let target: Vec<_> = target.components().collect();
  let common = base
    .iter()
    .zip(&target)
    .take_while(|(left, right)| left == right)
    .count();
  if common == 0 {
    return None;
  }
  let mut relative = PathBuf::new();
  for component in &base[common..] {
    if matches!(component, Component::Normal(_)) {
      relative.push("..");
    }
  }
  for component in &target[common..] {
    relative.push(component.as_os_str());
  }
  Some(relative)
}

fn authorized_graph_names(plan: &crate::cargo::UnificationPlan) -> BTreeSet<String> {
  let mut names = BTreeSet::new();
  names.extend(plan.workspace_deps.iter().map(|dependency| dependency.name.to_string()));
  names.extend(
    plan
      .transitive_pins
      .iter()
      .map(|dependency| dependency.name.to_string()),
  );
  for dependency in &plan.unused_deps {
    names.insert(dependency.member.to_string());
    names.insert(dependency.dep_name.to_string());
  }
  for feature in &plan.undeclared_features {
    names.insert(feature.member.to_string());
    names.insert(feature.dep_name.to_string());
  }
  for feature in &plan.pruned_features {
    names.insert(feature.crate_name.to_string());
    for edge in &feature.declared_edges {
      let dependency = edge
        .strip_prefix("dep:")
        .unwrap_or(edge)
        .split(['/', '?'])
        .next()
        .unwrap_or_default();
      if !dependency.is_empty() {
        names.insert(dependency.to_string());
      }
    }
  }
  names
}

fn graph_name_closure(
  roots: &BTreeSet<String>,
  before: &ResolvedGraphSnapshot,
  after: &ResolvedGraphSnapshot,
) -> BTreeSet<String> {
  let mut closure = roots.clone();
  let mut pending: Vec<_> = roots.iter().cloned().collect();
  while let Some(name) = pending.pop() {
    for next in before
      .adjacency
      .get(&name)
      .into_iter()
      .chain(after.adjacency.get(&name))
      .flatten()
    {
      if closure.insert(next.clone()) {
        pending.push(next.clone());
      }
    }
  }
  closure
}

impl ManifestTransaction {
  fn capture(
    ctx: &WorkspaceContext,
    plan: &crate::cargo::UnificationPlan,
    msrv_write_needed: bool,
  ) -> RailResult<Self> {
    let mut paths = std::collections::BTreeSet::new();
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() || msrv_write_needed {
      paths.insert(ctx.workspace_root().join("Cargo.toml"));
    }
    for member in plan.member_edits.keys() {
      if let Some(path) = plan.member_paths.get(member) {
        paths.insert(path.clone());
      }
    }
    if !plan.transitive_pins.is_empty() {
      paths.insert(transitive_pins_host_manifest_path(ctx)?);
    }
    let mut originals = Vec::with_capacity(paths.len());
    for path in paths {
      originals.push((
        path.clone(),
        std::fs::read(&path)
          .map_err(|error| RailError::message(format!("capturing {} before unify apply: {error}", path.display())))?,
      ));
    }
    Ok(Self { originals })
  }

  fn restore(&self) -> RailResult<()> {
    for (path, content) in &self.originals {
      std::fs::write(path, content).map_err(|error| {
        RailError::message(format!(
          "restoring {} after failed verification: {error}",
          path.display()
        ))
      })?;
    }
    Ok(())
  }
}

fn verify_applied_unify_graph(
  ctx: &WorkspaceContext,
  plan: &crate::cargo::UnificationPlan,
) -> RailResult<VerifiedGraphDelta> {
  let snapshot = ctx.snapshot()?;
  let target_metadata = ctx.multi_target_metadata()?;
  let targets = target_metadata.targets();
  let mut verified_feature_edits = std::collections::BTreeSet::new();
  let authorized_names = authorized_graph_names(plan);
  let mut delta_lines = BTreeSet::new();
  let mut added = 0usize;
  let mut removed = 0usize;
  for target in targets {
    let before_metadata = target_metadata
      .metadata_for_target(target)
      .ok_or_else(|| RailError::message(format!("missing pre-edit metadata for target `{target}`")))?;
    let before = resolved_graph_snapshot(before_metadata)?;
    let mut command = cargo_metadata::MetadataCommand::new();
    command
      .cargo_path(std::path::PathBuf::from(snapshot.toolchain().cargo_program()))
      .current_dir(snapshot.cargo_current_dir())
      .manifest_path(ctx.workspace_root().join("Cargo.toml"));
    if target != "default" {
      command.other_options(vec![String::from("--filter-platform"), target.to_string()]);
    }
    snapshot.validate_resolution_environment_unchanged()?;
    crate::instrumentation::record_cargo_metadata_load(target != "default");
    let metadata = command.exec().map_err(|error| {
      if snapshot.cargo_config().has_credential_capability() {
        RailError::with_help(
          format!(
            "resolving post-edit metadata for target '{target}' failed while credential capabilities were active"
          ),
          "run cargo metadata directly for provider diagnostics; cargo-rail suppresses credential-provider output",
        )
      } else {
        RailError::message(format!("resolving post-edit metadata for target `{target}`: {error}"))
      }
    })?;
    snapshot.validate_resolution_environment_unchanged()?;
    let after = resolved_graph_snapshot(&metadata)?;
    let closure = graph_name_closure(&authorized_names, &before, &after);
    for (fact, participants) in after.facts.iter().filter(|(fact, _)| !before.facts.contains_key(*fact)) {
      if participants.is_disjoint(&closure) {
        return Err(RailError::message(format!(
          "unplanned graph addition on `{target}` outside the authorized dependency closure: {fact}"
        )));
      }
      added += 1;
      delta_lines.insert(format!("{target}|+|{fact}"));
    }
    for (fact, participants) in before.facts.iter().filter(|(fact, _)| !after.facts.contains_key(*fact)) {
      if participants.is_disjoint(&closure) {
        return Err(RailError::message(format!(
          "unplanned graph removal on `{target}` outside the authorized dependency closure: {fact}"
        )));
      }
      removed += 1;
      delta_lines.insert(format!("{target}|-|{fact}"));
    }
    let resolve = metadata
      .resolve
      .as_ref()
      .ok_or_else(|| RailError::message(format!("Cargo returned no resolve graph for target `{target}`")))?;

    for unused in &plan.unused_deps {
      let Some(package) = metadata
        .packages
        .iter()
        .find(|package| package.name.as_ref() == &*unused.member)
      else {
        return Err(RailError::message(format!(
          "workspace member `{}` disappeared after removing `{}`",
          unused.member, unused.dep_name
        )));
      };
      let declaration_remains = package.dependencies.iter().any(|dependency| {
        let declaration_name = dependency.rename.as_deref().unwrap_or(dependency.name.as_ref());
        let kind_matches = matches!(
          (unused.kind, dependency.kind),
          (crate::cargo::DepKind::Normal, cargo_metadata::DependencyKind::Normal)
            | (crate::cargo::DepKind::Dev, cargo_metadata::DependencyKind::Development)
            | (crate::cargo::DepKind::Build, cargo_metadata::DependencyKind::Build)
        );
        declaration_name == &*unused.dep_name
          && kind_matches
          && dependency.target.as_ref().map(ToString::to_string).as_deref() == unused.target.as_deref()
      });
      if declaration_remains {
        return Err(RailError::message(format!(
          "removed {} dependency `{}` still exists for `{}` in target scope {:?}",
          unused.kind.as_str(),
          unused.dep_name,
          unused.member,
          unused.target
        )));
      }
    }

    for undeclared in &plan.undeclared_features {
      let Some(package) = metadata
        .packages
        .iter()
        .find(|package| package.name.as_ref() == &*undeclared.member)
      else {
        continue;
      };
      let Some(member_node) = resolve.nodes.iter().find(|node| node.id == package.id) else {
        continue;
      };
      let crate_name = undeclared.dep_name.replace('-', "_");
      let Some(dependency) = member_node.deps.iter().find(|dependency| dependency.name == crate_name) else {
        continue;
      };
      let Some(dependency_node) = resolve.nodes.iter().find(|node| node.id == dependency.pkg) else {
        continue;
      };
      for feature in &undeclared.undeclared_features {
        if dependency_node
          .features
          .iter()
          .any(|resolved| resolved.as_str() == &**feature)
        {
          verified_feature_edits.insert((
            undeclared.member.to_string(),
            undeclared.dep_name.to_string(),
            feature.to_string(),
          ));
        }
      }
    }
  }

  for undeclared in &plan.undeclared_features {
    for feature in &undeclared.undeclared_features {
      let key = (
        undeclared.member.to_string(),
        undeclared.dep_name.to_string(),
        feature.to_string(),
      );
      if !verified_feature_edits.contains(&key) {
        return Err(RailError::message(format!(
          "feature `{}/{}` did not resolve for `{}` after the planned edit",
          undeclared.dep_name, feature, undeclared.member
        )));
      }
    }
  }
  let facts: Vec<_> = delta_lines.into_iter().collect();
  let mut encoded = String::new();
  for line in &facts {
    encoded.push_str(line);
    encoded.push('\n');
  }
  Ok(VerifiedGraphDelta {
    added,
    removed,
    fingerprint: sha256_fingerprint(encoded.as_bytes()),
    facts,
  })
}

/// Apply a verified dependency-unification plan transactionally.
pub fn run_unify_apply(
  ctx: &WorkspaceContext,
  backup: bool,
  no_report: bool,
  report_path: Option<std::path::PathBuf>,
  plan_path: Option<std::path::PathBuf>,
  format: UnifyOutputFormat,
) -> RailResult<()> {
  ctx.snapshot()?;
  use crate::backup::{BackupManager, BackupMetadata};
  use std::path::PathBuf;

  let json = format.is_json();
  if json {
    crate::output::set_json_mode(true);
  }

  // Create analyzer (config comes from rail.toml via ctx)
  let analyzer = UnifyAnalyzer::new(ctx)?;

  let plan = analyzer.analyze()?;
  let msrv_write_needed = if let Some(msrv) = plan.computed_msrv.as_ref() {
    workspace_msrv_write_needed(ctx.workspace_root(), &msrv.version)?
  } else {
    false
  };
  let proof_certificates = proof_certificates_to_json(&plan, msrv_write_needed);
  let proof_fingerprint = proof_set_fingerprint(&proof_certificates);

  // Check for blockers
  if plan.has_blocking_issues() {
    crate::error!("blocking issues prevent unification:");
    for issue in &plan.issues {
      if issue.severity == crate::cargo::IssueSeverity::Error {
        eprintln!("  {}: {}", issue.dep_name, issue.message);
      }
    }
    return Err(crate::error::RailError::message("blocking issues prevent unification"));
  }

  if !plan.has_planned_changes(msrv_write_needed) {
    if json {
      let output = crate::output::machine_json_envelope(
        "unify",
        "apply",
        "unchanged",
        0,
        serde_json::json!({
          "dependencies": 0,
          "members": 0,
          "portable": false,
        }),
      );
      println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
      println!("nothing to unify");
    }
    return Ok(());
  }

  let expected_mutation_plan =
    build_unify_mutation_plan(ctx, &plan, msrv_write_needed, backup, no_report, report_path.as_ref())?;
  let mutation_plan = if let Some(path) = plan_path.as_ref() {
    let from_file = mutation::read_plan_file(path)?;
    if from_file.contract_version != mutation::MUTATION_CONTRACT_VERSION {
      return Err(RailError::with_help(
        format!(
          "unsupported mutation plan contract version: {} (expected {})",
          from_file.contract_version,
          mutation::MUTATION_CONTRACT_VERSION
        ),
        "regenerate the plan using the current cargo-rail version".to_string(),
      ));
    }
    if !from_file.operation_id.starts_with("unify-") {
      return Err(RailError::with_help(
        format!("plan '{}' is not a unify plan", path.display()),
        "use a plan generated from 'cargo rail unify --check -f json'".to_string(),
      ));
    }
    mutation::validate_pre_apply_with_allowed_paths(ctx, &from_file, std::slice::from_ref(path))?;
    mutation::validate_requested_operation(&from_file, &expected_mutation_plan)?;
    from_file
  } else {
    mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
    expected_mutation_plan
  };
  let plan_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "unify",
    "plan",
    "planned",
    mutation_plan.clone(),
    vec![
      MutationTrace::new("UNIFY_PLAN_CREATED", "created deterministic unify mutation plan"),
      MutationTrace::new(
        "UNIFY_PROOF_SET_BOUND",
        format!("bound portable proof set {proof_fingerprint}"),
      ),
    ],
  )?;
  progress!("receipt: {}", plan_receipt.display());

  // Create backup if requested or first run
  let backup_manager = BackupManager::new(ctx.workspace_root());
  let is_first_run = !backup_manager.has_backups();
  let should_backup = backup || is_first_run;
  let mut created_backup_id: Option<String> = None;

  if should_backup {
    if is_first_run && !backup {
      progress!("creating backup (first run)...");
    } else {
      progress!("creating backup...");
    }

    // Estimate: root Cargo.toml + one per member with edits
    let mut files_to_backup = Vec::with_capacity(1 + plan.member_edits.len());
    if !plan.workspace_deps.is_empty() || !plan.transitive_pins.is_empty() || msrv_write_needed {
      files_to_backup.push(PathBuf::from("Cargo.toml"));
    }

    // Transitive pins may also modify the configured host's Cargo.toml.
    if !plan.transitive_pins.is_empty() {
      let host_path = transitive_pins_host_manifest_path(ctx)?;
      if let Ok(rel_path) = host_path.strip_prefix(ctx.workspace_root()) {
        // Avoid duplicating root Cargo.toml when host is root.
        if rel_path != std::path::Path::new("Cargo.toml") {
          files_to_backup.push(rel_path.to_path_buf());
        }
      }
    }

    for member_name in plan.member_edits.keys() {
      if let Some(manifest_path) = plan.member_paths.get(member_name)
        && let Ok(rel_path) = manifest_path.strip_prefix(ctx.workspace_root())
      {
        files_to_backup.push(rel_path.to_path_buf());
      }
    }

    let metadata = BackupMetadata::new("cargo rail unify");
    let max_backups = ctx.config().map(|c| c.unify.max_backups).unwrap_or(3);
    let backup_id = backup_manager.create_backup(&files_to_backup, metadata, max_backups)?;
    progress!("backup: {}", backup_id);
    created_backup_id = Some(backup_id);
  }

  let writer = ManifestWriter::new();
  let manifest_transaction = ManifestTransaction::capture(ctx, &plan, msrv_write_needed)?;

  if !plan.workspace_deps.is_empty() {
    progress!("writing [workspace.dependencies]...");
    writer.write_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.workspace_deps)?;
  }

  progress!("updating {} members...", plan.member_edits.len());
  for (member_name, edits) in &plan.member_edits {
    let member_path = plan
      .member_paths
      .get(member_name)
      .ok_or_else(|| crate::error::RailError::message(format!("member path not found: {}", member_name)))?;

    for edit in edits {
      match edit {
        crate::cargo::MemberEdit::UseWorkspace {
          dep_name,
          dep_kind,
          target,
          local_features,
          is_optional,
        } => {
          writer.update_member(
            member_path,
            dep_name,
            *dep_kind,
            target.as_deref(),
            if local_features.is_empty() {
              None
            } else {
              Some(local_features.as_slice())
            },
            *is_optional,
          )?;
        }
        crate::cargo::MemberEdit::RemoveDep {
          dep_name,
          dep_kind,
          target,
        } => {
          writer.remove_dep(member_path, dep_name, *dep_kind, target.as_deref())?;
        }
        crate::cargo::MemberEdit::RemoveFeature { feature_name } => {
          writer.remove_feature(member_path, feature_name)?;
        }
        crate::cargo::MemberEdit::AddFeatures {
          dep_name,
          dep_kind,
          target,
          features_to_add,
        } => {
          writer.add_features(member_path, dep_name, *dep_kind, target.as_deref(), features_to_add)?;
        }
        crate::cargo::MemberEdit::EnforceMsrvInheritance => {
          writer.enforce_member_msrv_inheritance(member_path)?;
        }
      }
    }
  }

  if !plan.transitive_pins.is_empty() {
    progress!("pinning {} transitives...", plan.transitive_pins.len());

    // Add transitive deps to [workspace.dependencies] first
    // This is required before we can reference them with `workspace = true`
    progress!("  adding to [workspace.dependencies]...");
    writer.write_transitive_workspace_deps(&ctx.workspace_root().join("Cargo.toml"), &plan.transitive_pins)?;

    // Add to host's [dev-dependencies] with workspace = true
    let host_path = transitive_pins_host_manifest_path(ctx)?;
    let host_dir = host_path.parent().unwrap_or(&host_path);
    let relative_path = host_dir.strip_prefix(ctx.workspace_root()).unwrap_or(host_dir);
    if relative_path != std::path::Path::new("") && host_path != ctx.workspace_root().join("Cargo.toml") {
      progress!("  host: {}", relative_path.display());
    }
    writer.add_transitive_pins(&host_path, &plan.transitive_pins)?;
  }

  let msrv_warning = plan.computed_msrv.as_ref().and_then(|msrv| msrv.warning.clone());
  if let Some(ref msrv) = plan.computed_msrv {
    // Show warning if workspace mode has compatibility issues
    if let Some(ref warning) = msrv.warning
      && !json
    {
      crate::warn!("{}", warning);
    }
    if msrv_write_needed {
      progress!(
        "writing rust-version = \"{}.{}.{}\"...",
        msrv.version.major,
        msrv.version.minor,
        msrv.version.patch
      );
      writer.write_workspace_msrv(&ctx.workspace_root().join("Cargo.toml"), &msrv.version)?;
    }
  }

  progress!("verifying planned Cargo graph...");
  let graph_delta = match verify_applied_unify_graph(ctx, &plan) {
    Ok(delta) => delta,
    Err(error) => {
      manifest_transaction.restore()?;
      return Err(RailError::with_help(
        format!("unify graph verification failed: {error}"),
        "all modified manifests were restored; regenerate the plan after resolving the reported graph mismatch"
          .to_string(),
      ));
    }
  };
  progress!(
    "  Authorized graph delta: {} addition(s), {} removal(s), {}",
    graph_delta.added,
    graph_delta.removed,
    graph_delta.fingerprint
  );

  let repaired_members: BTreeSet<_> = plan
    .member_edits
    .iter()
    .filter(|(_, edits)| {
      edits
        .iter()
        .any(|edit| matches!(edit, crate::cargo::MemberEdit::AddFeatures { .. }))
    })
    .map(|(member, _)| member.as_ref())
    .collect();
  if !repaired_members.is_empty() {
    progress!("verifying standalone feature repairs...");
    for member in repaired_members {
      if let Err(error) = crate::compiler::verify_standalone_member(ctx.workspace_root(), member) {
        manifest_transaction.restore()?;
        return Err(RailError::with_help(
          format!("unify standalone verification failed: {error}"),
          "all modified manifests were restored; the proposed feature repair did not make the member self-contained"
            .to_string(),
        ));
      }
    }
  }

  let mut written_report_path = None;
  if !no_report {
    let actual_report_path = report_path
      .unwrap_or_else(|| crate::workspace::cargo_rail_state_root(ctx.workspace_root()).join("unify-report.md"));
    UnifyReport::write_to_file(&plan, &actual_report_path)?;
    progress!("report: {}", actual_report_path.display());
    written_report_path = Some(actual_report_path);
  }

  // Count undeclared feature fixes
  let features_fixed: usize = plan
    .member_edits
    .values()
    .flat_map(|edits| edits.iter())
    .filter_map(|e| match e {
      crate::cargo::MemberEdit::AddFeatures { features_to_add, .. } => Some(features_to_add.len()),
      _ => None,
    })
    .sum();
  let crates_fixed = plan
    .member_edits
    .values()
    .filter(|edits| {
      edits
        .iter()
        .any(|e| matches!(e, crate::cargo::MemberEdit::AddFeatures { .. }))
    })
    .count();

  if !json {
    println!(
      "\nunified {} dependencies across {} members",
      plan.workspace_deps.len(),
      plan.member_edits.len()
    );
    if !plan.transitive_pins.is_empty() {
      println!("  {} transitives pinned", plan.transitive_pins.len());
    }
    if !plan.duplicates_cleaned.is_empty() {
      println!("  {} duplicates resolved", plan.duplicates_cleaned.len());
    }
    if !plan.pruned_features.is_empty() {
      println!("  {} unreachable private features pruned", plan.pruned_features.len());
    }
    if !plan.optional_features.is_empty() {
      println!(
        "  {} optional features detected (user-facing, preserved)",
        plan.optional_features.len()
      );
    }
    if features_fixed > 0 {
      println!(
        "  {} undeclared features fixed across {} crates",
        features_fixed, crates_fixed
      );
    }
    if let Some(ref msrv) = plan.computed_msrv {
      use crate::cargo::MsrvSourceUsed;
      let source_desc = match msrv.source_used {
        MsrvSourceUsed::Deps => format!(
          "from deps: {}",
          msrv.contributors.first().unwrap_or(&"unknown".to_string())
        ),
        MsrvSourceUsed::Workspace => "preserved from workspace".to_string(),
        MsrvSourceUsed::MaxWorkspace => "from workspace (higher than deps)".to_string(),
        MsrvSourceUsed::MaxDeps => format!(
          "from deps: {}",
          msrv.contributors.first().unwrap_or(&"unknown".to_string())
        ),
      };
      println!(
        "  rust-version = {}.{}.{} ({})",
        msrv.version.major, msrv.version.minor, msrv.version.patch, source_desc
      );
    }

    println!("\nnext: cargo check && cargo test");

    if let Some(ref backup_id) = created_backup_id {
      println!("undo: cargo rail unify undo  (backup: {})", backup_id);
    }
  }

  let apply_receipt = mutation::write_receipt(
    ctx.workspace_root(),
    "unify",
    "apply",
    "applied",
    mutation_plan,
    vec![
      MutationTrace::new("UNIFY_APPLY_STARTED", "started applying unify plan"),
      MutationTrace::new(
        "UNIFY_GRAPH_DELTA_VERIFIED",
        format!(
          "authorized graph delta: {} addition(s), {} removal(s), {}",
          graph_delta.added, graph_delta.removed, graph_delta.fingerprint
        ),
      ),
      MutationTrace::new(
        "UNIFY_PROOF_SET_BOUND",
        format!("applied portable proof set {proof_fingerprint}"),
      ),
      MutationTrace::new("UNIFY_APPLY_COMPLETED", "completed unify apply"),
    ],
  )?;
  progress!("receipt: {}", apply_receipt.display());

  if json {
    let warnings = msrv_warning.into_iter().collect::<Vec<_>>();
    let output = crate::output::machine_json_envelope(
      "unify",
      "apply",
      "applied",
      0,
      serde_json::json!({
        "dependencies": plan.workspace_deps.len(),
        "members": plan.member_edits.len(),
        "transitives_pinned": plan.transitive_pins.len(),
        "duplicates_resolved": plan.duplicates_cleaned.len(),
        "features_pruned": plan.pruned_features.len(),
        "optional_features_preserved": plan.optional_features.len(),
        "undeclared_features_fixed": features_fixed,
        "backup_id": created_backup_id,
        "report_path": written_report_path,
        "plan_receipt": plan_receipt,
        "apply_receipt": apply_receipt,
        "graph_delta": {
          "added_facts": graph_delta.added,
          "removed_facts": graph_delta.removed,
          "fingerprint": graph_delta.fingerprint,
          "facts": graph_delta.facts,
        },
        "proof_fingerprint": proof_fingerprint,
        "warnings": warnings,
      }),
    );
    println!("{}", serde_json::to_string_pretty(&output)?);
  }

  Ok(())
}

/// Restore a previous state from backup
pub fn run_unify_undo(workspace_root: &std::path::Path, list: bool, backup_id: Option<String>) -> RailResult<()> {
  use crate::backup::BackupManager;

  let backup_manager = BackupManager::new(workspace_root);

  if list {
    let backups = backup_manager.list_backups()?;

    if backups.is_empty() {
      println!("no backups found");
      return Ok(());
    }

    println!("backups:\n");
    for (i, backup) in backups.iter().enumerate() {
      let marker = if i == 0 { " (latest)" } else { "" };
      println!("  {}{}", backup.id, marker);
      println!("    {}", backup.metadata.timestamp);
      println!("    {} files", backup.metadata.files_modified.len());
    }

    println!("\nrestore with: cargo rail unify undo [--backup-id <id>]");
    return Ok(());
  }

  let target_backup_id = if let Some(id) = backup_id {
    id
  } else {
    match backup_manager.get_latest_backup()? {
      Some(backup) => backup.id,
      None => {
        return Err(crate::error::RailError::with_help(
          "no backups found",
          "run 'cargo rail unify undo --list' to see available backups",
        ));
      }
    }
  };

  backup_manager.restore_backup(&target_backup_id)?;

  Ok(())
}

/// Display detailed explanation of unification decisions
fn display_explain(sink: &mut UnifyTextSink, plan: &crate::cargo::UnificationPlan) {
  use std::collections::BTreeMap;

  outln!(sink);
  outln!(sink, "=== Explanation ===");
  outln!(sink);

  if !plan.dependency_decisions.is_empty() {
    outln!(sink, "Dependency decisions:");
    outln!(sink);
    for decision in &plan.dependency_decisions {
      match (&decision.member, &decision.target) {
        (Some(member), Some(target)) => outln!(
          sink,
          "  {} [{}:{} @ {}]",
          decision.dep_name,
          decision.subject.as_str(),
          member,
          target
        ),
        (Some(member), None) => outln!(
          sink,
          "  {} [{}:{}]",
          decision.dep_name,
          decision.subject.as_str(),
          member
        ),
        (None, _) => outln!(sink, "  {} [{}]", decision.dep_name, decision.subject.as_str()),
      }
      for reason in &decision.reasons {
        outln!(sink, "    - {}: {}", reason.code.as_str(), reason.summary);
        if !reason.features.is_empty() {
          outln!(sink, "      features: {}", format_preview_list(&reason.features, 10));
        }
        if !reason.members.is_empty() {
          outln!(sink, "      members: {}", format_preview_list(&reason.members, 10));
        }
        if !reason.borrowed_from.is_empty() {
          outln!(
            sink,
            "      borrowed from: {}",
            format_preview_list(&reason.borrowed_from, 10)
          );
        }
        for path in &reason.feature_paths {
          outln!(
            sink,
            "      path: {}:{} [{}{}] features={} default-features={} optional={}",
            path.member,
            path.alias,
            path.dependency_kind.as_str(),
            path
              .target
              .as_deref()
              .map(|target| format!(" @ {target}"))
              .unwrap_or_default(),
            format_preview_list(&path.features, 10),
            path.default_features,
            path.optional,
          );
        }
      }
      outln!(sink);
    }
  }

  // Explain unused deps being removed
  if !plan.unused_deps.is_empty() {
    outln!(sink, "Unused dependencies flagged for removal:");
    outln!(sink);
    for unused in &plan.unused_deps {
      outln!(sink, "  {} in {}", unused.dep_name, unused.member);
      outln!(sink, "    reason: {:?}", unused.reason);
      outln!(
        sink,
        "    proof: {}/{} applicable configurations complete; {} unused; {} used; {} incomplete",
        unused.proof.complete_configurations,
        unused.proof.applicable_configurations,
        unused.proof.unused_observations,
        unused.proof.used_observations,
        unused.proof.incomplete_observations
      );
      outln!(
        sink,
        "    cache: {} hit; {} miss{}{}",
        unused.proof.cache_hits,
        unused.proof.cache_misses,
        if unused.proof.cache_misses == 1 { "" } else { "es" },
        if unused.proof.cache_miss_reasons.is_empty() {
          String::new()
        } else {
          format!(" ({})", unused.proof.cache_miss_reasons.join(", "))
        }
      );
      outln!(sink);
    }
  }

  if !plan.reachable_features.is_empty() {
    outln!(sink, "Feature reachability:");
    outln!(sink);
    for feature in &plan.reachable_features {
      outln!(
        sink,
        "  {} in {}: {} ({})",
        feature.feature_name,
        feature.crate_name,
        feature.path.join(" -> "),
        feature.root_kind
      );
    }
    outln!(sink);
  }

  // Explain pruned features
  if !plan.pruned_features.is_empty() {
    outln!(sink, "Dead features pruned:");
    outln!(sink);
    for pruned in &plan.pruned_features {
      outln!(sink, "  [features].{} in {}", pruned.feature_name, pruned.crate_name);
      outln!(sink, "    reason: unreachable from every feature root");
      if !pruned.declared_edges.is_empty() {
        outln!(sink, "    removed edges: {}", pruned.declared_edges.join(", "));
      }
      outln!(sink);
    }
  }

  // Explain issues/blockers
  if !plan.issues.is_empty() {
    outln!(sink, "Issues detected:");
    outln!(sink);

    // Group by severity
    let mut by_severity: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for issue in &plan.issues {
      let severity = format!("{:?}", issue.severity);
      by_severity.entry(severity).or_default().push(issue);
    }

    for (severity, issues) in &by_severity {
      outln!(sink, "  {} ({}):", severity, issues.len());
      for issue in issues {
        outln!(sink, "    {}: {}", issue.dep_name, issue.message);
      }
      outln!(sink);
    }
  }

  if !plan.undeclared_features.is_empty() {
    outln!(sink, "Undeclared feature causality:");
    outln!(sink);
    for uf in &plan.undeclared_features {
      outln!(sink, "  {} in {}", uf.dep_name, uf.member);
      outln!(sink, "    undeclared: [{}]", uf.undeclared_features.join(", "));
      if !uf.borrowed_from.is_empty() {
        outln!(sink, "    borrowed from: {}", uf.borrowed_from.join(", "));
      }
      if !uf.required_by.is_empty() {
        outln!(sink, "    required by: {}", uf.required_by.join(", "));
      }
      if let Some(target) = &uf.target {
        outln!(sink, "    target: {}", target);
      }
      outln!(
        sink,
        "    reason: standalone compiler failure names a feature currently supplied by another member"
      );
      outln!(sink);
    }
  }

  // Explain why no changes if plan is empty
  if plan.workspace_deps.is_empty()
    && plan.member_edits.is_empty()
    && plan.transitive_pins.is_empty()
    && plan.unused_deps.is_empty()
  {
    outln!(sink, "No unification opportunities found.");
    outln!(sink);
    outln!(sink, "Possible reasons:");
    outln!(sink, "  - Dependencies are already unified");
    outln!(sink, "  - Dependencies have incompatible versions (see issues above)");
    outln!(sink, "  - Dependencies are excluded via [unify].exclude config");
    outln!(sink, "  - Dependencies are renamed (use include_renamed = true)");
    outln!(sink, "  - Single-use dependencies (not shared across crates)");
  }
}

/// Check if a workspace is a "virtual" workspace (has [workspace] but no [package])
///
/// Virtual workspaces cannot have [dev-dependencies] directly in their manifest,
/// which affects transitive dependency pinning.
fn is_virtual_workspace(workspace_root: &std::path::Path) -> bool {
  use std::fs;

  let root_manifest = workspace_root.join("Cargo.toml");
  let Ok(content) = fs::read_to_string(&root_manifest) else {
    return false;
  };

  let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
    return false;
  };

  // A virtual workspace has [workspace] but no [package]
  doc.contains_key("workspace") && !doc.contains_key("package")
}

fn workspace_msrv_write_needed(workspace_root: &std::path::Path, msrv: &semver::Version) -> RailResult<bool> {
  use std::fs;

  let root_manifest = workspace_root.join("Cargo.toml");
  let Ok(content) = fs::read_to_string(&root_manifest) else {
    return Ok(true);
  };
  let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
    return Ok(true);
  };

  let desired = format!("{}.{}.{}", msrv.major, msrv.minor, msrv.patch);
  let current = doc
    .get("workspace")
    .and_then(|ws| ws.get("package"))
    .and_then(|pkg| pkg.get("rust-version"))
    .and_then(|v| v.as_str());

  Ok(current != Some(desired.as_str()))
}

fn transitive_pins_host_manifest_path(ctx: &WorkspaceContext) -> RailResult<std::path::PathBuf> {
  let transitive_host_setting = ctx
    .config()
    .and_then(|config| config.unify.transitive_pinning.as_ref())
    .map(|pinning| &pinning.host);
  let is_root_host = matches!(
    transitive_host_setting,
    None | Some(crate::config::TransitiveFeatureHost::Root)
  );

  // For virtual workspaces (no [package] section), we can't use the root as the transitive host
  // because virtual manifests can't have [dev-dependencies].
  if is_root_host && is_virtual_workspace(ctx.workspace_root()) {
    // Auto-select first workspace member as the host
    let members = ctx.graph().workspace_members();
    if members.is_empty() {
      return Err(RailError::with_help(
        "transitive pinning host `root` is incompatible with virtual workspaces".to_string(),
        "Virtual workspaces cannot have [dev-dependencies]. Set transitive_pinning.host to a workspace member path in your rail.toml:\n  \
           [unify]\n  \
           transitive_pinning = { host = \"crates/some-crate\" }"
          .to_string(),
      ));
    }

    let first_member = &members[0];
    if let Some(pkg) = ctx.cargo().get_package(first_member) {
      let member_path = pkg
        .manifest_path
        .parent()
        .ok_or_else(|| RailError::message(format!("Invalid manifest path: {}", pkg.manifest_path)))?;
      return Ok(member_path.join("Cargo.toml").into_std_path_buf());
    }

    return Err(RailError::message("Failed to find a suitable transitive host member"));
  }

  Ok(match transitive_host_setting {
    Some(crate::config::TransitiveFeatureHost::Path(p)) => ctx.workspace_root().join(p).join("Cargo.toml"),
    _ => ctx.workspace_root().join("Cargo.toml"),
  })
}

fn build_unify_mutation_plan(
  ctx: &WorkspaceContext,
  plan: &crate::cargo::UnificationPlan,
  msrv_write_needed: bool,
  backup_enabled: bool,
  no_report: bool,
  report_path: Option<&std::path::PathBuf>,
) -> RailResult<mutation::MutationPlan> {
  let (actions, risks, trace) =
    build_unify_mutation_parts(plan, msrv_write_needed, backup_enabled, no_report, report_path);
  mutation::build_plan(ctx, "unify", actions, risks, trace)
}

fn build_unify_mutation_parts(
  plan: &crate::cargo::UnificationPlan,
  msrv_write_needed: bool,
  backup_enabled: bool,
  no_report: bool,
  report_path: Option<&std::path::PathBuf>,
) -> (Vec<MutationAction>, Vec<MutationRisk>, Vec<MutationTrace>) {
  let mut actions = Vec::with_capacity(8); // Typically few distinct action types
  let mut risks = Vec::with_capacity(4);
  let mut trace = Vec::with_capacity(8);
  let proof_certificates = proof_certificates_to_json(plan, msrv_write_needed);
  let proof_fingerprint = proof_set_fingerprint(&proof_certificates);

  actions.push(
    MutationAction::new(
      "VERIFY_PROOF_SET",
      "portable unify proof certificates",
      Some(proof_fingerprint.clone()),
    )
    .with_payload(serde_json::json!({
      "schema_version": 1,
      "proof_fingerprint": proof_fingerprint,
    })),
  );

  if !plan.workspace_deps.is_empty() {
    actions.push(MutationAction::new(
      "WRITE_WORKSPACE_DEPS",
      "Cargo.toml:[workspace.dependencies]",
      Some(format!("{} dependencies", plan.workspace_deps.len())),
    ));
  }

  if !plan.member_edits.is_empty() {
    actions.push(MutationAction::new(
      "APPLY_MEMBER_EDITS",
      "workspace member manifests",
      Some(format!("{} member(s)", plan.member_edits.len())),
    ));
  }

  if !plan.transitive_pins.is_empty() {
    actions.push(MutationAction::new(
      "APPLY_TRANSITIVE_PINS",
      "transitive host manifest",
      Some(format!("{} pinned dependencies", plan.transitive_pins.len())),
    ));
  }

  if msrv_write_needed {
    actions.push(MutationAction::new(
      "WRITE_WORKSPACE_MSRV",
      "Cargo.toml:[workspace.package.rust-version]",
      None,
    ));
  }

  if backup_enabled {
    actions.push(MutationAction::new("CREATE_BACKUP", "target/cargo-rail/backups", None));
  }

  if !no_report {
    let target = report_path
      .map(|p| p.display().to_string())
      .unwrap_or_else(|| "target/cargo-rail/unify-report.md".to_string());
    actions.push(MutationAction::new("WRITE_REPORT", target, None));
  }

  let error_count = plan
    .issues
    .iter()
    .filter(|issue| issue.severity == crate::cargo::IssueSeverity::Error)
    .count();
  if error_count > 0 {
    risks.push(MutationRisk::new(
      "BLOCKING_ISSUES",
      "high",
      format!("{} blocking issue(s) detected", error_count),
    ));
  }

  if !plan.transitive_pins.is_empty() {
    risks.push(MutationRisk::new(
      "TRANSITIVE_PIN_SIDE_EFFECTS",
      "medium",
      "transitive pinning mutates host dev-dependencies",
    ));
  }

  trace.push(MutationTrace::new(
    "UNIFY_ANALYSIS_COMPLETE",
    format!(
      "planned {} workspace dep(s), {} member edit(s), {} transitive pin(s)",
      plan.workspace_deps.len(),
      plan.member_edit_count(),
      plan.transitive_pins.len()
    ),
  ));
  if !plan.workspace_deps.is_empty() {
    trace.push(MutationTrace::new(
      "UNIFY_VERSION_DECISIONS",
      format!(
        "resolved versions for {} workspace dependency entries",
        plan.workspace_deps.len()
      ),
    ));
  }
  let feature_edit_count: usize = plan
    .member_edits
    .values()
    .flat_map(|edits| edits.iter())
    .filter(|edit| {
      matches!(
        edit,
        crate::cargo::MemberEdit::AddFeatures { .. } | crate::cargo::MemberEdit::RemoveFeature { .. }
      )
    })
    .count();
  if feature_edit_count > 0 {
    trace.push(MutationTrace::new(
      "UNIFY_FEATURE_DECISIONS",
      format!("planned {} feature-level member edit(s)", feature_edit_count),
    ));
  }
  if msrv_write_needed || plan.computed_msrv.is_some() {
    trace.push(MutationTrace::new(
      "UNIFY_MSRV_DECISIONS",
      format!(
        "msrv evaluated: computed={}, write_needed={}",
        plan.computed_msrv.is_some(),
        msrv_write_needed
      ),
    ));
  }
  if !plan.transitive_pins.is_empty() {
    trace.push(MutationTrace::new(
      "UNIFY_TRANSITIVE_DECISIONS",
      format!("planned {} transitive pin decision(s)", plan.transitive_pins.len()),
    ));
  }

  (actions, risks, trace)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::cargo::{
    DepKind, UnificationPlan, UnifyDecision, UnifyDecisionCode, UnifyDecisionReason, UnifyDecisionSubject,
  };
  use rustc_hash::FxHashMap;
  use std::sync::Arc;

  fn arc(value: &str) -> Arc<str> {
    Arc::from(value)
  }

  fn empty_plan() -> UnificationPlan {
    UnificationPlan {
      workspace_deps: Vec::new(),
      member_edits: FxHashMap::default(),
      member_paths: FxHashMap::default(),
      transitive_pins: Vec::new(),
      validation_results: Vec::new(),
      issues: Vec::new(),
      computed_msrv: None,
      duplicates_cleaned: Vec::new(),
      pruned_features: Vec::new(),
      reachable_features: Vec::new(),
      optional_features: Vec::new(),
      version_mismatches: Vec::new(),
      unused_deps: Vec::new(),
      undeclared_features: Vec::new(),
      dependency_decisions: Vec::new(),
    }
  }

  #[test]
  fn test_dependency_decisions_to_json_includes_reason_codes() {
    let mut plan = empty_plan();
    plan.dependency_decisions.push(UnifyDecision {
      dep_name: arc("tokio"),
      subject: UnifyDecisionSubject::UndeclaredFeatureFix,
      member: Some(arc("crate-b")),
      target: Some(arc("cfg(unix)")),
      reasons: vec![UnifyDecisionReason {
        code: UnifyDecisionCode::UndeclaredFeatureFix,
        summary: arc("Added missing features locally."),
        features: vec![arc("macros")],
        members: vec![arc("crate-b")],
        borrowed_from: vec![arc("crate-a")],
        feature_paths: Vec::new(),
      }],
    });

    let json = dependency_decisions_to_json(&plan);
    assert_eq!(json.len(), 1);
    assert_eq!(json[0]["dep_name"], "tokio");
    assert_eq!(json[0]["subject"], "undeclared_feature_fix");
    assert_eq!(json[0]["member"], "crate-b");
    assert_eq!(json[0]["target"], "cfg(unix)");
    assert_eq!(json[0]["reasons"][0]["code"], "undeclared_feature_fix");
    assert_eq!(json[0]["reasons"][0]["features"][0], "macros");
    assert_eq!(json[0]["reasons"][0]["borrowed_from"][0], "crate-a");
  }

  #[test]
  fn test_relative_path_distinguishes_portable_sibling_path_dependencies() {
    assert_eq!(
      relative_path(Path::new("/checkout/workspace"), Path::new("/checkout/vendor/alpha")),
      Some(PathBuf::from("../vendor/alpha"))
    );
    assert_eq!(
      relative_path(Path::new("/checkout/workspace"), Path::new("/checkout/vendor/beta")),
      Some(PathBuf::from("../vendor/beta"))
    );
  }

  #[test]
  fn test_display_explain_renders_dependency_decisions() {
    let mut plan = empty_plan();
    plan.dependency_decisions.extend([
      UnifyDecision {
        dep_name: arc("serde"),
        subject: UnifyDecisionSubject::WorkspaceDependency,
        member: None,
        target: None,
        reasons: vec![
          UnifyDecisionReason {
            code: UnifyDecisionCode::FeatureIntersection,
            summary: arc("Used intersection to keep only shared features."),
            features: vec![arc("derive")],
            members: vec![arc("crate-a"), arc("crate-b")],
            borrowed_from: Vec::new(),
            feature_paths: Vec::new(),
          },
          UnifyDecisionReason {
            code: UnifyDecisionCode::ExactPinWarnCaret,
            summary: arc("Converted exact pin to ^1.0.200."),
            features: Vec::new(),
            members: vec![arc("crate-a"), arc("crate-b")],
            borrowed_from: Vec::new(),
            feature_paths: Vec::new(),
          },
        ],
      },
      UnifyDecision {
        dep_name: arc("tokio"),
        subject: UnifyDecisionSubject::WorkspaceDependency,
        member: None,
        target: None,
        reasons: vec![UnifyDecisionReason {
          code: UnifyDecisionCode::CohortEnforced,
          summary: arc("Enforced atomic workspace-member cohort."),
          features: Vec::new(),
          members: vec![arc("tokio"), arc("tokio-stream"), arc("tokio-util")],
          borrowed_from: Vec::new(),
          feature_paths: Vec::new(),
        }],
      },
      UnifyDecision {
        dep_name: arc("windows-sys"),
        subject: UnifyDecisionSubject::TransitivePin,
        member: None,
        target: None,
        reasons: vec![UnifyDecisionReason {
          code: UnifyDecisionCode::TransitivePin,
          summary: arc("Pinned transitively to stabilize target-specific feature resolution."),
          features: vec![arc("Win32_Foundation")],
          members: Vec::new(),
          borrowed_from: Vec::new(),
          feature_paths: Vec::new(),
        }],
      },
      UnifyDecision {
        dep_name: arc("tokio"),
        subject: UnifyDecisionSubject::UndeclaredFeatureFix,
        member: Some(arc("crate-c")),
        target: None,
        reasons: vec![UnifyDecisionReason {
          code: UnifyDecisionCode::UndeclaredFeatureFix,
          summary: arc("Added missing features to stop borrowed resolver state."),
          features: vec![arc("macros")],
          members: vec![arc("crate-c")],
          borrowed_from: vec![arc("crate-a")],
          feature_paths: Vec::new(),
        }],
      },
    ]);
    plan.member_edits.insert(
      arc("crate-c"),
      vec![crate::cargo::MemberEdit::AddFeatures {
        dep_name: arc("tokio"),
        dep_kind: DepKind::Normal,
        target: None,
        features_to_add: vec![arc("macros")],
      }],
    );

    let mut sink = UnifyTextSink::new(true);
    display_explain(&mut sink, &plan);
    let output = sink.finish().expect("captured output");

    assert!(output.contains("Dependency decisions:"));
    assert!(output.contains("intersection: Used intersection to keep only shared features."));
    assert!(output.contains("exact_pin_warn_caret: Converted exact pin to ^1.0.200."));
    assert!(output.contains("cohort_enforced: Enforced atomic workspace-member cohort."));
    assert!(output.contains("transitive_pin: Pinned transitively to stabilize target-specific feature resolution."));
    assert!(output.contains("undeclared_feature_fix: Added missing features to stop borrowed resolver state."));
    assert!(output.contains("borrowed from: crate-a"));
  }
}
