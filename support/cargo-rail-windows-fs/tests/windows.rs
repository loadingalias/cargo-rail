#![cfg(windows)]

use cargo_rail_windows_fs::{observe_file, open_for_observation, prove_local_ntfs, rename_write_through};
use std::fs::{self, File};
use std::io;

#[test]
fn ordinary_byte_mutation_advances_ntfs_change_time() -> io::Result<()> {
  let directory = tempfile::tempdir()?;
  let path = directory.path().join("input.rs");
  fs::write(&path, b"X")?;

  let before_file = File::open(&path)?;
  let before = observe_file(&before_file)?;
  if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
    return Ok(());
  }
  drop(before_file);

  fs::write(&path, b"Y")?;
  let after = observe_file(&File::open(&path)?)?;
  assert_eq!(
    after.file_id, before.file_id,
    "ordinary writes must not replace the file"
  );
  assert_eq!(
    after.size, before.size,
    "the mutation intentionally preserves file length"
  );
  assert!(
    after.change_time > before.change_time,
    "rapid X-to-Y mutation must advance NTFS ChangeTime: before={}, after={}",
    before.change_time,
    after.change_time
  );
  Ok(())
}

#[test]
fn file_id_is_stable_across_write_through_rename() -> io::Result<()> {
  let directory = tempfile::tempdir()?;
  let before_path = directory.path().join("before.rmeta");
  let after_path = directory.path().join("after.rmeta");
  fs::write(&before_path, b"artifact")?;

  let before_file = File::open(&before_path)?;
  let before = observe_file(&before_file)?;
  if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
    return Ok(());
  }
  drop(before_file);
  rename_write_through(&before_path, &after_path, false)?;
  let after = observe_file(&File::open(&after_path)?)?;

  assert_eq!(after.volume_serial_number, before.volume_serial_number);
  assert_eq!(
    after.file_id, before.file_id,
    "a same-volume rename must preserve file identity"
  );
  assert!(!before_path.exists());
  assert_eq!(fs::read(&after_path)?, b"artifact");
  Ok(())
}

#[test]
fn local_ntfs_proof_succeeds_or_reports_unsupported() -> io::Result<()> {
  let directory = tempfile::tempdir()?;
  let path = directory.path().join("proof");
  fs::write(&path, b"proof")?;
  let file = File::open(path)?;
  let observation = observe_file(&file)?;

  let _supported = local_ntfs_or_explicitly_unsupported(&file, observation.volume_serial_number)?;
  Ok(())
}

#[test]
fn directories_have_handle_bound_change_time_and_volume_proof() -> io::Result<()> {
  let directory = tempfile::tempdir()?;
  let before_file = open_for_observation(directory.path())?;
  let before = observe_file(&before_file)?;
  if !local_ntfs_or_explicitly_unsupported(&before_file, before.volume_serial_number)? {
    return Ok(());
  }
  drop(before_file);

  let transient = directory.path().join("transient");
  fs::write(&transient, b"value")?;
  fs::remove_file(transient)?;

  let after_file = open_for_observation(directory.path())?;
  let after = observe_file(&after_file)?;
  assert_eq!(
    after.file_id, before.file_id,
    "the directory itself must not be replaced"
  );
  assert!(
    after.change_time > before.change_time,
    "a create/delete mutation must advance the parent directory ChangeTime: before={}, after={}",
    before.change_time,
    after.change_time
  );
  Ok(())
}

#[test]
fn write_through_rename_preserves_or_replaces_destination_as_requested() -> io::Result<()> {
  let directory = tempfile::tempdir()?;
  let source = directory.path().join("source");
  let destination = directory.path().join("destination");
  fs::write(&source, b"new")?;
  fs::write(&destination, b"old")?;

  let error = rename_write_through(&source, &destination, false)
    .expect_err("a no-clobber rename must reject an existing destination");
  assert_ne!(error.kind(), io::ErrorKind::NotFound);
  assert_eq!(fs::read(&source)?, b"new");
  assert_eq!(fs::read(&destination)?, b"old");

  rename_write_through(&source, &destination, true)?;
  assert!(!source.exists());
  assert_eq!(fs::read(&destination)?, b"new");
  Ok(())
}

fn local_ntfs_or_explicitly_unsupported(file: &File, volume_serial_number: u64) -> io::Result<bool> {
  match prove_local_ntfs(file, volume_serial_number) {
    Ok(proof) => {
      assert_eq!(proof.volume_serial_number, volume_serial_number);
      Ok(true)
    }
    Err(error) if error.kind() == io::ErrorKind::Unsupported => {
      eprintln!("local NTFS proof is unavailable on this test volume: {error}");
      Ok(false)
    }
    Err(error) => Err(error),
  }
}
