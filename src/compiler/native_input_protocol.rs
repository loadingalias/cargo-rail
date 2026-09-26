//! Exact-invocation compiler input observations, separate from compiler facts.

#![allow(
    dead_code,
    reason = "the shared wire model has disjoint producers and consumers in the main crate and isolated companion"
)]

use std::path::{Component, Path};

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};

pub(crate) const NATIVE_INPUT_PROTOCOL_VERSION: u32 = 4;
pub(crate) const NATIVE_INPUT_PROTOCOL_VERSION_ARGUMENT: &str = "--cargo-rail-native-input-protocol-version";
pub(crate) const NATIVE_INPUT_INVOCATION_ENV: &str = "CARGO_RAIL_NATIVE_INPUT_INVOCATION";
pub(crate) const NATIVE_INPUT_INVOCATION_ARGUMENT: &str = "--cargo-rail-native-input-invocation";
pub(crate) const MAX_NATIVE_INPUT_INVOCATION_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_NATIVE_INPUT_OBSERVATION_BYTES: u64 = 4 * 1024 * 1024;

/// Where the driver records the compiler's crate selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeInputPhase {
    /// The driver performs the complete compilation and records its inputs after analysis.
    Compilation,
    /// The driver records the crate selection when rustc freezes crate loading, then stops
    /// before writing any output. Another compiler, such as Clippy, produces the outputs.
    Resolution,
}

/// One-shot capability for the actual compiler arguments and result destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeInputInvocation {
    pub(crate) version: u32,
    pub(crate) phase: NativeInputPhase,
    pub(crate) nonce: String,
    pub(crate) action_identity: String,
    pub(crate) invocation_digest: String,
    pub(crate) result_path: String,
    pub(crate) source_working_directory: Option<String>,
}

impl NativeInputInvocation {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_NATIVE_INPUT_INVOCATION_BYTES {
            return Err("native input invocation exceeds its byte bound".into());
        }
        let invocation: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        invocation.validate()?;
        if serde_json::to_vec(&invocation).map_err(|error| error.to_string())? != bytes {
            return Err("native input invocation is not canonical JSON".into());
        }
        Ok(invocation)
    }

    pub(crate) fn identity(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_NATIVE_INPUT_INVOCATION_BYTES {
            return Err("native input invocation exceeds its byte bound".into());
        }
        Ok(hex_digest(&bytes))
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != NATIVE_INPUT_PROTOCOL_VERSION
            || !is_digest(&self.nonce)
            || !is_digest(&self.invocation_digest)
            || self.action_identity.is_empty()
            || self.action_identity.len() > 512
            || self.action_identity.chars().any(char::is_control)
            || self
                .source_working_directory
                .as_deref()
                .is_some_and(|path| !absolute_input_path(path))
            || !absolute_input_path(&self.result_path)
            || Path::new(&self.result_path).file_name().is_none()
        {
            return Err("native input invocation has incompatible authority".into());
        }
        Ok(())
    }
}

/// Files rustc selected for one external crate, including transitive crates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCrateSource {
    pub(crate) name: String,
    pub(crate) files: Vec<String>,
}

/// Compiler-owned filename query, including the effective target's conventions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCratePattern {
    pub(crate) prefix: String,
    pub(crate) suffix: String,
}

impl NativeCratePattern {
    pub(crate) fn matches(&self, name: &str) -> bool {
        name.len() >= self.prefix.len() + self.suffix.len()
            && name.starts_with(&self.prefix)
            && name.ends_with(&self.suffix)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.prefix.is_empty()
            || self.prefix.len() > 1024
            || self.suffix.len() > 128
            || self
                .prefix
                .chars()
                .chain(self.suffix.chars())
                .any(|character| matches!(character, '\0' | '/' | '\\'))
        {
            return Err("native crate filename query is invalid".into());
        }
        Ok(())
    }
}

/// The retained filename index actually searched by rustc, not a later scan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCrateSearch {
    pub(crate) directory: String,
    pub(crate) patterns: Vec<NativeCratePattern>,
    pub(crate) files: Vec<String>,
}

