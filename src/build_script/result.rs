//! Immutable post-execution evidence for one build-script action.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compiler::observation::{EnvironmentObservation, is_secret_name};
use crate::source::{ContentDigest, RepositoryPath, SourceEntryKind, SourceTree};

const BUILD_SCRIPT_RESULT_VERSION: u32 = 1;
const BUILD_SCRIPT_RESULT_SEMANTICS_VERSION: u32 = 1;

/// Redaction-safe diagnostic result of deriving one build-script result digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildScriptResultAnalysis {
  version: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  digest: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  instructions: Option<BuildScriptInstructionSummary>,
  #[serde(skip_serializing_if = "Option::is_none")]
  environment_reads: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  generated_outputs: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  cargo_output: Option<BuildScriptCargoOutputSummary>,
  #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
  secret_capabilities: BTreeSet<String>,
  reasons: BTreeSet<String>,
}

impl BuildScriptResultAnalysis {
  /// Return the verified post-execution result digest when evidence is complete.
  pub(crate) fn digest(&self) -> Option<&str> {
    self.digest.as_deref()
  }
}

/// Counts from Cargo's stable, normalized `build-script-executed` message.
///
/// Cargo can replay this message without executing the script, and it omits the
/// raw instruction stream and generated tree. It is diagnostic evidence only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildScriptCargoOutputSummary {
  pub(crate) linked_libraries: usize,
  pub(crate) linked_paths: usize,
  pub(crate) cfgs: usize,
  pub(crate) rustc_environment: usize,
  pub(crate) output_directory_reported: bool,
}

/// Minimal stable facts from the process boundary that produced the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildScriptExecutionObservation {
  pub(crate) success: bool,
  pub(crate) platform_identity: String,
}

