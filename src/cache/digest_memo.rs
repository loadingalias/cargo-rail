//! Per-profile SHA-256 memo keyed by stable file generation.
//!
//! Warm compiler-cache work is dominated by rehashing unchanged dependency artifacts: each
//! invocation hashes every artifact it reads, and every dependent rereads the same files. A memo
//! entry binds one digest to the complete stable generation of the file it was computed from
//! (device, file identity, size, and modification and change times). A lookup compares that whole
//! generation. A missing, stale, or malformed entry falls back to reading the file.

use crate::source::ContentDigest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
const TREE_VERSION: u32 = 1;
const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024;
/// Record keys start with a versioned text domain, so they cannot equal a file generation.
const TREE_KEY_DOMAIN: &[u8] = b"cargo-rail-tree-memo-v1\0";
const DIRECTORY_KEY_DOMAIN: &[u8] = b"cargo-rail-directory-memo-v1\0";
/// A directory record keeps at most this many files; a merge drops older names beyond it.
const MAX_DIRECTORY_FILES: usize = 16 * 1024;

/// Which directory record a tree names: a recursive namespace, or the direct files of one directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeRecord {
    Namespace,
    Directory,
}

/// Directory records this process loaded, with the files it digested since.
static DIRECTORIES: std::sync::Mutex<BTreeMap<PathBuf, LoadedDirectory>> = std::sync::Mutex::new(BTreeMap::new());

#[derive(Default)]
struct LoadedDirectory {
    files: BTreeMap<String, RememberedFile>,
    added: BTreeMap<String, RememberedFile>,
}

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

/// One regular file's digest, valid only while its complete generation is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RememberedFile {
    generation: String,
    digest: String,
    bytes: u64,
}