/// Whether this compilation can consume assembler inputs outside dep-info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeAssemblyObservation {
    NoCodegen,
    Absent,
    Present,
    ImportedLtoUnobserved,
}

/// Backend work observed after code generation has completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NativeCodegenObservation {
    NotRun,
    Llvm,
    Cranelift { separate_assembly: bool },
    Unsupported,
}

impl NativeCodegenObservation {
    pub(crate) fn certifies_no_external_tools(self) -> bool {
        matches!(
            self,
            Self::Llvm
                | Self::Cranelift {
                    separate_assembly: false
                }
        )
    }
}

pub(crate) const MAX_MACRO_PATH_READS: usize = 4096;
pub(crate) const MAX_MACRO_PATH_BYTES: usize = 4096;
pub(crate) const MAX_MACRO_ENVIRONMENT_READS: usize = 1024;
pub(crate) const MAX_MACRO_ENVIRONMENT_NAME_BYTES: usize = 1024;
pub(crate) const MAX_MACRO_SPAWNS: usize = 64;
pub(crate) const MAX_MACRO_SPAWN_ARGUMENTS: usize = 256;
pub(crate) const MAX_MACRO_UNOBSERVABLE_IMPORTS: usize = 64;
pub(crate) const MAX_MACRO_IMPORT_NAME_BYTES: usize = 256;

/// How a procedural macro reached one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeMacroPathAccess {
    /// Existence, type, or link target, such as `stat`, `access`, or `readlink`.
    Entry,
    /// Contents opened for reading.
    Contents,
    /// Directory entries listed.
    Listing,
    /// Canonical resolution, which follows the link state of every ancestor.
    Resolution,
}

/// One path a procedural macro read, spelled as the absolute path the operating system resolved.
/// The spelling can contain `..` components, because resolving them lexically ignores symbolic links.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMacroPathRead {
    pub(crate) path: String,
    pub(crate) access: NativeMacroPathAccess,
}

/// A process a procedural macro started with the inherited environment and working directory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMacroSpawn {
    pub(crate) program: String,
    pub(crate) arguments: Vec<String>,
}

/// An effect of procedural-macro execution that no observation can bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeMacroUnobservable {
    /// The host cannot observe a loaded macro, for example a kernel without seccomp user notification.
    ObservationUnavailable,
    /// The macro imports a function outside the classified set.
    ImportUnclassified,
    /// The macro loads a shared library other than the system C library.
    DynamicDependency,
    /// The macro runs load-time initializers that execute before observation.
    Initializer,
    /// The macro issues system calls without the C library, and the host cannot observe them.
    RawSystemCall,
    /// The macro forks, executes, or starts a process with a changed environment or directory.
    ProcessControl,
    Network,
    FileWrite,
    EnvironmentWrite,
    /// The macro read the whole environment instead of named variables.
    EnvironmentEnumeration,
    DynamicLoad,
    ExecutableMemory,
    WorkingDirectory,
    /// The macro read host identity, such as the host name or user database.
    HostState,
    /// A path is not UTF-8 or names an open descriptor that cannot be resolved.
    PathUnavailable,
    ObservationLimit,
}

/// Everything loaded procedural macros read during one compiler run.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMacroObservation {
    pub(crate) paths: Vec<NativeMacroPathRead>,
    pub(crate) environment: Vec<String>,
    pub(crate) spawns: Vec<NativeMacroSpawn>,
    pub(crate) unobservable: Vec<NativeMacroUnobservable>,
    /// Imported symbols that made the observation incomplete, so an `import_unclassified` bypass names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) unobservable_imports: Vec<String>,
}