/// Complete post-execution inputs required to issue a result digest.
///
/// `instruction_stream` contains only Cargo instruction lines, in emitted
/// order. A canonical logical output tree excludes the physical `OUT_DIR`.
#[derive(Debug, Clone)]
pub(crate) struct BuildScriptResultInputs {
  pub(crate) instruction_stream: Option<Vec<String>>,
  pub(crate) environment_reads: Option<BTreeSet<EnvironmentObservation>>,
  pub(crate) generated_outputs: Option<SourceTree>,
  pub(crate) execution: Option<BuildScriptExecutionObservation>,
  pub(crate) secret_capabilities: BTreeSet<String>,
  pub(crate) limitations: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BuildScriptInstructionSummary {
  total: usize,
  rerun_declarations: usize,
  linked_libraries: usize,
  linked_paths: usize,
  link_arguments: usize,
  cfgs: usize,
  check_cfgs: usize,
  rustc_environment: usize,
  metadata: usize,
  warnings: usize,
  errors: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstructionSyntax {
  Modern,
  Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuildScriptResultDigest {
  version: u32,
  digest: ContentDigest,
}

impl fmt::Display for BuildScriptResultDigest {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      formatter,
      "build-script-result-v{}-sha256-{}",
      self.version, self.digest
    )
  }
}

/// Issue a result digest only for complete, portable post-execution evidence.
pub(crate) fn analyze_result(
  source_root: &Path,
  inputs: BuildScriptResultInputs,
  cargo_output: Option<BuildScriptCargoOutputSummary>,
) -> BuildScriptResultAnalysis {
  let BuildScriptResultInputs {
    instruction_stream,
    environment_reads,
    generated_outputs,
    execution,
    mut secret_capabilities,
    limitations: mut reasons,
  } = inputs;
  let instructions = instruction_stream
    .as_deref()
    .map(|stream| summarize_instructions(stream, &mut reasons));
  if instruction_stream.is_none() {
    reasons.insert("build_script_instruction_stream_unavailable".to_string());
  }
  if environment_reads.is_none() {
    reasons.insert("build_script_environment_reads_unavailable".to_string());
  }
  if generated_outputs.is_none() {
    reasons.insert("build_script_generated_output_tree_unavailable".to_string());
  }
  if execution.is_none() {
    reasons.insert("build_script_execution_observations_unavailable".to_string());
  }

  if let Some(instructions) = &instruction_stream {
    secret_capabilities.extend(instructions.iter().filter_map(|instruction| {
      let (syntax, name, value) = instruction_parts(instruction)?;
      if syntax == InstructionSyntax::Legacy && !legacy_instruction_name(name) {
        return None;
      }
      let (environment_name, _) = (name == "rustc-env").then_some(value)?.split_once('=')?;
      is_secret_name(environment_name).then(|| environment_name.to_string())
    }));
  }
  if let Some(environment) = &environment_reads {
    for read in environment {
      if !valid_environment_name(&read.name)
        || read
          .value_digest
          .as_deref()
          .is_some_and(|digest| !valid_sha256_digest(digest))
      {
        reasons.insert("build_script_environment_read_invalid".to_string());
      }
      if read.secret_capability {
        secret_capabilities.insert(read.name.clone());
      }
    }
  }
  let mut invalid_secret_capability = false;
  secret_capabilities.retain(|name| {
    let valid = valid_environment_name(name);
    invalid_secret_capability |= !valid;
    valid
  });
  if invalid_secret_capability {
    reasons.insert("build_script_secret_capability_invalid".to_string());
  }
  if !secret_capabilities.is_empty() {
    reasons.insert("secret_build_script_result".to_string());
  }

  if let Some(outputs) = &generated_outputs {
    for output in outputs.entries() {
      if output.path.as_str().contains('\\') {
        reasons.insert("build_script_output_path_not_portable".to_string());
      }
      match &output.kind {
        SourceEntryKind::RegularFile { .. } => {}
        SourceEntryKind::Symlink { target } => {
          if target.contains('\0') {
            reasons.insert("build_script_output_tree_invalid".to_string());
          } else if symlink_target_escapes(&output.path, target) {
            reasons.insert("build_script_output_symlink_escape".to_string());
          }
        }
        SourceEntryKind::Deleted => {
          reasons.insert("build_script_output_tree_invalid".to_string());
        }
      }
    }
  }

  if let Some(execution) = &execution {
    if !execution.success {
      reasons.insert("build_script_execution_failed".to_string());
    }
    if execution.platform_identity.is_empty() {
      reasons.insert("build_script_execution_platform_unavailable".to_string());
    }
  }

  if instruction_stream.is_some()
    || execution.is_some()
    || generated_outputs.as_ref().is_some_and(|outputs| {
      outputs
        .entries()
        .iter()
        .any(|output| matches!(output.kind, SourceEntryKind::Symlink { .. }))
    })
  {
    let physical_roots = super::physical_source_roots(source_root);
    let contains_root = |value: &str| physical_roots.iter().any(|root| value.contains(root));
    if instruction_stream
      .as_ref()
      .is_some_and(|stream| stream.iter().any(|instruction| contains_root(instruction)))
      || generated_outputs.as_ref().is_some_and(|outputs| {
        outputs
          .entries()
          .iter()
          .any(|output| matches!(&output.kind, SourceEntryKind::Symlink { target } if contains_root(target)))
      })
      || execution
        .as_ref()
        .is_some_and(|execution| contains_root(&execution.platform_identity))
    {
      reasons.insert("physical_checkout_path_in_build_script_result".to_string());
    }
  }

  let digest = if reasons.is_empty() {
    match (
      instruction_stream.as_deref(),
      environment_reads.as_ref(),
      generated_outputs.as_ref(),
      execution.as_ref(),
    ) {
      (Some(instructions), Some(environment), Some(outputs), Some(execution)) => Some(BuildScriptResultDigest {
        version: BUILD_SCRIPT_RESULT_VERSION,
        digest: result_digest(instructions, environment, outputs, execution),
      }),
      _ => None,
    }
  } else {
    None
  }
  .map(|digest| digest.to_string());
  if reasons.is_empty() && digest.is_none() {
    reasons.insert("build_script_result_incomplete".to_string());
  }

  BuildScriptResultAnalysis {
    version: BUILD_SCRIPT_RESULT_VERSION,
    digest,
    instructions,
    environment_reads: environment_reads.as_ref().map(BTreeSet::len),
    generated_outputs: generated_outputs.as_ref().map(|outputs| outputs.entries().len()),
    cargo_output,
    secret_capabilities,
    reasons,
  }
}

fn summarize_instructions(instructions: &[String], reasons: &mut BTreeSet<String>) -> BuildScriptInstructionSummary {
  let mut summary = BuildScriptInstructionSummary {
    total: instructions.len(),
    rerun_declarations: 0,
    linked_libraries: 0,
    linked_paths: 0,
    link_arguments: 0,
    cfgs: 0,
    check_cfgs: 0,
    rustc_environment: 0,
    metadata: 0,
    warnings: 0,
    errors: 0,
  };
  for instruction in instructions {
    let Some((syntax, name, value)) = instruction_parts(instruction) else {
      reasons.insert("build_script_instruction_stream_invalid".to_string());
      continue;
    };
    if syntax == InstructionSyntax::Legacy && !legacy_instruction_name(name) {
      summary.metadata += 1;
      continue;
    }
    match name {
      "rerun-if-changed" | "rerun-if-env-changed" => summary.rerun_declarations += 1,
      "rustc-link-lib" => summary.linked_libraries += 1,
      "rustc-link-search" => summary.linked_paths += 1,
      "rustc-flags" => {
        if !summarize_rustc_flags(value, &mut summary) {
          reasons.insert("build_script_instruction_stream_invalid".to_string());
        }
      }
      "rustc-cdylib-link-arg" => summary.link_arguments += 1,
      name if name.starts_with("rustc-link-arg") => summary.link_arguments += 1,
      "rustc-cfg" => summary.cfgs += 1,
      "rustc-check-cfg" => summary.check_cfgs += 1,
      "rustc-env" if value.split_once('=').is_some_and(|(name, _)| !name.is_empty()) => {
        summary.rustc_environment += 1;
      }
      "metadata" if value.split_once('=').is_some_and(|(name, _)| !name.is_empty()) => summary.metadata += 1,
      "warning" => summary.warnings += 1,
      "error" if syntax == InstructionSyntax::Modern => {
        summary.errors += 1;
        reasons.insert("build_script_error_instruction_emitted".to_string());
      }
      "rustc-env" | "metadata" => {
        reasons.insert("build_script_instruction_stream_invalid".to_string());
      }
      _ => {
        reasons.insert("build_script_instruction_stream_invalid".to_string());
      }
    }
  }
  summary
}

fn instruction_parts(instruction: &str) -> Option<(InstructionSyntax, &str, &str)> {
  if instruction.contains(['\n', '\r', '\0']) {
    return None;
  }
  let (syntax, body) = if let Some(body) = instruction.strip_prefix("cargo::") {
    (InstructionSyntax::Modern, body)
  } else {
    (InstructionSyntax::Legacy, instruction.strip_prefix("cargo:")?)
  };
  let (name, value) = body.split_once('=')?;
  (!name.is_empty()).then_some((syntax, name, value))
}

fn legacy_instruction_name(name: &str) -> bool {
  matches!(
    name,
    "rustc-flags"
      | "rustc-link-lib"
      | "rustc-link-search"
      | "rustc-link-arg-cdylib"
      | "rustc-cdylib-link-arg"
      | "rustc-link-arg-bins"
      | "rustc-link-arg-bin"
      | "rustc-link-arg-tests"
      | "rustc-link-arg-benches"
      | "rustc-link-arg-examples"
      | "rustc-link-arg"
      | "rustc-cfg"
      | "rustc-check-cfg"
      | "rustc-env"
      | "warning"
      | "rerun-if-changed"
      | "rerun-if-env-changed"
  )
}

fn summarize_rustc_flags(value: &str, summary: &mut BuildScriptInstructionSummary) -> bool {
  let mut flags = value.split_whitespace();
  while let Some(flag) = flags.next() {
    let (kind, attached) = if let Some(attached) = flag.strip_prefix("-l") {
      ("-l", attached)
    } else if let Some(attached) = flag.strip_prefix("-L") {
      ("-L", attached)
    } else {
      return false;
    };
    if attached.is_empty() && flags.next().is_none() {
      return false;
    }
    match kind {
      "-l" => summary.linked_libraries += 1,
      "-L" => summary.linked_paths += 1,
      _ => return false,
    }
  }
  true
}

fn valid_sha256_digest(digest: &str) -> bool {
  digest
    .strip_prefix("sha256:")
    .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
}

fn valid_environment_name(name: &str) -> bool {
  !name.is_empty() && !name.contains(['=', '\0'])
}

fn symlink_target_escapes(path: &RepositoryPath, target: &str) -> bool {
  let bytes = target.as_bytes();
  let windows_drive = matches!(
    (bytes.first(), bytes.get(1)),
    (Some(first), Some(b':')) if first.is_ascii_alphabetic()
  );
  if target.is_empty() || target.starts_with(['/', '\\']) || windows_drive {
    return true;
  }
  let mut depth = path.as_str().split('/').count().saturating_sub(1);
  for component in target.split(['/', '\\']) {
    match component {
      "" | "." => {}
      ".." if depth == 0 => return true,
      ".." => depth -= 1,
      _ => depth += 1,
    }
  }
  false
}

fn result_digest(
  instructions: &[String],
  environment: &BTreeSet<EnvironmentObservation>,
  outputs: &SourceTree,
  execution: &BuildScriptExecutionObservation,
) -> ContentDigest {
  let mut material = Vec::from(&b"cargo-rail-build-script-result\0"[..]);
  append_frame(&mut material, b"version", &BUILD_SCRIPT_RESULT_VERSION.to_le_bytes());
  append_frame(
    &mut material,
    b"semantics-version",
    &BUILD_SCRIPT_RESULT_SEMANTICS_VERSION.to_le_bytes(),
  );
  for instruction in instructions {
    append_frame(&mut material, b"instruction", instruction.as_bytes());
  }
  for read in environment {
    append_frame(&mut material, b"environment-name", read.name.as_bytes());
    append_frame(
      &mut material,
      b"environment-value",
      read.value_digest.as_deref().unwrap_or("unset").as_bytes(),
    );
  }
  for output in outputs.entries() {
    append_frame(&mut material, b"output-path", output.path.as_str().as_bytes());
    match &output.kind {
      SourceEntryKind::RegularFile { digest, executable } => {
        append_frame(&mut material, b"output-kind", b"file");
        append_frame(&mut material, b"output-digest", digest.as_bytes());
        append_frame(&mut material, b"output-executable", &[u8::from(*executable)]);
      }
      SourceEntryKind::Symlink { target } => {
        append_frame(&mut material, b"output-kind", b"symlink");
        append_frame(&mut material, b"output-target", target.as_bytes());
      }
      SourceEntryKind::Deleted => append_frame(&mut material, b"output-kind", b"deleted"),
    }
  }
  append_frame(&mut material, b"execution-success", &[u8::from(execution.success)]);
  append_frame(
    &mut material,
    b"execution-platform",
    execution.platform_identity.as_bytes(),
  );
  ContentDigest::sha256(&material)
}

fn append_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use crate::source::SourceTreeEntry;

