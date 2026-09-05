//! Positive host and role qualification for the local compiler runtime.

use std::ffi::OsStr;
use std::path::Path;

use serde::Serialize;

use crate::error::RailResult;
use crate::source::ContentDigest;

const LOCAL_COMPILER_SET_VERSION: u32 = 1;
const LOCAL_COMPILER_SET_IDENTITY_PREFIX: &str = "local-compiler-set-v1-sha256-";

/// Every pre-CLI compiler role implemented by the local runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalCompilerRole {
    LinkAdapter,
    DirectCache,
    MarkedCache,
    RustcObservation,
    RustdocObservation,
    DoctestBuilder,
    DoctestRunner,
}

impl LocalCompilerRole {
    pub(crate) const ALL: [Self; 7] = [
        Self::LinkAdapter,
        Self::DirectCache,
        Self::MarkedCache,
        Self::RustcObservation,
        Self::RustdocObservation,
        Self::DoctestBuilder,
        Self::DoctestRunner,
    ];

    const fn reuse_authority(self) -> LocalReuseAuthority {
        match self {
            Self::DirectCache | Self::MarkedCache => LocalReuseAuthority::ExactNativeEligible,
            Self::LinkAdapter => LocalReuseAuthority::NativeActionSubprocess,
            Self::RustcObservation | Self::RustdocObservation | Self::DoctestBuilder | Self::DoctestRunner => {
                LocalReuseAuthority::EvidenceOnly
            }
        }
    }
}

/// The maximum authority a role may exercise after its exact inputs validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalReuseAuthority {
    ExactNativeEligible,
    NativeActionSubprocess,
    EvidenceOnly,
}

#[derive(Serialize)]
struct LocalCompilerRoleContract {
    role: LocalCompilerRole,
    reuse: LocalReuseAuthority,
}

/// Return the stable reason that disables all compiler-result reuse on a host.
pub(crate) fn unqualified_host_reason() -> Option<&'static str> {
    unqualified_host_reason_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub(crate) fn host_is_qualified() -> bool {
    unqualified_host_reason().is_none()
}

pub(crate) fn unqualified_host_reason_for(host_os: &str, host_arch: &str) -> Option<&'static str> {
    match (host_os, host_arch) {
        ("linux", "x86_64" | "aarch64" | "riscv64" | "s390x" | "powerpc64")
        | ("macos", "x86_64" | "aarch64")
        | ("windows", "x86_64" | "aarch64") => None,
        ("linux" | "macos" | "windows", _) => Some("native_cache_hardware_qualification_unavailable"),
        _ => Some("native_cache_platform_qualification_unavailable"),
    }
}

/// Bind the exact declared role set and positive host qualification into recovery authority.
pub(crate) fn local_compiler_set_identity() -> RailResult<String> {
    local_compiler_set_identity_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn local_compiler_set_identity_for(host_os: &str, host_arch: &str) -> RailResult<String> {
    let roles = LocalCompilerRole::ALL.map(|role| LocalCompilerRoleContract {
        role,
        reuse: role.reuse_authority(),
    });
    let bytes = serde_json::to_vec(&(
        LOCAL_COMPILER_SET_VERSION,
        host_os,
        host_arch,
        unqualified_host_reason_for(host_os, host_arch),
        roles,
    ))?;
    Ok(format!(
        "{LOCAL_COMPILER_SET_IDENTITY_PREFIX}{}",
        ContentDigest::sha256(&bytes)
    ))
}

/// Reject compiler programs outside the exact native-result role before cache authority loads.
pub(crate) fn native_reuse_program_bypass_reason(program: &OsStr) -> Option<&'static str> {
    let Some(name) = Path::new(program).file_stem().and_then(OsStr::to_str) else {
        return Some("alternate_compiler_program_identity_unavailable");
    };
    if name.eq_ignore_ascii_case("clippy-driver") {
        return Some("clippy_diagnostic_result_authority_unavailable");
    }
    if name.eq_ignore_ascii_case("rustdoc") {
        return Some("rustdoc_output_tree_observation_unavailable");
    }
    if name.eq_ignore_ascii_case("rustc") || name.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("rustc")) {
        return None;
    }
    Some("alternate_compiler_program_identity_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_qualification_is_positive_and_closed() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("linux", "riscv64"),
            ("linux", "s390x"),
            ("linux", "powerpc64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            assert_eq!(unqualified_host_reason_for(os, arch), None, "{os}/{arch}");
        }
        for (os, arch, reason) in [
            ("linux", "x86", "native_cache_hardware_qualification_unavailable"),
            ("macos", "riscv64", "native_cache_hardware_qualification_unavailable"),
            ("macos", "powerpc64", "native_cache_hardware_qualification_unavailable"),
            ("windows", "x86", "native_cache_hardware_qualification_unavailable"),
            ("freebsd", "x86_64", "native_cache_platform_qualification_unavailable"),
        ] {
            assert_eq!(unqualified_host_reason_for(os, arch), Some(reason), "{os}/{arch}");
        }
    }

    #[test]
    fn declared_roles_have_one_bounded_reuse_authority() {
        let roles = LocalCompilerRole::ALL;
        assert!(roles.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            roles.map(LocalCompilerRole::reuse_authority),
            [
                LocalReuseAuthority::NativeActionSubprocess,
                LocalReuseAuthority::ExactNativeEligible,
                LocalReuseAuthority::ExactNativeEligible,
                LocalReuseAuthority::EvidenceOnly,
                LocalReuseAuthority::EvidenceOnly,
                LocalReuseAuthority::EvidenceOnly,
                LocalReuseAuthority::EvidenceOnly,
            ]
        );
    }

    #[test]
    fn compiler_set_identity_binds_host_qualification_and_role_contract() {
        let qualified = local_compiler_set_identity_for("linux", "x86_64").expect("qualified identity");
        let architecture = local_compiler_set_identity_for("linux", "aarch64").expect("architecture identity");
        let ibm_z = local_compiler_set_identity_for("linux", "s390x").expect("IBM Z identity");
        assert!(qualified.starts_with(LOCAL_COMPILER_SET_IDENTITY_PREFIX));
        assert_ne!(qualified, architecture);
        assert_ne!(qualified, ibm_z);
    }

    #[test]
    fn native_reuse_accepts_only_the_declared_rustc_family() {
        for program in ["rustc", "rustc.exe", "rustc-real", "rustc-clif"] {
            assert_eq!(
                native_reuse_program_bypass_reason(OsStr::new(program)),
                None,
                "{program}"
            );
        }
        assert_eq!(
            native_reuse_program_bypass_reason(OsStr::new("clippy-driver")),
            Some("clippy_diagnostic_result_authority_unavailable")
        );
        assert_eq!(
            native_reuse_program_bypass_reason(OsStr::new("rustdoc")),
            Some("rustdoc_output_tree_observation_unavailable")
        );
        assert_eq!(
            native_reuse_program_bypass_reason(OsStr::new("alternate-rust-compiler")),
            Some("alternate_compiler_program_identity_unavailable")
        );
    }
}