impl NativeMacroObservation {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let valid = self.paths.len() <= MAX_MACRO_PATH_READS
            && self.paths.windows(2).all(|pair| pair[0] < pair[1])
            && self.paths.iter().all(|read| {
                read.path.len() <= MAX_MACRO_PATH_BYTES
                    && !read.path.contains('\0')
                    && Path::new(&read.path).is_absolute()
            })
            && self.environment.len() <= MAX_MACRO_ENVIRONMENT_READS
            && self.environment.windows(2).all(|pair| pair[0] < pair[1])
            && self.environment.iter().all(|name| {
                !name.is_empty() && name.len() <= MAX_MACRO_ENVIRONMENT_NAME_BYTES && !name.contains(['\0', '='])
            })
            && self.spawns.len() <= MAX_MACRO_SPAWNS
            && self.spawns.windows(2).all(|pair| pair[0] < pair[1])
            && self.spawns.iter().all(|spawn| {
                !spawn.program.is_empty()
                    && spawn.program.len() <= MAX_MACRO_PATH_BYTES
                    && !spawn.program.contains('\0')
                    && spawn.arguments.len() <= MAX_MACRO_SPAWN_ARGUMENTS
                    && spawn
                        .arguments
                        .iter()
                        .all(|argument| argument.len() <= MAX_MACRO_PATH_BYTES && !argument.contains('\0'))
            })
            && self.unobservable.windows(2).all(|pair| pair[0] < pair[1])
            && self.unobservable_imports.len() <= MAX_MACRO_UNOBSERVABLE_IMPORTS
            && self.unobservable_imports.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .unobservable_imports
                .iter()
                .all(|name| !name.is_empty() && name.len() <= MAX_MACRO_IMPORT_NAME_BYTES && !name.contains('\0'))
            && (self.unobservable_imports.is_empty()
                || self.unobservable.contains(&NativeMacroUnobservable::ImportUnclassified));
        if valid {
            Ok(())
        } else {
            Err("procedural-macro observation has incompatible authority".into())
        }
    }
}

/// Complete selected crate sources and assembly coverage from one compiler run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeInputObservation {
    pub(crate) version: u32,
    pub(crate) request_identity: String,
    pub(crate) crates: Vec<NativeCrateSource>,
    pub(crate) searches: Vec<NativeCrateSearch>,
    pub(crate) assembly: NativeAssemblyObservation,
    pub(crate) codegen: NativeCodegenObservation,
    /// `None` when the compiler loaded no procedural macro.
    pub(crate) macros: Option<NativeMacroObservation>,
}

impl NativeInputObservation {
    pub(crate) fn decode(bytes: &[u8], invocation: &NativeInputInvocation) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_NATIVE_INPUT_OBSERVATION_BYTES {
            return Err("native input observation exceeds its byte bound".into());
        }
        let observation: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if observation.encode(invocation)? != bytes {
            return Err("native input observation is not canonical JSON".into());
        }
        Ok(observation)
    }

    pub(crate) fn encode(&self, invocation: &NativeInputInvocation) -> Result<Vec<u8>, String> {
        if self.version != NATIVE_INPUT_PROTOCOL_VERSION
            || self.request_identity != invocation.identity()?
            || self.crates.len() > 4096
            || self.crates.windows(2).any(|pair| pair[0] >= pair[1])
            || self.crates.iter().any(|source| {
                source.name.is_empty()
                    || source.name.len() > 256
                    || !source
                        .name
                        .chars()
                        .all(|character| character == '_' || character.is_alphanumeric())
                    || source.files.is_empty()
                    || source.files.len() > 4
                    || source.files.windows(2).any(|pair| pair[0] >= pair[1])
                    || source.files.iter().any(|path| !absolute_input_path(path))
            })
            || self.searches.len() > 512
            || self
                .searches
                .windows(2)
                .any(|pair| pair[0].directory >= pair[1].directory)
            || self.searches.iter().any(|search| {
                !absolute_input_path(&search.directory)
                    || search.patterns.is_empty()
                    || search.patterns.len() > 4096 * 10
                    || search.patterns.windows(2).any(|pair| pair[0] >= pair[1])
                    || search.patterns.iter().any(|pattern| pattern.validate().is_err())
                    || search.files.len() > 100_000
                    || search.files.windows(2).any(|pair| pair[0] >= pair[1])
                    || search.files.iter().any(|name| {
                        name.is_empty()
                            || name.chars().any(|character| matches!(character, '\0' | '/' | '\\'))
                            || !search.patterns.iter().any(|pattern| pattern.matches(name))
                    })
            })
        {
            return Err("native input observation has incompatible authority".into());
        }
        if let Some(macros) = &self.macros {
            macros.validate()?;
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_NATIVE_INPUT_OBSERVATION_BYTES {
            return Err("native input observation exceeds its byte bound".into());
        }
        Ok(bytes)
    }
}

