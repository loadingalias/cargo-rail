//! Utility functions for cross-platform path handling and hashing

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::config::RailConfig;
use crate::error::{RailError, RailResult};

/// Return whether metadata names a symlink or another Windows reparse point.
///
/// `FileType::is_symlink` does not classify directory junctions on Windows.
/// Cache authority must reject every reparse point before traversing it.
pub(crate) fn is_symlink_or_reparse(metadata: &fs::Metadata) -> bool {
  if metadata.file_type().is_symlink() {
    return true;
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
  }
  #[cfg(not(windows))]
  {
    false
  }
}

/// Open a cache lock without permitting pathname replacement on Windows.
pub(crate) fn open_cache_lock_file(path: &Path, create: bool) -> io::Result<fs::File> {
  let mut options = fs::OpenOptions::new();
  options.read(true).write(true).create(create).truncate(false);
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
  }
  options.open(path)
}

/// Verify that a path still names one private opened regular file.
pub(crate) fn private_file_matches_path(opened: &fs::File, path: &Path, expected_len: u64) -> io::Result<bool> {
  let opened_metadata = opened.metadata()?;
  let named_metadata = fs::symlink_metadata(path)?;
  if !opened_metadata.is_file()
    || !named_metadata.is_file()
    || is_symlink_or_reparse(&named_metadata)
    || opened_metadata.len() != expected_len
    || named_metadata.len() != expected_len
  {
    return Ok(false);
  }

  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;

    Ok(
      opened_metadata.dev() == named_metadata.dev()
        && opened_metadata.ino() == named_metadata.ino()
        && opened_metadata.nlink() == 1
        && named_metadata.nlink() == 1,
    )
  }
  #[cfg(windows)]
  {
    let named = fs::File::open(path)?;
    let named_handle_metadata = named.metadata()?;
    if !named_handle_metadata.is_file()
      || is_symlink_or_reparse(&named_handle_metadata)
      || named_handle_metadata.len() != expected_len
    {
      return Ok(false);
    }
    let opened_information = winapi_util::file::information(opened)?;
    let named_information = winapi_util::file::information(&named)?;
    Ok(
      opened_information.volume_serial_number() == named_information.volume_serial_number()
        && opened_information.file_index() == named_information.file_index()
        && opened_information.number_of_links() == 1
        && named_information.number_of_links() == 1
        && opened_information.file_size() == expected_len
        && named_information.file_size() == expected_len,
    )
  }
  #[cfg(not(any(unix, windows)))]
  {
    Ok(true)
  }
}

// ============================================================================
// Hashing and Fingerprinting
// ============================================================================

/// FNV-1a 64-bit hash function
///
/// A fast, non-cryptographic hash suitable for fingerprinting file contents.
/// Used for change detection and cache invalidation.
#[inline]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
  const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
  const FNV_PRIME: u64 = 0x100000001b3;

  crate::instrumentation::record_hash(bytes.len());
  let mut hash = FNV_OFFSET_BASIS;
  for byte in bytes {
    hash ^= *byte as u64;
    hash = hash.wrapping_mul(FNV_PRIME);
  }
  hash
}

/// Compute a fingerprint for a file's contents
///
/// Produces a formatted fingerprint like `fnv1a64:0123456789abcdef`,
/// or `"none"` when the file cannot be read.
pub fn file_fingerprint(path: &Path) -> String {
  match fs::read(path) {
    Ok(bytes) => {
      crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
      format!("fnv1a64:{:016x}", fnv1a64(&bytes))
    }
    Err(_) => "none".to_string(),
  }
}

/// Compute a content fingerprint after normalizing text line endings.
///
/// Git may materialize the same text as LF or CRLF according to checkout
/// configuration. Configuration identities must describe the parsed text, not
/// the platform's checkout filter.
fn text_file_fingerprint(path: &Path) -> String {
  match fs::read(path) {
    Ok(bytes) => {
      crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
      let normalized = normalize_line_endings(&bytes);
      format!("fnv1a64:{:016x}", fnv1a64(&normalized))
    }
    Err(_) => "none".to_string(),
  }
}

