//! Target-aware compiler diagnostics collection with persistent caching.

use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::compiler::diagnostics_store::CompilerDiagnosticsStore;
use crate::compiler::model::{COLLECTOR_VERSION, CompilerDiagEntry, CompilerDiagKey, DiagnosticsCompleteness};
use crate::error::{RailError, RailResult, ResultExt};
use crate::utils::{file_fingerprint, fnv1a64};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Compiler diagnostics for one workspace member, grouped by target triple.
#[derive(Debug, Default, Clone)]
pub struct MemberDiagnostics {
  /// `target_triple` -> package-unused dependency names.
  pub unused_by_target: HashMap<String, BTreeSet<String>>,
  /// `target_triple` -> diagnostics completeness.
  pub completeness_by_target: HashMap<String, DiagnosticsCompleteness>,
}

impl MemberDiagnostics {
  /// Returns `true` when `dep_name` is reported unused for every target in `required_targets`.
  #[must_use]
  pub fn is_unused_for_all_targets(&self, dep_name: &str, required_targets: &[&str]) -> bool {
    required_targets.iter().all(|target| {
      self
        .unused_by_target
        .get(*target)
        .is_some_and(|deps| deps.contains(dep_name))
    })
  }

  /// Returns `true` when diagnostics are present for all required targets.
  #[must_use]
  pub fn has_all_targets(&self, required_targets: &[&str]) -> bool {
    required_targets
      .iter()
      .all(|target| self.unused_by_target.contains_key(*target))
  }

  /// Returns `true` when all required targets have complete diagnostics.
  #[must_use]
  pub fn has_complete_targets(&self, required_targets: &[&str]) -> bool {
    required_targets.iter().all(|target| {
      self
        .completeness_by_target
        .get(*target)
        .is_some_and(|status| *status == DiagnosticsCompleteness::Complete)
    })
  }
}

/// Compiler diagnostics collector and cache coordinator.
pub struct CompilerDiagnosticsCollector<'a> {
  workspace_root: &'a Path,
  manifests: &'a ManifestAnalyzer,
  targets: Vec<&'a str>,
  rustc_version: String,
  host_triple: String,
  lock_fingerprint: String,
  compiler_env_fingerprint: String,
  cargo_config_fingerprint: String,
  enable_cache: bool,
}

impl<'a> CompilerDiagnosticsCollector<'a> {
  /// Create a new collector for a workspace-level analysis pass.
  pub fn new(
    workspace_root: &'a Path,
    manifests: &'a ManifestAnalyzer,
    targets: Vec<&'a str>,
    enable_cache: bool,
  ) -> RailResult<Self> {
    let (rustc_version, host_triple) = rustc_identity(workspace_root)?;
    let lock_fingerprint = file_fingerprint(&workspace_root.join("Cargo.lock"));
    let compiler_env_fingerprint = compiler_env_fingerprint();
    let cargo_config_fingerprint = cargo_config_fingerprint(workspace_root);

    Ok(Self {
      workspace_root,
      manifests,
      targets,
      rustc_version,
      host_triple,
      lock_fingerprint,
      compiler_env_fingerprint,
      cargo_config_fingerprint,
      enable_cache,
    })
  }

