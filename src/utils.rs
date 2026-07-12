//! Utility functions for cross-platform path handling and hashing

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::config::RailConfig;
use crate::error::RailResult;

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
    Ok(bytes) => format!("fnv1a64:{:016x}", fnv1a64(&bytes)),
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
      let normalized = normalize_line_endings(&bytes);
      format!("fnv1a64:{:016x}", fnv1a64(&normalized))
    }
    Err(_) => "none".to_string(),
  }
}

fn normalize_line_endings(bytes: &[u8]) -> Cow<'_, [u8]> {
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
