//! Root-independent shapes for exact compiler operations.

use std::path::Path;

use serde::Serialize;

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const COMPILER_OPERATION_VERSION: u32 = 3;
const COMPILER_OPERATION_ID_PREFIX: &str = "coverage-action-v3:sha256:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompilerOperationClass {
    CompilerRequest,
    ProcMacroProducer,
    BuildScript,
    Test,
    Binary,
    MixedCrateTypes,
    StaticLibrary,
    CDynamicLibrary,
    RustDynamicLibrary,
    RustLibrary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompilerCapability {
    PossibleProcMacroConsumer,
    NativeLinkConsumer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompilerDriver {
    Clippy,
    Other,
    Rustc,
    Rustdoc,
}

/// One typed, root-independent Rust compiler operation used by coverage evidence.
///
/// Exact cache authority remains in `native_cache`: this shape describes the work
/// without claiming that its inputs or effects are complete enough for reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CompilerOperation {
    action_class: CompilerOperationClass,
    capabilities: Vec<CompilerCapability>,
    cfg: Vec<String>,
    codegen: Vec<String>,
    crate_name: Option<String>,
    crate_types: Vec<String>,
    driver: CompilerDriver,
    edition: Option<String>,
    emit: Vec<String>,
    externs: Vec<String>,
    features: Vec<String>,
    native_libraries: Vec<String>,
    native_search_kinds: Vec<String>,
    package_hint: String,
    schema_version: u32,
    source_name: Option<String>,
    target: String,
    test: bool,
    unstable: Vec<String>,
}

