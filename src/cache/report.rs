//! Bounded measurements for one explicitly selected compiler-cache reporting interval.
//! These observations never participate in cache keys or restore authority.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};

pub(crate) const REPORT_ENV: &str = "CARGO_RAIL_CACHE_REPORT";
const MAX_BYTES: u64 = 32 * 1024;
const MAX_REASONS: usize = 128;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Measurements {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) bypasses: u64,
    pub(crate) failures: u64,
    pub(crate) local_bytes_read: u64,
    pub(crate) remote_bytes_read: u64,
    pub(crate) remote_bytes_written: u64,
    pub(crate) bypass_reasons: BTreeMap<String, u64>,
    pub(crate) failure_reasons: BTreeMap<String, u64>,
    pub(crate) incomplete: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recording {
    schema_version: u32,
    kind: String,
    finished: bool,
    measurements: Measurements,
}

pub(crate) fn start(path: &Path) -> RailResult<()> {
    if !path.is_absolute() {
        return Err(RailError::message("cache report path must be absolute"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.lock()?;
    validate_file(&file, path)?;
    write(
        &mut file,
        &Recording {
            schema_version: 1,
            kind: "cargo-rail-cache-recording".to_string(),
            finished: false,
            measurements: Measurements::default(),
        },
    )
}

pub(crate) fn finish(path: &Path) -> RailResult<Measurements> {
    let mut file = open(path)?;
    let mut recording = read(&mut file)?;
    recording.finished = true;
    recording.measurements.incomplete |= path.with_extension("incomplete").try_exists()?;
    validate_file(&file, path)?;
    write(&mut file, &recording)?;
    Ok(recording.measurements)
}

/// Ordinary compiler execution remains independent of diagnostic storage availability.
pub(crate) fn record(outcome: u8, reason: &str, local_read: u64, remote_read: u64, remote_written: u64) {
    let Some(path) = std::env::var_os(REPORT_ENV) else {
        return;
    };
    let path = Path::new(&path);
    if record_at(path, outcome, reason, local_read, remote_read, remote_written).is_err() && path.is_absolute() {
        // A separate create-only marker survives a failed update of the main file.
        // If the whole directory is unavailable, collection itself also fails.
        drop(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.with_extension("incomplete")),
        );
    }
}

fn record_at(
    path: &Path,
    outcome: u8,
    reason: &str,
    local_read: u64,
    remote_read: u64,
    remote_written: u64,
) -> RailResult<()> {
    let mut file = open(path)?;
    let mut recording = read(&mut file)?;
    if recording.finished {
        return Err(RailError::message("cache reporting interval has already finished"));
    }
    let counts = &mut recording.measurements;
    let count = match outcome {
        b'H' => &mut counts.hits,
        b'M' => &mut counts.misses,
        b'B' => &mut counts.bypasses,
        b'F' => &mut counts.failures,
        _ => return Err(RailError::message("unknown cache report outcome")),
    };
    add(count, 1, &mut counts.incomplete);
    add(&mut counts.local_bytes_read, local_read, &mut counts.incomplete);
    add(&mut counts.remote_bytes_read, remote_read, &mut counts.incomplete);
    add(&mut counts.remote_bytes_written, remote_written, &mut counts.incomplete);
    if outcome == b'B' {
        add_reason(&mut counts.bypass_reasons, reason, &mut counts.incomplete);
    }
    if outcome == b'F' || super::installation::NativeCacheFailureReason::from_reason(reason).is_some() {
        add_reason(&mut counts.failure_reasons, reason, &mut counts.incomplete);
    }
    validate_file(&file, path)?;
    write(&mut file, &recording)
}

fn add(counter: &mut u64, value: u64, incomplete: &mut bool) {
    match counter.checked_add(value) {
        Some(next) => *counter = next,
        None => {
            *counter = u64::MAX;
            *incomplete = true;
        }
    }
}

fn add_reason(reasons: &mut BTreeMap<String, u64>, reason: &str, incomplete: &mut bool) {
    if reason.is_empty()
        || reason.len() > 96
        || !reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || (!reasons.contains_key(reason) && reasons.len() >= MAX_REASONS)
    {
        *incomplete = true;
        return;
    }
    add(reasons.entry(reason.to_string()).or_default(), 1, incomplete);
}

fn open(path: &Path) -> RailResult<File> {
    if !path.is_absolute() {
        return Err(RailError::message("cache report path must be absolute"));
    }
    let file = crate::utils::open_cache_lock_file(path, false)?;
    file.lock()?;
    validate_file(&file, path)?;
    Ok(file)
}

fn validate_file(file: &File, path: &Path) -> RailResult<()> {
    let metadata = file.metadata()?;
    if metadata.len() > MAX_BYTES || !crate::utils::private_file_matches_path(file, path, metadata.len())? {
        return Err(RailError::message("cache report must remain one bounded regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(RailError::message("cache report must be private to the current user"));
        }
    }
    Ok(())
}

fn read(file: &mut File) -> RailResult<Recording> {
    let mut bytes = Vec::new();
    std::io::Read::by_ref(file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let recording: Recording = serde_json::from_slice(&bytes)?;
    if recording.schema_version != 1
        || recording.kind != "cargo-rail-cache-recording"
        || recording.measurements.bypass_reasons.len() > MAX_REASONS
        || recording.measurements.failure_reasons.len() > MAX_REASONS
    {
        return Err(RailError::message("unsupported cache recording contract"));
    }
    Ok(recording)
}

fn write(file: &mut File, recording: &Recording) -> RailResult<()> {
    let bytes = serde_json::to_vec(recording)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(RailError::message("cache recording exceeds its byte bound"));
    }
    file.seek(std::io::SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    file.set_len(bytes.len() as u64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_outcomes_are_scoped_and_finish_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.json");
        let second = directory.path().join("second.json");
        start(&first).unwrap();
        start(&second).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let first = &first;
                scope.spawn(move || {
                    for _ in 0..10 {
                        record_at(first, b'H', "local_hit", 10, 20, 30).unwrap();
                    }
                });
            }
        });
        record_at(&first, b'B', "unsupported_invocation", 0, 0, 0).unwrap();
        let measurements = finish(&first).unwrap();
        assert_eq!(
            (
                measurements.hits,
                measurements.bypasses,
                measurements.remote_bytes_written
            ),
            (40, 1, 1200)
        );
        assert_eq!(measurements.bypass_reasons["unsupported_invocation"], 1);
        assert!(!measurements.incomplete);
        assert_eq!(finish(&first).unwrap().hits, 40);
        assert_eq!(finish(&second).unwrap().hits, 0);
        assert!(record_at(&first, b'H', "local_hit", 0, 0, 0).is_err());
        assert!(start(&first).is_err());
    }

    #[test]
    fn recording_gaps_and_corruption_cannot_look_complete() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        start(&path).unwrap();
        std::fs::write(path.with_extension("incomplete"), b"").unwrap();
        assert!(finish(&path).unwrap().incomplete);
        std::fs::write(&path, b"{truncated").unwrap();
        finish(&path).expect_err("corrupt recording must fail collection");
    }
}