pub(crate) fn normalize_line_endings(bytes: &[u8]) -> Cow<'_, [u8]> {
  if !bytes.contains(&b'\r') {
    return Cow::Borrowed(bytes);
  }

  let mut normalized = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    if bytes[index] == b'\r' {
      normalized.push(b'\n');
      index += usize::from(bytes.get(index + 1) == Some(&b'\n'));
    } else {
      normalized.push(bytes[index]);
    }
    index += 1;
  }
  Cow::Owned(normalized)
}

/// Atomically replace a file without following a predictable temporary path.
///
/// The temporary file is created exclusively in the destination directory, so
/// a concurrent process cannot pre-create a symlink at the chosen path and the
/// final rename stays on the same filesystem.
pub(crate) fn write_file_atomic(path: &Path, contents: &[u8]) -> RailResult<()> {
  let parent = destination_parent(path);
  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-")
    .suffix(".tmp")
    .tempfile_in(parent)
    .map_err(|error| {
      RailError::message(format!(
        "failed to create temporary file in {}: {error}",
        parent.display()
      ))
    })?;

  if let Ok(metadata) = fs::symlink_metadata(path)
    && metadata.file_type().is_file()
  {
    temporary
      .as_file()
      .set_permissions(metadata.permissions())
      .map_err(|error| {
        RailError::message(format!(
          "failed to preserve permissions for {}: {error}",
          path.display()
        ))
      })?;
  }
  temporary
    .write_all(contents)
    .and_then(|()| temporary.as_file().sync_all())
    .map_err(|error| {
      RailError::message(format!(
        "failed to write temporary file for {}: {error}",
        path.display()
      ))
    })?;
  let persisted = {
    #[cfg(windows)]
    {
      persist_atomic_replacement(temporary, path)
    }
    #[cfg(not(windows))]
    {
      temporary.persist(path)
    }
  };
  persisted.map_err(|error| {
    RailError::message(format!(
      "failed to atomically replace {}: {}",
      path.display(),
      error.error
    ))
  })?;
  sync_parent_directory(parent).map_err(|error| {
    RailError::message(format!(
      "failed to persist the atomic replacement for {}: {error}",
      path.display()
    ))
  })?;
  Ok(())
}

#[cfg(windows)]
fn persist_atomic_replacement(
  mut temporary: tempfile::NamedTempFile,
  path: &Path,
) -> Result<fs::File, tempfile::PersistError> {
  const MAX_ATTEMPTS: usize = 50;
  let mut attempts = 0;
  loop {
    match temporary.persist(path) {
      Ok(file) => return Ok(file),
      Err(error) => {
        attempts += 1;
        let transient_reader_conflict = matches!(error.error.raw_os_error(), Some(5 | 32 | 33));
        if !transient_reader_conflict || attempts >= MAX_ATTEMPTS {
          return Err(error);
        }
        temporary = error.file;
        std::thread::sleep(std::time::Duration::from_millis(1));
      }
    }
  }
}

fn destination_parent(path: &Path) -> &Path {
  path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
  fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
  Ok(())
}

/// Compute a fingerprint for the rail.toml configuration file
///
/// Searches standard config locations and emits the fingerprint,
/// or `"none"` when no config file is found.
pub fn config_fingerprint(workspace_root: &Path) -> String {
  RailConfig::find_config_path(workspace_root)
    .map(|p| text_file_fingerprint(&p))
    .unwrap_or_else(|| "none".to_string())
}

/// Compute a fingerprint for the Rust toolchain file
///
/// Checks `rust-toolchain.toml` then `rust-toolchain` and emits the
/// fingerprint of the first match, or `"none"` if neither exists.
pub fn toolchain_fingerprint(workspace_root: &Path) -> String {
  ["rust-toolchain.toml", "rust-toolchain"]
    .iter()
    .map(|name| workspace_root.join(name))
    .find(|p| p.exists())
    .map(|p| text_file_fingerprint(&p))
    .unwrap_or_else(|| "none".to_string())
}

