//! Per-profile SHA-256 memo keyed by stable file generation.
//!
//! Warm compiler-cache work is dominated by rehashing unchanged dependency artifacts: each
//! invocation hashes every artifact it reads, and every dependent rereads the same files. A memo
//! entry binds one digest to the complete stable generation of the file it was computed from
//! (device, file identity, size, and modification and change times). A lookup compares that whole
//! generation. A missing, stale, or malformed entry falls back to reading the file.

use crate::source::ContentDigest;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Store-root directory holding memo entries, sharded by their first key byte.
pub(crate) const DIGEST_MEMO_DIRECTORY: &str = "digest-memo-v1";
/// Files modified this recently are not recorded: a coarse timestamp could hide a same-tick rewrite.
const RACY_WINDOW: Duration = Duration::from_secs(2);
/// Collection removes entries older than this; the next read of their file records them again.
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ENTRY_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: u64 = 4 * 1024;

static ACTIVE: OnceLock<DigestMemo> = OnceLock::new();

/// Memo owned by one local store, active only inside a native compiler-cache invocation.
#[derive(Debug)]
pub(crate) struct DigestMemo {
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoEntry {
    version: u32,
    generation: String,
    digest: String,
    bytes: u64,
}

/// Enable the memo of the store this process's cache context selected.
pub(crate) fn activate(store_root: &Path) {
    drop(ACTIVE.set(DigestMemo {
        directory: store_root.join(DIGEST_MEMO_DIRECTORY),
    }));
}

/// The memo enabled for this process, if a cache context selected one.
///
/// Only Linux and macOS generations include the change time, which no user can set, so a
/// same-size rewrite that restores the modification time still changes the generation there.
/// Windows generations omit it, so Windows always hashes.
pub(crate) fn active() -> Option<&'static DigestMemo> {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        ACTIVE.get()
    } else {
        None
    }
}

impl DigestMemo {
    /// Return the `sha256:` digest and byte length recorded for exactly this generation.
    pub(crate) fn lookup(&self, generation: &[u8]) -> Option<(String, u64)> {
        let file = fs::File::open(self.entry_path(generation)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes).ok()?;
        let entry: MemoEntry = serde_json::from_slice(&bytes).ok()?;
        let valid_digest = entry
            .digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
        (entry.version == ENTRY_VERSION && entry.generation == hex(generation) && valid_digest)
            .then_some((entry.digest, entry.bytes))
    }

    /// Record a digest computed from a file whose generation was identical before and after the read.
    ///
    /// Recording is best-effort: a memo that cannot be written only costs a later rehash.
    pub(crate) fn record(&self, generation: &[u8], modified: SystemTime, digest: &str, bytes: u64) {
        if SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age >= RACY_WINDOW)
        {
            drop(self.write(generation, digest, bytes));
        }
    }

    fn write(&self, generation: &[u8], digest: &str, bytes: u64) -> std::io::Result<()> {
        let path = self.entry_path(generation);
        let shard = path.parent().unwrap_or(&self.directory);
        fs::create_dir_all(shard)?;
        let entry = serde_json::to_vec(&MemoEntry {
            version: ENTRY_VERSION,
            generation: hex(generation),
            digest: digest.to_string(),
            bytes,
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(shard)?;
        temporary.write_all(&entry)?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(())
    }

    fn entry_path(&self, generation: &[u8]) -> PathBuf {
        let key = ContentDigest::sha256(generation).to_string();
        self.directory.join(key.get(..2).unwrap_or(&key)).join(key)
    }
}

/// Record the digest of a file this process just published and verified through `opened`.
///
/// The racy window protects coarse timestamps. A verified restore bypasses it only when the file's
/// change time has sub-second resolution, and only when the path still names the opened file with
/// an identical generation. Otherwise the first settled read records the entry instead.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn seed_verified(opened: &fs::File, path: &Path, digest: &str, bytes: u64) {
    use std::os::unix::fs::MetadataExt as _;

    let Some(memo) = active() else {
        return;
    };
    let Some(generation) = crate::utils::stable_open_file_generation(opened) else {
        return;
    };
    let exact = opened
        .metadata()
        .is_ok_and(|metadata| metadata.len() == bytes && metadata.ctime_nsec() != 0);
    if exact && crate::utils::stable_file_generation(path).as_deref() == Some(generation.as_slice()) {
        drop(memo.write(&generation, digest, bytes));
    }
}