impl RememberedFile {
    /// Remember a digest computed from a file whose generation was identical before and after the read,
    /// unless a coarse timestamp could hide a same-tick rewrite.
    pub(crate) fn settled(generation: &[u8], modified: SystemTime, digest: &str, bytes: u64) -> Option<Self> {
        SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age >= RACY_WINDOW)
            .then(|| Self {
                generation: hex(generation),
                digest: digest.to_string(),
                bytes,
            })
    }

    /// A digest the per-file memo already recorded under the settled-generation rule.
    pub(crate) fn recorded(generation: &[u8], digest: &str, bytes: u64) -> Self {
        Self {
            generation: hex(generation),
            digest: digest.to_string(),
            bytes,
        }
    }

    /// The recorded digest and length, if this record describes exactly this generation.
    pub(crate) fn digest_for(&self, generation: &[u8], bytes: u64) -> Option<(&str, u64)> {
        (self.bytes == bytes && self.generation == hex(generation) && valid_digest(&self.digest))
            .then_some((self.digest.as_str(), self.bytes))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeEntry {
    version: u32,
    root: String,
    files: BTreeMap<String, RememberedFile>,
}

impl DigestMemo {
    /// Digests recorded for the regular files below `root`, keyed by their root-relative path.
    ///
    /// One record per directory tree spares a capture one memo read per file. Each digest still
    /// applies only to a file whose current generation matches its record.
    pub(crate) fn tree(&self, root: &Path, record: TreeRecord) -> BTreeMap<String, RememberedFile> {
        let Some(key) = tree_key(root, record) else {
            return BTreeMap::new();
        };
        let Ok(file) = fs::File::open(self.entry_path(&key)) else {
            return BTreeMap::new();
        };
        let mut bytes = Vec::new();
        if file.take(MAX_TREE_BYTES + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_TREE_BYTES {
            return BTreeMap::new();
        }
        match serde_json::from_slice::<TreeEntry>(&bytes) {
            Ok(entry) if entry.version == TREE_VERSION && Some(entry.root.as_str()) == root.to_str() => entry.files,
            _ => BTreeMap::new(),
        }
    }

    /// Replace the record for `root`. Recording is best-effort, like every memo write.
    pub(crate) fn record_tree(&self, root: &Path, record: TreeRecord, files: BTreeMap<String, RememberedFile>) {
        let (Some(key), Some(root)) = (tree_key(root, record), root.to_str()) else {
            return;
        };
        let Ok(entry) = serde_json::to_vec(&TreeEntry {
            version: TREE_VERSION,
            root: root.to_string(),
            files,
        }) else {
            return;
        };
        if entry.len() as u64 <= MAX_TREE_BYTES {
            drop(self.write_bytes(&key, &entry));
        }
    }

    /// Return the `sha256:` digest and byte length recorded for exactly this generation.
    pub(crate) fn lookup(&self, generation: &[u8]) -> Option<(String, u64)> {
        let file = fs::File::open(self.entry_path(generation)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut bytes).ok()?;
        let entry: MemoEntry = serde_json::from_slice(&bytes).ok()?;
        (entry.version == ENTRY_VERSION && entry.generation == hex(generation) && valid_digest(&entry.digest))
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

    /// Whether a readiness proof was recorded for exactly this content identity.
    ///
    /// Readiness keys start with a versioned text domain, so they cannot equal a file generation.
    pub(crate) fn readiness_recorded(&self, key: &[u8]) -> bool {
        self.lookup(key)
            .is_some_and(|(digest, bytes)| bytes == 0 && digest == readiness_digest(key))
    }

    /// Record that the operation identified by `key` succeeded; retention expires it like any entry.
    pub(crate) fn record_readiness(&self, key: &[u8]) {
        drop(self.write(key, &readiness_digest(key), 0));
    }

    fn write(&self, generation: &[u8], digest: &str, bytes: u64) -> std::io::Result<()> {
        let entry = serde_json::to_vec(&MemoEntry {
            version: ENTRY_VERSION,
            generation: hex(generation),
            digest: digest.to_string(),
            bytes,
        })?;
        self.write_bytes(generation, &entry)
    }

    fn write_bytes(&self, key: &[u8], entry: &[u8]) -> std::io::Result<()> {
        let path = self.entry_path(key);
        let shard = path.parent().unwrap_or(&self.directory);
        fs::create_dir_all(shard)?;
        let mut temporary = tempfile::NamedTempFile::new_in(shard)?;
        temporary.write_all(entry)?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(())
    }

    fn entry_path(&self, generation: &[u8]) -> PathBuf {
        let key = ContentDigest::sha256(generation).to_string();
        self.directory.join(key.get(..2).unwrap_or(&key)).join(key)
    }
}

/// The digest recorded for `path` in its directory's record, if it describes exactly this generation.
///
/// A dependency directory holds hundreds of files that each invocation reads. The first lookup in a
/// directory loads its one record; later lookups in this process are in memory.
pub(crate) fn directory_digest(path: &Path, generation: &[u8], bytes: u64) -> Option<(String, u64)> {
    let memo = active()?;
    let (directory, name) = (path.parent()?, path.file_name()?.to_str()?);
    let mut directories = DIRECTORIES.lock().ok()?;
    let loaded = directories
        .entry(directory.to_path_buf())
        .or_insert_with(|| LoadedDirectory {
            files: memo.tree(directory, TreeRecord::Directory),
            added: BTreeMap::new(),
        });
    loaded
        .files
        .get(name)
        .and_then(|file| file.digest_for(generation, bytes))
        .map(|(digest, bytes)| (digest.to_string(), bytes))
}

/// Remember a settled digest for `path` in its directory's record at the next flush.
pub(crate) fn remember_in_directory(path: &Path, file: RememberedFile) {
    if active().is_none() {
        return;
    }
    let (Some(directory), Some(name)) = (path.parent(), path.file_name().and_then(|name| name.to_str())) else {
        return;
    };
    let Ok(mut directories) = DIRECTORIES.lock() else {
        return;
    };
    let loaded = directories.entry(directory.to_path_buf()).or_default();
    if loaded.files.get(name) != Some(&file) {
        loaded.files.insert(name.to_string(), file.clone());
        loaded.added.insert(name.to_string(), file);
    }
}

/// Merge this process's new digests into their directory records. Concurrent writers can drop each
/// other's additions, which only costs a later lookup its fallback read.
pub(crate) fn flush_directories() {
    let Some(memo) = active() else {
        return;
    };
    let Ok(mut directories) = DIRECTORIES.lock() else {
        return;
    };
    for (directory, loaded) in directories.iter_mut() {
        if loaded.added.is_empty() {
            continue;
        }
        let mut files = memo.tree(directory, TreeRecord::Directory);
        if files.len() + loaded.added.len() > MAX_DIRECTORY_FILES {
            files.clear();
        }
        files.append(&mut loaded.added);
        memo.record_tree(directory, TreeRecord::Directory, files);
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
        remember_in_directory(path, RememberedFile::recorded(&generation, digest, bytes));
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

fn tree_key(root: &Path, record: TreeRecord) -> Option<Vec<u8>> {
    let mut key = match record {
        TreeRecord::Namespace => TREE_KEY_DOMAIN,
        TreeRecord::Directory => DIRECTORY_KEY_DOMAIN,
    }
    .to_vec();
    key.extend_from_slice(root.to_str()?.as_bytes());
    Some(key)
}

fn valid_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
}

fn readiness_digest(key: &[u8]) -> String {
    format!("sha256:{}", ContentDigest::sha256(key))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_binds_its_exact_key_and_never_answers_a_digest_lookup() {
        let root = tempfile::tempdir().expect("store root");
        let memo = DigestMemo {
            directory: root.path().join(DIGEST_MEMO_DIRECTORY),
        };
        let key = b"cargo-rail-native-driver-readiness-v1\0driver\0library\0";
        assert!(!memo.readiness_recorded(key));
        memo.record_readiness(key);
        assert!(memo.readiness_recorded(key), "a recorded probe is reused");
        assert!(
            !memo.readiness_recorded(b"cargo-rail-native-driver-readiness-v1\0other-driver\0library\0"),
            "another driver or library identity must probe again"
        );
        let digest = format!("sha256:{}", "ab".repeat(32));
        memo.write(b"generation", &digest, 7).expect("digest entry");
        assert!(
            !memo.readiness_recorded(b"generation"),
            "a file digest entry is not a readiness proof"
        );
    }

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
    fn tree_records_bind_each_file_generation_and_their_root() {
        let root = tempfile::tempdir().expect("store root");
        let memo = DigestMemo {
            directory: root.path().join(DIGEST_MEMO_DIRECTORY),
        };
        let digest = format!("sha256:{}", "ef".repeat(32));
        let settled = SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .expect("settled time");
        assert_eq!(
            RememberedFile::settled(b"generation", SystemTime::now(), &digest, 3),
            None,
            "a just-modified file is not remembered"
        );
        let file = RememberedFile::settled(b"generation", settled, &digest, 3).expect("settled file");
        let namespace = Path::new("/workspace/src");
        memo.record_tree(
            namespace,
            TreeRecord::Namespace,
            BTreeMap::from([("lib.rs".to_string(), file)]),
        );

        let tree = memo.tree(namespace, TreeRecord::Namespace);
        let recorded = tree.get("lib.rs").expect("recorded file");
        assert_eq!(recorded.digest_for(b"generation", 3), Some((digest.as_str(), 3)));
        assert_eq!(recorded.digest_for(b"other-generation", 3), None);
        assert_eq!(recorded.digest_for(b"generation", 4), None);
        assert!(
            memo.tree(Path::new("/workspace/other"), TreeRecord::Namespace)
                .is_empty(),
            "another root never reads this record"
        );
        assert!(
            memo.tree(namespace, TreeRecord::Directory).is_empty(),
            "a directory record never reads a namespace record"
        );
        assert_eq!(
            memo.lookup(&tree_key(namespace, TreeRecord::Namespace).expect("key")),
            None,
            "a tree is not a file digest"
        );
    }

    /// nextest runs each test in its own process, so this activation cannot leak into other tests.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn directory_records_serve_later_lookups_and_merge_on_flush() {
        let store = tempfile::tempdir().expect("store root");
        activate(store.path());
        let memo = active().expect("active memo");
        let directory = Path::new("/workspace/target/debug/deps");
        let digest = format!("sha256:{}", "12".repeat(32));
        let existing = RememberedFile::recorded(b"existing-generation", &digest, 9);
        memo.record_tree(
            directory,
            TreeRecord::Directory,
            BTreeMap::from([("libexisting.rlib".to_string(), existing.clone())]),
        );

        let path = directory.join("libexisting.rlib");
        assert_eq!(
            directory_digest(&path, b"existing-generation", 9),
            Some((digest.clone(), 9)),
            "the first lookup loads the directory record"
        );
        assert_eq!(directory_digest(&path, b"replaced-generation", 9), None);

        remember_in_directory(
            &directory.join("libadded.rlib"),
            RememberedFile::recorded(b"added-generation", &digest, 4),
        );
        assert_eq!(
            directory_digest(&directory.join("libadded.rlib"), b"added-generation", 4),
            Some((digest, 4)),
            "an addition serves this process immediately"
        );
        flush_directories();
        let persisted = memo.tree(directory, TreeRecord::Directory);
        assert_eq!(
            persisted.get("libexisting.rlib"),
            Some(&existing),
            "a flush keeps recorded files"
        );
        assert!(persisted.contains_key("libadded.rlib"), "a flush merges additions");
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
