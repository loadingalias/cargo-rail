//! Exact-invocation compiler input observations, separate from compiler facts.

#![allow(
    dead_code,
    reason = "the shared wire model has disjoint producers and consumers in the main crate and isolated companion"
)]

use std::path::{Component, Path};

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};

pub(crate) const NATIVE_INPUT_PROTOCOL_VERSION: u32 = 2;
pub(crate) const NATIVE_INPUT_PROTOCOL_VERSION_ARGUMENT: &str = "--cargo-rail-native-input-protocol-version";
pub(crate) const NATIVE_INPUT_INVOCATION_ENV: &str = "CARGO_RAIL_NATIVE_INPUT_INVOCATION";
pub(crate) const NATIVE_INPUT_INVOCATION_ARGUMENT: &str = "--cargo-rail-native-input-invocation";
pub(crate) const MAX_NATIVE_INPUT_INVOCATION_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_NATIVE_INPUT_OBSERVATION_BYTES: u64 = 4 * 1024 * 1024;

/// One-shot capability for the actual compiler arguments and result destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeInputInvocation {
    pub(crate) version: u32,
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
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_NATIVE_INPUT_OBSERVATION_BYTES {
            return Err("native input observation exceeds its byte bound".into());
        }
        Ok(bytes)
    }
}

/// Bind the unmodified rustc arguments (excluding argv[0]) and working directory.
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