  /// Collect diagnostics for selected workspace members.
  pub fn collect_for_members(&self, members: &HashSet<&str>) -> RailResult<HashMap<String, MemberDiagnostics>> {
    if members.is_empty() {
      return Ok(HashMap::new());
    }

    let mut store = self
      .enable_cache
      .then(|| CompilerDiagnosticsStore::load(self.workspace_root));
    let key_inputs = self.build_key_inputs(members);
    let manifest_to_member = build_manifest_member_index(&self.manifests.members);

    let mut result: HashMap<String, MemberDiagnostics> = HashMap::with_capacity(members.len());
    let mut stale_by_target: HashMap<String, Vec<&str>> = HashMap::new();

    for (member, target, key) in key_inputs {
      if let Some(store) = store.as_ref()
        && let Some(entry) = store.get(&key)
      {
        result
          .entry(member.to_string())
          .or_default()
          .unused_by_target
          .insert(target.to_string(), entry.unused_deps.clone());
        result
          .entry(member.to_string())
          .or_default()
          .completeness_by_target
          .insert(target.to_string(), entry.completeness);
        continue;
      }

      stale_by_target.entry(target.to_string()).or_default().push(member);
    }

    for target in &self.targets {
      let Some(stale_members) = stale_by_target.get(*target) else {
        continue;
      };
      if stale_members.is_empty() {
        continue;
      }

      let mut stale_set = HashSet::with_capacity(stale_members.len());
      for member in stale_members {
        stale_set.insert(*member);
      }

      let run = run_workspace_check(self.workspace_root, target)?;
      let parsed = parse_target_run(&run.stdout, &manifest_to_member, &stale_set);
      let completeness = if run.success {
        DiagnosticsCompleteness::Complete
      } else {
        DiagnosticsCompleteness::Incomplete
      };

      for member in stale_members {
        let manifests_member = self
          .manifests
          .members
          .iter()
          .find(|m| m.package_name == *member)
          .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;

        let manifest_fp = file_fingerprint(&manifests_member.path);
        let source_fp = source_tree_fingerprint(
          manifests_member
            .path
            .parent()
            .unwrap_or(manifests_member.path.as_path()),
        );

        let key = CompilerDiagKey {
          member: (*member).to_string(),
          target_triple: (*target).to_string(),
          rustc_version: self.rustc_version.clone(),
          host_triple: self.host_triple.clone(),
          lock_fingerprint: self.lock_fingerprint.clone(),
          manifest_fingerprint: manifest_fp,
          source_fingerprint: source_fp,
          compiler_env_fingerprint: self.compiler_env_fingerprint.clone(),
          cargo_config_fingerprint: self.cargo_config_fingerprint.clone(),
        };

        let mut unused = BTreeSet::new();
        let mut compiled = BTreeSet::new();

        if completeness == DiagnosticsCompleteness::Complete
          && let Some(parsed_member) = parsed.get(*member)
        {
          compiled = parsed_member.compiled_targets.clone();
          if !compiled.is_empty() {
            for (dep_name, warned_targets) in &parsed_member.warned_targets_by_dep {
              if compiled.iter().all(|id| warned_targets.contains(id)) {
                unused.insert(dep_name.clone());
              }
            }
          }
        }

        let entry = CompilerDiagEntry {
          key,
          unused_deps: unused.clone(),
          compiled_crate_targets: compiled,
          generated_at_unix_ms: now_unix_ms(),
          collector_version: COLLECTOR_VERSION,
          completeness,
        };

        if let Some(store) = store.as_mut() {
          store.put(entry);
        }
        result
          .entry((*member).to_string())
          .or_default()
          .unused_by_target
          .insert((*target).to_string(), unused);
        result
          .entry((*member).to_string())
          .or_default()
          .completeness_by_target
          .insert((*target).to_string(), completeness);
      }
    }

    if let Some(store) = store.as_mut() {
      store.flush()?;
    }

    Ok(result)
  }

  fn build_key_inputs(&self, members: &HashSet<&str>) -> Vec<(&str, &str, CompilerDiagKey)> {
    let mut keys = Vec::with_capacity(members.len() * self.targets.len());

    for member in &self.manifests.members {
      if !members.contains(member.package_name.as_str()) {
        continue;
      }

      let manifest_fp = file_fingerprint(&member.path);
      let source_fp = source_tree_fingerprint(member.path.parent().unwrap_or(member.path.as_path()));

      for target in &self.targets {
        keys.push((
          member.package_name.as_str(),
          *target,
          CompilerDiagKey {
            member: member.package_name.clone(),
            target_triple: (*target).to_string(),
            rustc_version: self.rustc_version.clone(),
            host_triple: self.host_triple.clone(),
            lock_fingerprint: self.lock_fingerprint.clone(),
            manifest_fingerprint: manifest_fp.clone(),
            source_fingerprint: source_fp.clone(),
            compiler_env_fingerprint: self.compiler_env_fingerprint.clone(),
            cargo_config_fingerprint: self.cargo_config_fingerprint.clone(),
          },
        ));
      }
    }

    keys
  }
}