  use super::*;

  fn output_tree(bytes: &[u8], executable: bool) -> SourceTree {
    SourceTree::new(vec![
      SourceTreeEntry::regular_file(Path::new("generated.rs"), ContentDigest::sha256(bytes), executable)
        .expect("logical generated output"),
      SourceTreeEntry::symlink(Path::new("current.rs"), Path::new("generated.rs")).expect("logical generated symlink"),
    ])
    .expect("canonical generated output tree")
  }

  fn complete_inputs() -> BuildScriptResultInputs {
    BuildScriptResultInputs {
      instruction_stream: Some(vec![
        "cargo::rerun-if-changed=wrapper.h".to_string(),
        "cargo::rerun-if-env-changed=CC".to_string(),
        "cargo::rustc-link-search=native=output:lib".to_string(),
        "cargo::rustc-link-lib=static=wrapper".to_string(),
        "cargo::rustc-link-arg=-Wl,wrapper".to_string(),
        "cargo::rustc-cfg=has_wrapper".to_string(),
        "cargo::rustc-env=WRAPPER_VERSION=1".to_string(),
        "cargo::metadata=include=output:include".to_string(),
        "cargo::warning=using bundled wrapper".to_string(),
      ]),
      environment_reads: Some(BTreeSet::from([EnvironmentObservation {
        name: "CC".to_string(),
        value_digest: Some(format!("sha256:{}", ContentDigest::sha256(b"clang"))),
        secret_capability: false,
      }])),
      generated_outputs: Some(output_tree(b"pub const VALUE: u8 = 1;", false)),
      execution: Some(BuildScriptExecutionObservation {
        success: true,
        platform_identity: "sha256:platform".to_string(),
      }),
      secret_capabilities: BTreeSet::new(),
      limitations: BTreeSet::new(),
    }
  }