/// Bind the unmodified rustc arguments (excluding `argv[0]`) and working directory.
pub(crate) fn native_invocation_digest(arguments: &[String], current_directory: &Path) -> Result<String, String> {
    let current_directory = current_directory
        .to_str()
        .filter(|path| absolute_input_path(path))
        .ok_or_else(|| "native input working directory is not an absolute UTF-8 path".to_string())?;
    let bytes = serde_json::to_vec(&(NATIVE_INPUT_PROTOCOL_VERSION, arguments, current_directory))
        .map_err(|error| error.to_string())?;
    Ok(hex_digest(&bytes))
}

fn absolute_input_path(path: &str) -> bool {
    !path.contains('\0')
        && Path::new(path).is_absolute()
        && !Path::new(path)
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation() -> NativeInputInvocation {
        NativeInputInvocation {
            version: NATIVE_INPUT_PROTOCOL_VERSION,
            phase: NativeInputPhase::Compilation,
            source_working_directory: None,
            nonce: "1".repeat(64),
            action_identity: "native-action".into(),
            invocation_digest: "2".repeat(64),
            result_path: std::env::temp_dir().join("native-result.json").to_str().unwrap().into(),
        }
    }

    #[test]
    fn invocation_rejects_noncanonical_and_unknown_authority() {
        let invocation = invocation();
        let mut bytes = serde_json::to_vec(&invocation).unwrap();
        bytes.push(b'\n');
        assert_eq!(
            NativeInputInvocation::decode(&bytes).unwrap_err(),
            "native input invocation is not canonical JSON"
        );
        let mut value = serde_json::to_value(&invocation).unwrap();
        value["extra"] = true.into();
        assert!(
            NativeInputInvocation::decode(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .contains("unknown field")
        );
    }

    #[test]
    fn observation_rejects_another_request_and_duplicate_sources() {
        let mut invocation = invocation();
        let observation = NativeInputObservation {
            version: NATIVE_INPUT_PROTOCOL_VERSION,
            request_identity: invocation.identity().unwrap(),
            crates: vec![],
            searches: vec![],
            assembly: NativeAssemblyObservation::NoCodegen,
            codegen: NativeCodegenObservation::NotRun,
            macros: None,
        };
        let bytes = observation.encode(&invocation).unwrap();
        invocation.nonce = "3".repeat(64);
        assert_eq!(
            NativeInputObservation::decode(&bytes, &invocation).unwrap_err(),
            "native input observation has incompatible authority"
        );
        let source = NativeCrateSource {
            name: "dependency".into(),
            files: vec![invocation.result_path.clone()],
        };
        let duplicated = NativeInputObservation {
            request_identity: invocation.identity().unwrap(),
            crates: vec![source.clone(), source],
            ..observation
        };
        assert_eq!(
            duplicated.encode(&invocation).unwrap_err(),
            "native input observation has incompatible authority"
        );
    }

    #[test]
    fn invocation_identity_binds_the_observation_phase() {
        let compilation = invocation();
        let resolution = NativeInputInvocation {
            phase: NativeInputPhase::Resolution,
            ..compilation.clone()
        };
        assert_ne!(compilation.identity().unwrap(), resolution.identity().unwrap());
        let encoded = serde_json::to_vec(&resolution).unwrap();
        assert_eq!(NativeInputInvocation::decode(&encoded).unwrap(), resolution);
    }

    #[test]
    fn invocation_digest_binds_argument_boundaries_and_directory() {
        let current = std::env::temp_dir();
        let digest = native_invocation_digest(&["--cfg".into(), "a b".into()], &current).unwrap();
        assert_ne!(
            digest,
            native_invocation_digest(&["--cfg a".into(), "b".into()], &current).unwrap()
        );
        assert_ne!(
            digest,
            native_invocation_digest(&["--cfg".into(), "a b".into()], &current.join("other")).unwrap()
        );
    }
}