#[derive(Debug)]
struct WorkspaceCheckOutput {
  stdout: String,
  success: bool,
}

fn run_workspace_check(workspace_root: &Path, target: &str) -> RailResult<WorkspaceCheckOutput> {
  let rustflags = std::env::var("RUSTFLAGS").ok();
  let lint_flag = "-Wunused-crate-dependencies";
  let merged_rustflags = match rustflags {
    Some(flags) if flags.split_whitespace().any(|flag| flag == lint_flag) => flags,
    Some(flags) => format!("{} {}", flags, lint_flag),
    None => lint_flag.to_string(),
  };

  let mut args = vec![
    "check",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--message-format=json",
  ];
  if target != "default" {
    args.push("--target");
    args.push(target);
  }

  let output = Command::new("cargo")
    .current_dir(workspace_root)
    .env("RUSTFLAGS", merged_rustflags)
    .args(&args)
    .output()
    .with_context(|| {
      format!(
        "running cargo check for target '{target}' in {}",
        workspace_root.display()
      )
    })?;

  Ok(WorkspaceCheckOutput {
    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    success: output.status.success(),
  })
}

#[derive(Debug, Default)]
struct ParsedMemberTarget {
  compiled_targets: BTreeSet<String>,
  warned_targets_by_dep: HashMap<String, BTreeSet<String>>,
}

fn parse_target_run(
  stdout: &str,
  manifest_to_member: &HashMap<String, String>,
  stale_members: &HashSet<&str>,
) -> HashMap<String, ParsedMemberTarget> {
  let mut parsed: HashMap<String, ParsedMemberTarget> = HashMap::new();

  for line in stdout.lines() {
    let Ok(message) = serde_json::from_str::<CargoEvent>(line) else {
      continue;
    };
    if message.reason != "compiler-message" && message.reason != "compiler-artifact" {
      continue;
    }

    let Some(manifest_path) = message.manifest_path.as_deref() else {
      continue;
    };
    let Some(member_name) = manifest_to_member.get(manifest_path) else {
      continue;
    };
    if !stale_members.contains(member_name.as_str()) {
      continue;
    }

    let Some(target) = message.target.as_ref() else {
      continue;
    };
    if !is_relevant_target(target) {
      continue;
    }

    let target_id = target.identifier();
    parsed
      .entry(member_name.clone())
      .or_default()
      .compiled_targets
      .insert(target_id.clone());

    if message.reason != "compiler-message" {
      continue;
    }

    let Some(diagnostic) = message.message.as_ref() else {
      continue;
    };
    if diagnostic.code.as_ref().and_then(|c| c.code.as_deref()) != Some("unused_crate_dependencies") {
      continue;
    }
    let Some(crate_name) = parse_unused_crate_name(&diagnostic.message) else {
      continue;
    };

    parsed
      .entry(member_name.clone())
      .or_default()
      .warned_targets_by_dep
      .entry(crate_name.replace('-', "_"))
      .or_default()
      .insert(target_id);
  }

  parsed
}

fn rustc_identity(workspace_root: &Path) -> RailResult<(String, String)> {
  let output = Command::new("rustc")
    .current_dir(workspace_root)
    .arg("-vV")
    .output()
    .with_context(|| "running rustc -vV".to_string())?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let mut version = String::new();
  let mut host = String::new();

  for line in stdout.lines() {
    if let Some(value) = line.strip_prefix("release: ") {
      version = value.trim().to_string();
      continue;
    }
    if let Some(value) = line.strip_prefix("host: ") {
      host = value.trim().to_string();
    }
  }

  if version.is_empty() {
    version = "unknown".to_string();
  }
  if host.is_empty() {
    host = "unknown".to_string();
  }

  Ok((version, host))
}

fn source_tree_fingerprint(member_dir: &Path) -> String {
  let mut hash: u64 = 0xcbf29ce484222325;
  let mut roots = vec![
    member_dir.join("src"),
    member_dir.join("tests"),
    member_dir.join("examples"),
    member_dir.join("benches"),
    member_dir.join("build.rs"),
  ];
  roots.sort_unstable();

  for path in roots {
    hash_path_metadata(member_dir, &path, &mut hash);
  }

  format!("fnv1a64:{hash:016x}")
}