/// Canonicalize an existing path and return a form suitable for both Rust and
/// external tools on the current platform.
///
/// Windows canonicalization adds a verbatim `\\?\` prefix. That representation
/// is safe for Win32 APIs but is not accepted consistently by Git for Windows,
/// and it does not compare lexically with ordinary absolute paths.
pub fn canonicalize_existing(path: &Path) -> io::Result<PathBuf> {
  fs::canonicalize(path).map(simplify_canonical_path)
}

/// Resolve a possibly missing path through its nearest existing ancestor.
///
/// Existing ancestors are canonicalized, so symlink escapes remain visible;
/// only a normalized, non-existent suffix is appended afterward.
pub fn canonicalize_allow_missing(path: &Path) -> io::Result<PathBuf> {
  let absolute = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir()?.join(path)
  };
  let normalized = normalize_absolute_path(&absolute)?;
  if fs::symlink_metadata(&normalized).is_ok() {
    return canonicalize_existing(&normalized);
  }

  let mut ancestor = normalized.as_path();
  let mut suffix = Vec::<OsString>::new();
  loop {
    match fs::symlink_metadata(ancestor) {
      Ok(_) => break,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        let name = ancestor.file_name().ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no existing ancestor", path.display()),
          )
        })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no existing ancestor", path.display()),
          )
        })?;
      }
      Err(error) => return Err(error),
    }
  }

  let mut resolved = canonicalize_existing(ancestor)?;
  for component in suffix.iter().rev() {
    resolved.push(component);
  }
  Ok(resolved)
}

/// Return `path` relative to `root` after resolving both representations.
pub fn path_relative_to(root: &Path, path: &Path) -> io::Result<PathBuf> {
  let root = canonicalize_existing(root)?;
  let path = canonicalize_allow_missing(path)?;
  path.strip_prefix(&root).map(Path::to_path_buf).map_err(|_| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("path '{}' is outside root '{}'", path.display(), root.display()),
    )
  })
}

fn normalize_absolute_path(path: &Path) -> io::Result<PathBuf> {
  let mut normalized = PathBuf::new();
  for component in path.components() {
    match component {
      Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
      Component::RootDir => normalized.push(component.as_os_str()),
      Component::CurDir => {}
      Component::ParentDir => {
        if !normalized.pop() {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' escapes the filesystem root", path.display()),
          ));
        }
      }
      Component::Normal(part) => normalized.push(part),
    }
  }
  Ok(normalized)
}

#[cfg(not(windows))]
fn simplify_canonical_path(path: PathBuf) -> PathBuf {
  path
}

#[cfg(windows)]
fn simplify_canonical_path(path: PathBuf) -> PathBuf {
  use std::path::Prefix;

  let mut components = path.components();
  let Some(Component::Prefix(prefix)) = components.next() else {
    return path;
  };
  let mut simplified = match prefix.kind() {
    Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive).to_ascii_uppercase())),
    Prefix::VerbatimUNC(server, share) => {
      let mut raw = OsString::from(r"\\");
      raw.push(server);
      raw.push(r"\");
      raw.push(share);
      PathBuf::from(raw)
    }
    _ => return path,
  };
  for component in components {
    simplified.push(component.as_os_str());
  }
  simplified
}

// ============================================================================
// Path Utilities
// ============================================================================