  fn digest(root: &Path, inputs: BuildScriptResultInputs) -> Option<String> {
    analyze_result(root, inputs, None).digest
  }

  #[test]
  fn result_digest_changes_for_every_result_domain() {
    type Mutation = Box<dyn Fn(&mut BuildScriptResultInputs)>;

    let root = tempfile::tempdir().expect("source root");
    let inputs = complete_inputs();
    let baseline = digest(root.path(), inputs.clone()).expect("complete result digest");
    assert!(baseline.starts_with("build-script-result-v1-sha256-"));
    let mut mutations: Vec<(&str, Mutation)> = (0..9)
      .map(|index| {
        (
          "instruction",
          Box::new(move |inputs: &mut BuildScriptResultInputs| {
            inputs.instruction_stream.as_mut().unwrap()[index].push('0');
          }) as Mutation,
        )
      })
      .collect();
    mutations.extend([
      (
        "environment read",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.environment_reads = Some(BTreeSet::from([EnvironmentObservation {
            name: "CC".to_string(),
            value_digest: Some(format!("sha256:{}", ContentDigest::sha256(b"gcc"))),
            secret_capability: false,
          }]));
        }) as Mutation,
      ),
      (
        "environment name",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          let read = inputs.environment_reads.as_mut().unwrap().pop_first().unwrap();
          inputs
            .environment_reads
            .as_mut()
            .unwrap()
            .insert(EnvironmentObservation {
              name: "CXX".to_string(),
              ..read
            });
        }),
      ),
      (
        "generated bytes",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.generated_outputs = Some(output_tree(b"pub const VALUE: u8 = 2;", false));
        }),
      ),
      (
        "generated mode",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.generated_outputs = Some(output_tree(b"pub const VALUE: u8 = 1;", true));
        }),
      ),
      (
        "generated path",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.generated_outputs = Some(
            SourceTree::new(vec![
              SourceTreeEntry::regular_file(
                Path::new("renamed.rs"),
                ContentDigest::sha256(b"pub const VALUE: u8 = 1;"),
                false,
              )
              .unwrap(),
              SourceTreeEntry::symlink(Path::new("current.rs"), Path::new("generated.rs")).unwrap(),
            ])
            .unwrap(),
          );
        }),
      ),
      (
        "generated symlink target",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.generated_outputs = Some(
            SourceTree::new(vec![
              SourceTreeEntry::regular_file(
                Path::new("generated.rs"),
                ContentDigest::sha256(b"pub const VALUE: u8 = 1;"),
                false,
              )
              .unwrap(),
              SourceTreeEntry::symlink(Path::new("current.rs"), Path::new("other.rs")).unwrap(),
            ])
            .unwrap(),
          );
        }),
      ),
      (
        "platform",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.execution.as_mut().unwrap().platform_identity.push('0');
        }),
      ),
      (
        "status",
        Box::new(|inputs: &mut BuildScriptResultInputs| {
          inputs.execution.as_mut().unwrap().success = false;
        }),
      ),
    ]);

    for (name, mutate) in mutations {
      let mut changed = inputs.clone();
      mutate(&mut changed);
      assert_ne!(
        digest(root.path(), changed).as_deref(),
        Some(baseline.as_str()),
        "{name} did not change or invalidate the result"
      );
    }
  }

  #[test]
  fn result_analysis_summarizes_every_instruction_domain_without_values() {
    let root = tempfile::tempdir().expect("source root");
    let analysis = analyze_result(root.path(), complete_inputs(), None);
    assert_eq!(
      analysis.instructions,
      Some(BuildScriptInstructionSummary {
        total: 9,
        rerun_declarations: 2,
        linked_libraries: 1,
        linked_paths: 1,
        link_arguments: 1,
        cfgs: 1,
        check_cfgs: 0,
        rustc_environment: 1,
        metadata: 1,
        warnings: 1,
        errors: 0,
      })
    );
    let encoded = serde_json::to_string(&analysis).expect("serialize result summary");
    for value in [
      "wrapper.h",
      "static=wrapper",
      "WRAPPER_VERSION=1",
      "using bundled wrapper",
    ] {
      assert!(!encoded.contains(value), "persisted raw build-script value {value:?}");
    }
  }

  #[test]
  fn result_analysis_matches_legacy_metadata_and_rustc_flags_semantics() {
    let root = tempfile::tempdir().expect("source root");
    let mut inputs = complete_inputs();
    inputs.instruction_stream.as_mut().unwrap().extend([
      "cargo:include=output:include".to_string(),
      "cargo:rustc-flags=-L legacy -llegacy".to_string(),
    ]);
    let analysis = analyze_result(root.path(), inputs, None);
    assert!(
      analysis.digest.is_some(),
      "legacy Cargo instructions should remain valid"
    );
    let summary = analysis.instructions.expect("instruction summary");
    assert_eq!(summary.total, 11);
    assert_eq!(summary.metadata, 2);
    assert_eq!(summary.linked_paths, 2);
    assert_eq!(summary.linked_libraries, 2);

    let mut invalid = complete_inputs();
    invalid
      .instruction_stream
      .as_mut()
      .unwrap()
      .push("cargo::future-unknown=value".to_string());
    let analysis = analyze_result(root.path(), invalid, None);
    assert!(analysis.digest.is_none());
    assert!(analysis.reasons.contains("build_script_instruction_stream_invalid"));
  }

  #[test]
  fn result_digest_preserves_instruction_order() {
    let root = tempfile::tempdir().expect("source root");
    let inputs = complete_inputs();
    let baseline = digest(root.path(), inputs.clone());
    let mut reordered = inputs;
    reordered.instruction_stream.as_mut().unwrap().swap(2, 3);
    assert_ne!(digest(root.path(), reordered), baseline);
  }

  #[test]
  fn result_digest_is_checkout_root_independent() {
    let left = tempfile::tempdir().expect("left root");
    let right = tempfile::tempdir().expect("right root");
    assert_eq!(
      digest(left.path(), complete_inputs()),
      digest(right.path(), complete_inputs())
    );
  }

  #[test]
  fn result_digest_requires_every_observation_domain() {
    type Missing = Box<dyn Fn(&mut BuildScriptResultInputs)>;

    let root = tempfile::tempdir().expect("source root");
    let cases: Vec<(&str, &str, Missing)> = vec![
      (
        "instructions",
        "build_script_instruction_stream_unavailable",
        Box::new(|inputs| inputs.instruction_stream = None),
      ),
      (
        "environment",
        "build_script_environment_reads_unavailable",
        Box::new(|inputs| inputs.environment_reads = None),
      ),
      (
        "outputs",
        "build_script_generated_output_tree_unavailable",
        Box::new(|inputs| inputs.generated_outputs = None),
      ),
      (
        "execution",
        "build_script_execution_observations_unavailable",
        Box::new(|inputs| inputs.execution = None),
      ),
    ];
    for (name, reason, remove) in cases {
      let mut inputs = complete_inputs();
      remove(&mut inputs);
      let analysis = analyze_result(root.path(), inputs, None);
      assert!(analysis.digest.is_none(), "{name} absence issued a digest");
      assert!(analysis.reasons.contains(reason), "{name} did not report {reason}");
    }
  }

  #[test]
  fn cargo_summary_is_diagnostic_only_and_redaction_safe() {
    let root = tempfile::tempdir().expect("source root");
    let incomplete = BuildScriptResultInputs {
      instruction_stream: None,
      environment_reads: None,
      generated_outputs: None,
      execution: None,
      secret_capabilities: BTreeSet::new(),
      limitations: BTreeSet::from(["cargo_build_script_execution_freshness_unavailable".to_string()]),
    };
    let summary = BuildScriptCargoOutputSummary {
      linked_libraries: 1,
      linked_paths: 2,
      cfgs: 3,
      rustc_environment: 4,
      output_directory_reported: true,
    };
    let analysis = analyze_result(root.path(), incomplete, Some(summary.clone()));
    assert!(analysis.digest.is_none());
    assert_eq!(analysis.cargo_output, Some(summary));

    let baseline = analyze_result(root.path(), complete_inputs(), None).digest;
    let with_summary = analyze_result(
      root.path(),
      complete_inputs(),
      Some(BuildScriptCargoOutputSummary {
        linked_libraries: 99,
        linked_paths: 99,
        cfgs: 99,
        rustc_environment: 99,
        output_directory_reported: false,
      }),
    )
    .digest;
    assert_eq!(with_summary, baseline, "Cargo's replayable subset gained authority");
  }

  #[test]
  fn result_analysis_never_serializes_secret_values() {
    let root = tempfile::tempdir().expect("source root");
    let mut inputs = complete_inputs();
    inputs.instruction_stream.as_mut().unwrap()[6] =
      "cargo::rustc-env=REGISTRY_TOKEN=never-persist-this-value".to_string();
    inputs
      .secret_capabilities
      .insert("BROKEN=second-never-persist-this-value".to_string());
    let analysis = analyze_result(root.path(), inputs, None);
    assert!(analysis.digest.is_none());
    assert!(analysis.reasons.contains("secret_build_script_result"));
    assert!(analysis.reasons.contains("build_script_secret_capability_invalid"));
    assert_eq!(
      analysis.secret_capabilities,
      BTreeSet::from(["REGISTRY_TOKEN".to_string()])
    );
    let encoded = serde_json::to_string(&analysis).expect("serialize analysis");
    assert!(!encoded.contains("never-persist-this-value"));
    assert!(!encoded.contains("second-never-persist-this-value"));
    assert!(encoded.contains("REGISTRY_TOKEN"));
  }

  #[test]
  fn result_digest_rejects_physical_checkout_paths() {
    let root = tempfile::tempdir().expect("source root");
    let mut inputs = complete_inputs();
    inputs
      .instruction_stream
      .as_mut()
      .unwrap()
      .push(format!("cargo::warning={}", root.path().display()));
    let analysis = analyze_result(root.path(), inputs, None);
    assert!(analysis.digest.is_none());
    assert!(
      analysis
        .reasons
        .contains("physical_checkout_path_in_build_script_result")
    );
  }

  #[test]
  fn result_digest_allows_internal_symlinks_but_rejects_escape() {
    let root = tempfile::tempdir().expect("source root");
    let mut safe = complete_inputs();
    safe.generated_outputs = Some(
      SourceTree::new(vec![
        SourceTreeEntry::symlink(Path::new("nested/link"), Path::new("../generated")).expect("internal symlink"),
      ])
      .expect("safe output tree"),
    );
    assert!(digest(root.path(), safe).is_some());

    let mut escaped = complete_inputs();
    escaped.generated_outputs = Some(
      SourceTree::new(vec![
        SourceTreeEntry::symlink(Path::new("link"), Path::new("../outside")).expect("escaping symlink"),
      ])
      .expect("canonical output tree"),
    );
    let analysis = analyze_result(root.path(), escaped, None);
    assert!(analysis.digest.is_none());
    assert!(analysis.reasons.contains("build_script_output_symlink_escape"));
  }
}