impl CompilerOperation {
    /// Parse the exact compiler argv into the stable operation vocabulary.
    pub(crate) fn capture(program: &str, arguments: &[String]) -> RailResult<Self> {
        if program.is_empty() {
            return Err(RailError::message("compiler operation has no program"));
        }
        let driver = match Path::new(program)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or(program)
        {
            name if name.eq_ignore_ascii_case("rustc") => CompilerDriver::Rustc,
            name if name.eq_ignore_ascii_case("rustdoc") => CompilerDriver::Rustdoc,
            name if name.eq_ignore_ascii_case("clippy-driver") => CompilerDriver::Clippy,
            _ => CompilerDriver::Other,
        };
        let crate_name = long_option_values(arguments, "--crate-name").next().map(str::to_string);
        let mut crate_types = long_option_values(arguments, "--crate-type")
            .flat_map(|value| value.split(','))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        crate_types.sort_unstable();

        let mut emit = long_option_values(arguments, "--emit")
            .flat_map(|value| value.split(','))
            .filter(|value| !value.is_empty())
            .map(|value| value.split_once('=').map_or(value, |(name, _)| name).to_string())
            .collect::<Vec<_>>();
        emit.sort_unstable();

        let cfg_values = long_option_values(arguments, "--cfg").collect::<Vec<_>>();
        let mut features = cfg_values
            .iter()
            .filter_map(|value| {
                value
                    .strip_prefix("feature=\"")
                    .and_then(|value| value.strip_suffix('"'))
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        features.sort_unstable();
        let mut cfg = cfg_values.into_iter().map(cfg_shape).collect::<Vec<_>>();
        cfg.sort_unstable();

        let mut codegen = short_option_values(arguments, "-C")
            .filter(|value| {
                !matches!(
                    value.split_once('=').map_or(*value, |(name, _)| name),
                    "metadata" | "extra-filename" | "incremental"
                )
            })
            .map(codegen_shape)
            .collect::<Vec<_>>();
        codegen.sort_unstable();

        let mut unstable = short_option_values(arguments, "-Z")
            .map(unstable_shape)
            .collect::<Vec<_>>();
        unstable.sort_unstable();

        let extern_values = long_option_values(arguments, "--extern").collect::<Vec<_>>();
        let mut externs = extern_values
            .iter()
            .map(|value| value.split_once('=').map_or(*value, |(name, _)| name))
            .map(|name| name.strip_prefix("priv:").unwrap_or(name).to_string())
            .collect::<Vec<_>>();
        externs.sort_unstable();

        let mut native_libraries = short_option_values(arguments, "-l")
            .map(str::to_string)
            .collect::<Vec<_>>();
        native_libraries.sort_unstable();
        let mut native_search_kinds = short_option_values(arguments, "-L")
            .filter_map(|value| {
                let kind = value.split_once('=').map_or("all", |(kind, _)| kind);
                (kind != "dependency").then(|| kind.to_string())
            })
            .collect::<Vec<_>>();
        native_search_kinds.sort_unstable();

        let mut capabilities = Vec::new();
        if extern_values
            .iter()
            .filter_map(|value| value.split_once('=').map(|(_, path)| path))
            .any(|path| matches!(portable_extension(path), Some("dll" | "dylib" | "so")))
        {
            capabilities.push(CompilerCapability::PossibleProcMacroConsumer);
        }
        if !native_libraries.is_empty() || !native_search_kinds.is_empty() {
            capabilities.push(CompilerCapability::NativeLinkConsumer);
        }

        let source = source_argument(arguments);
        let action_class = if crate_name.is_none() {
            CompilerOperationClass::CompilerRequest
        } else if crate_types.iter().any(|crate_type| crate_type == "proc-macro") {
            CompilerOperationClass::ProcMacroProducer
        } else if crate_types.iter().any(|crate_type| crate_type == "bin")
            && crate_name.as_deref() == Some("build_script_build")
        {
            CompilerOperationClass::BuildScript
        } else if arguments.iter().any(|argument| argument == "--test") {
            CompilerOperationClass::Test
        } else if crate_types.iter().any(|crate_type| crate_type == "bin") {
            CompilerOperationClass::Binary
        } else if crate_types.len() != 1 {
            CompilerOperationClass::MixedCrateTypes
        } else if crate_types.iter().any(|crate_type| crate_type == "staticlib") {
            CompilerOperationClass::StaticLibrary
        } else if crate_types.iter().any(|crate_type| crate_type == "cdylib") {
            CompilerOperationClass::CDynamicLibrary
        } else if crate_types.iter().any(|crate_type| crate_type == "dylib") {
            CompilerOperationClass::RustDynamicLibrary
        } else {
            CompilerOperationClass::RustLibrary
        };
        let source_name = source.map(|source| {
            if source == "-" {
                "stdin".to_string()
            } else {
                portable_basename(source).unwrap_or(source).to_string()
            }
        });
        let package_hint = package_hint(source, crate_name.as_deref());
        if package_hint.is_empty() {
            return Err(RailError::message("compiler operation has no package identity"));
        }
        Ok(Self {
            action_class,
            capabilities,
            cfg,
            codegen,
            crate_name,
            crate_types,
            driver,
            edition: long_option_values(arguments, "--edition").next().map(str::to_string),
            emit,
            externs,
            features,
            native_libraries,
            native_search_kinds,
            package_hint,
            schema_version: COMPILER_OPERATION_VERSION,
            source_name,
            target: target_shape(long_option_values(arguments, "--target").next().unwrap_or("host")),
            test: arguments.iter().any(|argument| argument == "--test"),
            unstable,
        })
    }

    /// Return the root-independent identity shared with benchmark comparison.
    pub(crate) fn identity(&self) -> RailResult<String> {
        // Fields are declared in lexical order, matching the canonical JSON key
        // order used by independent evidence readers.
        let encoded = serde_json::to_vec(self)?;
        Ok(format!(
            "{COMPILER_OPERATION_ID_PREFIX}{}",
            ContentDigest::sha256(&encoded)
        ))
    }
}

fn long_option_values<'a>(arguments: &'a [String], option: &'a str) -> impl Iterator<Item = &'a str> {
    let inline = format!("{option}=");
    arguments.iter().enumerate().filter_map(move |(index, argument)| {
        if argument == option {
            arguments.get(index + 1).map(String::as_str)
        } else {
            argument.strip_prefix(&inline)
        }
    })
}

fn short_option_values<'a>(arguments: &'a [String], option: &'a str) -> impl Iterator<Item = &'a str> {
    arguments.iter().enumerate().filter_map(move |(index, argument)| {
        if argument == option {
            arguments.get(index + 1).map(String::as_str)
        } else if argument.starts_with(option) {
            argument.strip_prefix(option).filter(|value| !value.is_empty())
        } else {
            None
        }
    })
}

fn source_argument(arguments: &[String]) -> Option<&str> {
    arguments
        .iter()
        .find(|argument| argument.as_str() == "-" || argument.ends_with(".rs"))
        .map(String::as_str)
}

fn cfg_shape(value: &str) -> String {
    let Some((name, configured)) = value.split_once('=') else {
        return value.to_string();
    };
    if configured_path(configured) {
        format!("{name}=<path>")
    } else {
        value.to_string()
    }
}

fn codegen_shape(value: &str) -> String {
    let (name, configured) = value
        .split_once('=')
        .map_or((value, None), |(name, value)| (name, Some(value)));
    match name {
        "linker" | "dlltool" => configured.map_or_else(|| name.to_string(), |program| tool_shape(name, program)),
        "link-arg" | "link-args" => format!("{name}=<opaque>"),
        _ if configured.is_some_and(configured_path) => format!("{name}=<path>"),
        _ => value.to_string(),
    }
}

fn unstable_shape(value: &str) -> String {
    let (name, configured) = value
        .split_once('=')
        .map_or((value, None), |(name, value)| (name, Some(value)));
    match (name, configured) {
        ("codegen-backend", Some(backend)) if configured_path(backend) => tool_shape(name, backend),
        (_, Some(configured)) if configured_path(configured) => format!("{name}=<path>"),
        _ => value.to_string(),
    }
}

fn tool_shape(option: &str, program: &str) -> String {
    portable_basename(program).filter(|name| !name.is_empty()).map_or_else(
        || format!("{option}=<external-tool>"),
        |name| format!("{option}={name}"),
    )
}

fn target_shape(value: &str) -> String {
    if value != "host" && (configured_path(value) || value.ends_with(".json")) {
        portable_basename(value).map_or_else(|| "custom-target".to_string(), |name| format!("custom-target:{name}"))
    } else {
        value.to_string()
    }
}

fn configured_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn package_hint(source: Option<&str>, crate_name: Option<&str>) -> String {
    let Some(source) = source else {
        return crate_name.unwrap_or("compiler-request").to_string();
    };
    let normalized = source.replace('\\', "/");
    if let Some(registry) = normalized.split_once("/registry/src/").map(|(_, path)| path) {
        let mut components = registry.split('/');
        if components.next().is_some()
            && let Some(package) = components.next().filter(|package| !package.is_empty())
        {
            return package.to_string();
        }
    }
    if let Some(crates) = normalized.split_once("/crates/").map(|(_, path)| path)
        && let Some(package) = crates.split('/').next().filter(|package| !package.is_empty())
    {
        return package.to_string();
    }
    if let Some(crates) = normalized.strip_prefix("crates/")
        && let Some(package) = crates.split('/').next().filter(|package| !package.is_empty())
    {
        return package.to_string();
    }
    let path = Path::new(&normalized);
    if path.file_name().and_then(std::ffi::OsStr::to_str) == Some("build.rs")
        && let Some(package) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
    {
        return package.to_string();
    }
    crate_name
        .or_else(|| path.file_name().and_then(std::ffi::OsStr::to_str))
        .unwrap_or("compiler-request")
        .to_string()
}

fn portable_basename(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|component| !component.is_empty())
}

fn portable_extension(path: &str) -> Option<&str> {
    portable_basename(path)?
        .rsplit_once('.')
        .map(|(_, extension)| extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_operation_is_root_independent_and_typed() {
        fn capture(root: &str) -> CompilerOperation {
            CompilerOperation::capture(
                "/toolchain/bin/rustc",
                &[
                    "--crate-name".to_string(),
                    "fixture_service".to_string(),
                    "--crate-type=lib".to_string(),
                    "--emit=dep-info,metadata,link".to_string(),
                    "--edition=2024".to_string(),
                    "--cfg".to_string(),
                    "feature=\"json\"".to_string(),
                    "--cfg".to_string(),
                    format!("fixture_root=\"{root}/generated\""),
                    "--extern".to_string(),
                    format!("fixture_macros={root}/target/libfixture_macros.dylib"),
                    "-L".to_string(),
                    format!("native={root}/target/native"),
                    "-lstatic=fixture".to_string(),
                    "-C".to_string(),
                    format!("linker={root}/tools/fixture-linker"),
                    format!("-Clink-arg=-T{root}/link/fixture.ld"),
                    format!("-Zcodegen-backend={root}/backends/fixture_backend.so"),
                    format!("--target={root}/targets/fixture.json"),
                    format!("{root}/crates/fixture-service/src/lib.rs"),
                ],
            )
            .expect("operation")
        }

        let first = capture("/first");
        let second = capture("/second");
        assert_eq!(first, second);
        assert_eq!(
            first.identity().expect("identity"),
            second.identity().expect("identity")
        );
        assert_eq!(
            first.identity().expect("canonical identity"),
            "coverage-action-v3:sha256:1beb6d5ae68b8c50a0a353eefb2db1ecc926ebc32d1c4f81b4b8829a2ee574f1"
        );
        let value = serde_json::to_value(first).expect("serialize operation");
        assert_eq!(value["package_hint"], "fixture-service");
        assert_eq!(value["action_class"], "rust_library");
        assert_eq!(value["driver"], "rustc");
        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["target"], "custom-target:fixture.json");
        assert_eq!(
            value["codegen"],
            serde_json::json!(["link-arg=<opaque>", "linker=fixture-linker"])
        );
        assert_eq!(
            value["unstable"],
            serde_json::json!(["codegen-backend=fixture_backend.so"])
        );
        assert_eq!(
            value["capabilities"],
            serde_json::json!(["possible_proc_macro_consumer", "native_link_consumer"])
        );
        assert_eq!(value["native_libraries"], serde_json::json!(["static=fixture"]));
    }

    #[test]
    fn compiler_operation_distinguishes_material_codegen_and_target_inputs() {
        let baseline = CompilerOperation::capture(
            "rustc",
            &[
                "--crate-name=fixture".to_string(),
                "--crate-type=lib".to_string(),
                "--emit=dep-info,metadata".to_string(),
                "src/lib.rs".to_string(),
            ],
        )
        .expect("baseline");
        let clippy = CompilerOperation::capture(
            "clippy-driver",
            &[
                "--crate-name=fixture".to_string(),
                "--crate-type=lib".to_string(),
                "--emit=dep-info,metadata".to_string(),
                "src/lib.rs".to_string(),
            ],
        )
        .expect("clippy operation");
        assert_ne!(baseline, clippy, "different compiler modes cannot share one operation");
        for arguments in [
            vec!["-Copt-level=3"],
            vec!["-Zcodegen-backend=cranelift"],
            vec!["--target=wasm32-wasip1"],
            vec!["--test"],
        ] {
            let mut changed = vec![
                "--crate-name=fixture".to_string(),
                "--crate-type=lib".to_string(),
                "--emit=dep-info,metadata".to_string(),
                "src/lib.rs".to_string(),
            ];
            changed.extend(arguments.into_iter().map(str::to_string));
            let changed = CompilerOperation::capture("rustc", &changed).expect("changed operation");
            assert_ne!(
                baseline.identity().expect("baseline identity"),
                changed.identity().expect("changed identity")
            );
        }
    }
}