/// Check if a path is a local filesystem path (not a remote URL)
///
/// Returns true for:
/// - Absolute paths on Unix: /path/to/repo
/// - Absolute paths on Windows: C:\path\to\repo or C:/path/to/repo
/// - Relative paths: ./path or ../path
/// - UNC paths on Windows: \\server\share
///
/// Returns false for:
/// - SSH URLs: `git@github.com:user/repo.git`
/// - HTTPS URLs: `https://github.com/user/repo.git`
pub fn is_local_path(path: &str) -> bool {
  let p = Path::new(path);

  // Check for relative paths
  if path.starts_with("./") || path.starts_with("../") {
    return true;
  }

  // Check for Windows drive letter (C:\ or C:/)
  // Must check before URL check since Windows paths contain ':'
  if path.len() >= 3 {
    let bytes = path.as_bytes();
    if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
      return true;
    }
  }

  // Check for Windows UNC paths (\\server\share)
  if path.starts_with("\\\\") {
    return true;
  }

  // Check for Unix absolute paths (/path/to/repo)
  // Important: Check this BEFORE is_absolute() because on Windows,
  // Path::is_absolute() returns false for Unix-style paths
  if path.starts_with('/') {
    // Make sure it's not part of a URL pattern
    if !path.contains("://") && !path.contains('@') {
      return true;
    }
  }

  // Check for absolute paths (fallback for platform-specific cases)
  if p.is_absolute() {
    return true;
  }

  // If it contains :// it's a URL
  if path.contains("://") {
    return false;
  }

  // If it contains @ it's likely an SSH URL (git@github.com:user/repo.git)
  if path.contains('@') {
    return false;
  }

  // Default to false for safety (require preflight checks)
  false
}

/// Prompt user for confirmation (Enter to confirm, Ctrl+C or any input to cancel)
///
/// Returns Ok(true) if user presses Enter without typing anything.
/// Returns Ok(false) if user types anything before pressing Enter.
pub fn prompt_for_confirmation(message: &str) -> RailResult<bool> {
  print!("\n{}: ", message);
  io::stdout().flush()?;

  let mut input = String::new();
  io::stdin().read_line(&mut input)?;

  // If user just presses Enter (empty line), that's a confirmation
  Ok(input.trim().is_empty())
}

/// Detect CHANGELOG file in a crate directory
///
/// Searches for common changelog file patterns and returns the first match found.
pub fn detect_crate_changelog(crate_dir: &cargo_metadata::camino::Utf8Path) -> Option<std::path::PathBuf> {
  let changelog_patterns = [
    "CHANGELOG.md",
    "CHANGELOG.txt",
    "CHANGELOG",
    "Changelog.md",
    "changelog.md",
    "CHANGES.md",
    "CHANGES.txt",
    "CHANGES",
    "Changes.md",
    "changes.md",
  ];

  for pattern in &changelog_patterns {
    let changelog = crate_dir.join(pattern);
    if changelog.exists() {
      // Return relative path from crate root
      return Some(std::path::PathBuf::from(pattern));
    }
  }

  None
}