/// The memo is inactive on other hosts, so there is nothing to seed.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn seed_verified(_opened: &fs::File, _path: &Path, _digest: &str, _bytes: u64) {}

/// Remove memo entries older than the retention period. Returns the bytes removed.
pub(crate) fn prune(store_root: &Path) -> std::io::Result<u64> {
    let directory = store_root.join(DIGEST_MEMO_DIRECTORY);
    let shards = match fs::read_dir(&directory) {
        Ok(shards) => shards,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let now = SystemTime::now();
    let mut removed = 0u64;
    for shard in shards {
        let shard = shard?.path();
        if !fs::symlink_metadata(&shard)?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&shard)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            let expired = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_none_or(|age| age >= RETENTION);
            if expired {
                fs::remove_file(&path)?;
                removed = removed.saturating_add(metadata.len());
            }
        }
    }
    Ok(removed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_bind_the_exact_generation_and_respect_the_racy_window() {
        let root = tempfile::tempdir().expect("store root");
        let memo = DigestMemo {
            directory: root.path().join(DIGEST_MEMO_DIRECTORY),
        };
        let digest = format!("sha256:{}", "ab".repeat(32));
        let settled = SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .expect("settled time");

        memo.record(b"generation-a", SystemTime::now(), &digest, 7);
        assert_eq!(
            memo.lookup(b"generation-a"),
            None,
            "a just-modified file is not recorded"
        );

        memo.record(b"generation-a", settled, &digest, 7);
        assert_eq!(memo.lookup(b"generation-a"), Some((digest, 7)));
        assert_eq!(memo.lookup(b"generation-b"), None, "another generation never matches");

        let path = memo.entry_path(b"generation-a");
        fs::write(
            &path,
            b"{\"version\":1,\"generation\":\"00\",\"digest\":\"sha256:x\",\"bytes\":7}",
        )
        .expect("tampered entry");
        assert_eq!(
            memo.lookup(b"generation-a"),
            None,
            "a mismatched entry falls back to reading"
        );
    }

    /// nextest runs each test in its own process, so this activation cannot leak into other tests.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn verified_restores_seed_only_while_the_path_names_the_opened_file() {
        let store = tempfile::tempdir().expect("store root");
        let outputs = tempfile::tempdir().expect("outputs");
        activate(store.path());
        let memo = active().expect("active memo");
        let restored = outputs.path().join("librestored.rlib");
        fs::write(&restored, b"restored bytes").expect("restored output");
        let opened = fs::File::open(&restored).expect("opened output");
        let digest = format!("sha256:{}", ContentDigest::sha256(b"restored bytes"));
        let generation = crate::utils::stable_file_generation(&restored).expect("generation");
        let fine_grained = {
            use std::os::unix::fs::MetadataExt as _;
            opened.metadata().expect("metadata").ctime_nsec() != 0
        };

        seed_verified(&opened, &outputs.path().join("elsewhere.rlib"), &digest, 14);
        assert_eq!(
            memo.lookup(&generation),
            None,
            "a path that is not the opened file is never seeded"
        );

        seed_verified(&opened, &restored, &digest, 14);
        let expected = fine_grained.then(|| (digest.clone(), 14));
        assert_eq!(
            memo.lookup(&generation),
            expected,
            "a just-written verified file is seeded exactly when its change time is sub-second"
        );
    }

    #[test]
    fn prune_removes_only_expired_entries() {
        let root = tempfile::tempdir().expect("store root");
        let memo = DigestMemo {
            directory: root.path().join(DIGEST_MEMO_DIRECTORY),
        };
        let digest = format!("sha256:{}", "cd".repeat(32));
        let settled = SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .expect("settled time");
        memo.record(b"fresh", settled, &digest, 1);
        memo.record(b"stale", settled, &digest, 1);
        fs::File::options()
            .write(true)
            .open(memo.entry_path(b"stale"))
            .expect("stale entry")
            .set_modified(
                SystemTime::now()
                    .checked_sub(RETENTION + Duration::from_secs(1))
                    .expect("expired time"),
            )
            .expect("age the stale entry");

        assert!(prune(root.path()).expect("prune") > 0);
        assert!(memo.lookup(b"fresh").is_some());
        assert!(memo.lookup(b"stale").is_none());
    }
}