fn hash_path_metadata(base: &Path, path: &Path, hash: &mut u64) {
  if !path.exists() {
    hash_bytes(hash, b"missing");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    return;
  }

  let Ok(metadata) = std::fs::metadata(path) else {
    hash_bytes(hash, b"metadata-error");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    return;
  };

  if metadata.is_file() {
    hash_bytes(hash, b"file");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    hash_bytes(hash, &metadata.len().to_le_bytes());
    let modified = metadata
      .modified()
      .ok()
      .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
      .map(|d| d.as_nanos() as u64)
      .unwrap_or(0);
    hash_bytes(hash, &modified.to_le_bytes());
    return;
  }

  if metadata.is_dir() {
    hash_bytes(hash, b"dir");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );

    let Ok(entries) = std::fs::read_dir(path) else {
      hash_bytes(hash, b"read-dir-error");
      return;
    };

    let mut child_paths = Vec::new();
    for entry in entries.flatten() {
      child_paths.push(entry.path());
    }
    child_paths.sort_unstable();

    for child in child_paths {
      hash_path_metadata(base, &child, hash);
    }
  }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
  const FNV_PRIME: u64 = 0x100000001b3;
  for byte in bytes {
    *hash ^= u64::from(*byte);
    *hash = hash.wrapping_mul(FNV_PRIME);
  }
}

fn now_unix_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

fn compiler_env_fingerprint() -> String {
  let rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
  let encoded_rustflags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
  let combined = format!("RUSTFLAGS={rustflags}\nCARGO_ENCODED_RUSTFLAGS={encoded_rustflags}");
  format!("fnv1a64:{:016x}", fnv1a64(combined.as_bytes()))
}

fn cargo_config_fingerprint(workspace_root: &Path) -> String {
  let cfg_toml = file_fingerprint(&workspace_root.join(".cargo").join("config.toml"));
  let cfg_legacy = file_fingerprint(&workspace_root.join(".cargo").join("config"));
  let combined = format!("{cfg_toml}\n{cfg_legacy}");
  format!("fnv1a64:{:016x}", fnv1a64(combined.as_bytes()))
}

fn build_manifest_member_index(members: &[crate::cargo::manifest_analyzer::ParsedManifest]) -> HashMap<String, String> {
  let mut index = HashMap::with_capacity(members.len() * 2);
  for member in members {
    index.insert(member.path.to_string_lossy().into_owned(), member.package_name.clone());
    if let Ok(canonical) = member.path.canonicalize() {
      index.insert(canonical.to_string_lossy().into_owned(), member.package_name.clone());
    }
  }
  index
}

fn parse_unused_crate_name(message: &str) -> Option<&str> {
  let prefix = "extern crate `";
  let start = message.find(prefix)? + prefix.len();
  let rest = &message[start..];
  let end = rest.find('`')?;
  Some(&rest[..end])
}

#[derive(Debug, Deserialize)]
struct CargoEvent {
  reason: String,
  manifest_path: Option<String>,
  target: Option<CargoTarget>,
  message: Option<CargoDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
  kind: Vec<String>,
  name: String,
  src_path: Option<String>,
}

impl CargoTarget {
  fn identifier(&self) -> String {
    match &self.src_path {
      Some(src_path) => src_path.clone(),
      None => format!("{}:{}", self.kind.join(","), self.name),
    }
  }
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
  message: String,
  code: Option<CargoDiagnosticCode>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnosticCode {
  code: Option<String>,
}

fn is_relevant_target(target: &CargoTarget) -> bool {
  !target.kind.iter().any(|kind| kind == "custom-build")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_source_tree_fingerprint_changes_with_mtime_or_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let member = temp.path();
    std::fs::create_dir_all(member.join("src")).expect("mkdir");
    std::fs::write(member.join("src/lib.rs"), "pub fn a() {}\n").expect("write1");

    let before = source_tree_fingerprint(member);
    std::fs::write(member.join("src/lib.rs"), "pub fn a() { let _ = 1; }\n").expect("write2");
    let after = source_tree_fingerprint(member);

    assert_ne!(before, after);
  }
}