/// Convert a path to Git format (always forward slashes)
///
/// Git expects paths with forward slashes, even on Windows.
/// This function converts backslashes to forward slashes for use in Git commands.
pub fn path_to_git_format(path: &Path) -> String {
  path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

  use super::*;
  use std::path::PathBuf;

  #[test]
  fn test_fnv1a64_empty() {
    assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
  }

  #[test]
  fn test_fnv1a64_known_values() {
    // FNV-1a test vectors
    assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
  }

  #[test]
  fn test_fnv1a64_deterministic() {
    let data = b"hello world";
    assert_eq!(fnv1a64(data), fnv1a64(data));
  }

  #[test]
  fn test_file_fingerprint_missing_file() {
    let result = file_fingerprint(Path::new("/nonexistent/path/to/file"));
    assert_eq!(result, "none");
  }

  #[test]
  fn atomic_writes_use_current_directory_for_bare_filenames() {
    assert_eq!(destination_parent(Path::new("rail.toml")), Path::new("."));
    assert_eq!(destination_parent(Path::new("config/rail.toml")), Path::new("config"));
  }

  #[test]
  fn atomic_replacement_never_exposes_partial_bytes_to_concurrent_readers() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("state");
    let first = Arc::new(vec![b'a'; 256 * 1024]);
    let second = Arc::new(vec![b'b'; 256 * 1024]);
    write_file_atomic(&destination, &first).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
      let reader_stop = Arc::clone(&stop);
      let reader_reads = Arc::clone(&reads);
      let reader_first = Arc::clone(&first);
      let reader_second = Arc::clone(&second);
      let destination = &destination;
      let reader = scope.spawn(move || {
        while !reader_stop.load(Ordering::Acquire) {
          let observed = fs::read(destination).expect("the destination must remain visible");
          assert!(
            observed == *reader_first || observed == *reader_second,
            "a reader observed a partial atomic replacement"
          );
          reader_reads.fetch_add(1, Ordering::Relaxed);
          if cfg!(windows) {
            // Leave the writer a gap between NTFS read handles so the bounded
            // replacement retry can observe a normal sharing window.
            std::thread::sleep(std::time::Duration::from_millis(1));
          }
        }
      });

      // One concurrent NTFS rename exercises the visibility boundary without
      // turning this oracle into an EBS flush-latency test.
      let replacements = if cfg!(windows) { 1 } else { 64 };
      let replacement = (|| {
        for iteration in 0..replacements {
          let contents = if iteration % 2 == 0 { &second } else { &first };
          write_file_atomic(destination, contents)?;
        }
        Ok::<(), RailError>(())
      })();
      stop.store(true, Ordering::Release);
      reader.join().unwrap();
      replacement.unwrap();
    });
    assert!(reads.load(Ordering::Relaxed) > 0, "the concurrent reader did not run");
  }

  #[cfg(unix)]
  #[test]
  fn atomic_replacement_preserves_mode_and_replaces_symlinks_without_following_them() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("state");
    fs::write(&destination, b"old").unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
    write_file_atomic(&destination, b"new").unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"new");
    assert_eq!(fs::metadata(&destination).unwrap().permissions().mode() & 0o777, 0o600);

    let victim = directory.path().join("victim");
    fs::write(&victim, b"keep").unwrap();
    fs::remove_file(&destination).unwrap();
    symlink(&victim, &destination).unwrap();
    write_file_atomic(&destination, b"replacement").unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    assert_eq!(fs::read(&destination).unwrap(), b"replacement");
    assert!(!fs::symlink_metadata(&destination).unwrap().file_type().is_symlink());
  }

  #[test]
  fn atomic_replacement_preserves_the_old_file_when_the_filesystem_is_full() {
    const ROOT_ENV: &str = "CARGO_RAIL_TEST_ENOSPC_ROOT";
    const MAX_BYTES_ENV: &str = "CARGO_RAIL_TEST_ENOSPC_MAX_BYTES";
    let (Some(root), Some(maximum)) = (std::env::var_os(ROOT_ENV), std::env::var_os(MAX_BYTES_ENV)) else {
      return;
    };
    let maximum = maximum
      .to_str()
      .and_then(|value| value.parse::<u64>().ok())
      .expect("the ENOSPC byte bound must be an unsigned integer");
    assert!(
      (1024 * 1024..=512 * 1024 * 1024).contains(&maximum),
      "the ENOSPC byte bound must be between 1 MiB and 512 MiB"
    );

    let directory = tempfile::Builder::new()
      .prefix("cargo-rail-enospc-")
      .tempdir_in(PathBuf::from(root))
      .expect("isolated ENOSPC test directory");
    let destination = directory.path().join("state");
    fs::write(&destination, b"old").expect("initial state");
    let filler_path = directory.path().join("filler");
    let mut filler = fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&filler_path)
      .expect("filler file");
    let mut block = vec![0_u8; 1024 * 1024];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for chunk in block.chunks_exact_mut(8) {
      state ^= state << 7;
      state ^= state >> 9;
      state ^= state << 8;
      chunk.copy_from_slice(&state.to_le_bytes());
    }

    let mut written = 0_u64;
    let exhausted = loop {
      match filler.write_all(&block).and_then(|()| filler.sync_data()) {
        Ok(()) => {
          written = written.saturating_add(block.len() as u64);
          if written >= maximum {
            break None;
          }
        }
        Err(error) if error.kind() == io::ErrorKind::StorageFull => break Some(error),
        Err(error) => panic!("isolated filesystem failed before ENOSPC: {error}"),
      }
    };
    if exhausted.is_none() {
      drop(filler);
      fs::remove_file(&filler_path).expect("remove bounded filler");
      panic!("isolated filesystem did not report ENOSPC within {maximum} bytes");
    }

    let replacement = vec![b'n'; 1024 * 1024];
    let error = write_file_atomic(&destination, &replacement).expect_err("replacement must fail on a full filesystem");
    assert!(
      error.to_string().contains("temporary file") || error.to_string().contains("atomically replace"),
      "{error}"
    );
    assert_eq!(fs::read(&destination).expect("original state"), b"old");
    assert_eq!(
      fs::read_dir(directory.path())
        .expect("test directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".cargo-rail-"))
        .count(),
      0,
      "a failed atomic replacement must clean its private temporary file"
    );

    drop(filler);
    fs::remove_file(&filler_path).expect("free isolated filesystem capacity");
    write_file_atomic(&destination, b"new").expect("replacement should recover after capacity is available");
    assert_eq!(fs::read(&destination).expect("recovered state"), b"new");
  }

  #[test]
  fn test_text_fingerprint_ignores_checkout_line_endings() {
    let temp = tempfile::tempdir().unwrap();
    let lf = temp.path().join("lf.toml");
    let crlf = temp.path().join("crlf.toml");
    fs::write(&lf, b"[workspace]\nroot = \".\"\n").unwrap();
    fs::write(&crlf, b"[workspace]\r\nroot = \".\"\r\n").unwrap();

    assert_eq!(text_file_fingerprint(&lf), text_file_fingerprint(&crlf));
    assert_ne!(file_fingerprint(&lf), file_fingerprint(&crlf));
  }

  #[test]
  fn test_path_relative_to_resolves_existing_and_missing_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("existing.txt"), b"content").unwrap();

    assert_eq!(
      path_relative_to(&root, &root.join("existing.txt")).unwrap(),
      PathBuf::from("existing.txt")
    );
    assert_eq!(
      path_relative_to(&root, &root.join("new/child.txt")).unwrap(),
      PathBuf::from("new/child.txt")
    );
    assert!(path_relative_to(&root, temp.path()).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn test_path_relative_to_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file.txt"), b"outside").unwrap();
    symlink(&outside, root.join("escape")).unwrap();

    assert!(path_relative_to(&root, &root.join("escape/file.txt")).is_err());
  }

  #[cfg(windows)]
  #[test]
  fn test_canonical_paths_are_external_tool_compatible_on_windows() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = canonicalize_existing(temp.path()).unwrap();
    assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
  }

  #[test]
  fn test_is_local_path_classification() {
    // Local paths - absolute
    assert!(is_local_path("/home/user/repo"));
    assert!(is_local_path("C:\\Users\\test\\repo"));
    assert!(is_local_path("C:/Users/test/repo"));

    // Local paths - relative
    assert!(is_local_path("./repo"));
    assert!(is_local_path("../path/to/repo"));

    // Windows UNC paths (platform-specific)
    #[cfg(target_os = "windows")]
    assert!(is_local_path("\\\\server\\share\\repo"));

    // Remote URLs - various protocols
    assert!(!is_local_path("git@github.com:user/repo.git"));
    assert!(!is_local_path("https://github.com/user/repo.git"));
    assert!(!is_local_path("ssh://git@github.com/user/repo.git"));

    // Edge cases - ambiguous bare names
    assert!(!is_local_path("repo"));
    assert!(!is_local_path(""));
  }

  #[test]
  fn test_path_to_git_format_preserves_forward_slashes() {
    let path = PathBuf::from("/home/user/repo/src/main.rs");
    assert_eq!(path_to_git_format(&path), "/home/user/repo/src/main.rs");

    let path = PathBuf::from("./relative/path.rs");
    assert_eq!(path_to_git_format(&path), "./relative/path.rs");
  }

  #[test]
  fn test_path_to_git_format_normalizes_backslashes_on_every_host() {
    let path = PathBuf::from("C:\\Users\\test\\repo\\src\\main.rs");
    assert_eq!(path_to_git_format(&path), "C:/Users/test/repo/src/main.rs");

    let path = PathBuf::from("..\\relative\\path.rs");
    assert_eq!(path_to_git_format(&path), "../relative/path.rs");
  }
}
