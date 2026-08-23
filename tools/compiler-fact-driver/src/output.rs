//! Atomic fragment publication and Cargo diagnostic announcement.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde_json::json;
use sha2::{Digest as _, Sha256};

use crate::fact_protocol::{
    COMPILER_FACT_ANNOUNCEMENT_CODE, COMPILER_FACT_ANNOUNCEMENT_PREFIX, COMPILER_FACT_PROTOCOL_VERSION,
    CompilerFactAnnouncement, CompilerFactFragment, CompilerFactInvocation, CompilerFactObject,
    FRAGMENT_OBJECT_IDENTITY_PREFIX,
};

pub(crate) fn publish(invocation: &CompilerFactInvocation, object: CompilerFactObject) -> Result<(), String> {
    let object_bytes =
        serde_json::to_vec(&object).map_err(|error| format!("serialize compiler fact object: {error}"))?;
    let object_identity = format!("{FRAGMENT_OBJECT_IDENTITY_PREFIX}{}", hex_digest(&object_bytes));
    let fragment = CompilerFactFragment {
        version: COMPILER_FACT_PROTOCOL_VERSION,
        run_authority: invocation.run_authority.clone(),
        object,
    };
    let bytes = serde_json::to_vec(&fragment).map_err(|error| format!("serialize compiler fact fragment: {error}"))?;
    let content_digest = hex_digest(&bytes);
    let directory = Path::new(&invocation.observation_directory);
    let final_path = directory.join(format!("compiler-fact-fragment-sha256-{content_digest}.json"));
    let temporary_path = directory.join(format!(
        ".compiler-fact-fragment-{}-{content_digest}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut temporary = options
        .open(&temporary_path)
        .map_err(|error| format!("create compiler fact sidecar: {error}"))?;
    // Cargo joins this compiler before the parent reads the regenerable
    // content-addressed sidecar. Atomic publication is required; crash
    // durability is not.
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("write compiler fact sidecar: {error}"))?;
    drop(temporary);
    match fs::rename(&temporary_path, &final_path) {
        Ok(()) => {}
        Err(error) if final_path.is_file() => {
            let existing =
                fs::read(&final_path).map_err(|read| format!("read existing compiler fact sidecar: {read}"))?;
            if existing != bytes {
                return Err("content-addressed compiler fact sidecar has conflicting bytes".to_string());
            }
            fs::remove_file(&temporary_path)
                .map_err(|remove| format!("remove duplicate compiler fact sidecar: {remove}"))?;
            let _ = error;
        }
        Err(error) => return Err(format!("publish compiler fact sidecar: {error}")),
    }

    let announcement = CompilerFactAnnouncement {
        version: COMPILER_FACT_PROTOCOL_VERSION,
        run_authority: invocation.run_authority.clone(),
        producer_authority: invocation.producer_authority.clone(),
        unit_identity: invocation.unit.identity.clone(),
        object_identity,
        content_digest: format!("sha256:{content_digest}"),
        bytes: bytes.len() as u64,
    };
    let payload = serde_json::to_string(&announcement)
        .map_err(|error| format!("serialize compiler fact announcement: {error}"))?;
    let diagnostic = json!({
      "$message_type": "diagnostic",
      "message": format!("{COMPILER_FACT_ANNOUNCEMENT_PREFIX}{payload}"),
      "code": { "code": COMPILER_FACT_ANNOUNCEMENT_CODE, "explanation": null },
      "level": "note",
      "spans": [],
      "children": [],
      "rendered": null
    });
    eprintln!("{diagnostic}");
    Ok(())
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
