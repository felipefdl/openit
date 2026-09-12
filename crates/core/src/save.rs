//! Writing a snapshot to disk without ever leaving a half-written original.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use tempfile::NamedTempFile;

use crate::document::{Revision, Snapshot};
use crate::error::Error;
use crate::watch::Fingerprint;

/// Outcome of a successful save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saved {
  /// The revision now on disk.
  pub revision: Revision,
  /// The fingerprint of the bytes written to disk.
  pub fingerprint: Fingerprint,
}

type PathLocks = HashMap<PathBuf, Arc<Mutex<()>>>;

/// One lock per path, so two windows saving the same file take turns and the
/// last rename wins with a complete file.
///
/// The map is never shrunk on purpose: an entry is two words next to a path a
/// user is actively editing, and removing an idle one would race a saver that
/// already cloned it.
static PATH_LOCKS: LazyLock<Mutex<PathLocks>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
  let mut locks = PATH_LOCKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  Arc::clone(locks.entry(path.to_path_buf()).or_default())
}

/// The file a save should actually replace.
///
/// Renaming over a symlink would replace the link itself with a regular file
/// and orphan its target, so an existing path is resolved to what it points
/// at. A path with no file yet cannot be resolved and stands as given.
fn resolve(path: &Path) -> PathBuf {
  fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Write through a temp file in `dir` and rename it over `target`. `prepare`
/// runs on the open temp file before any byte is written (permissions), and
/// `write` streams the content. The target is never opened for writing.
pub(crate) fn write_atomic(
  target: &Path,
  dir: &Path,
  prepare: impl FnOnce(&File) -> io::Result<()>,
  write: impl FnOnce(&mut BufWriter<&File>) -> io::Result<()>,
) -> io::Result<()> {
  write_atomic_with_result(target, dir, prepare, write, |_| Ok(()))
}

fn write_atomic_with_result<R>(
  target: &Path,
  dir: &Path,
  prepare: impl FnOnce(&File) -> io::Result<()>,
  write: impl FnOnce(&mut BufWriter<&File>) -> io::Result<()>,
  result: impl FnOnce(&File) -> io::Result<R>,
) -> io::Result<R> {
  let temp = NamedTempFile::new_in(dir)?;
  prepare(temp.as_file())?;
  {
    let mut writer = BufWriter::new(temp.as_file());
    write(&mut writer)?;
    writer.flush()?;
  }
  temp.as_file().sync_all()?;
  let result = result(temp.as_file())?;
  temp.persist(target).map_err(|e| e.error)?;
  Ok(result)
}

/// Write `snapshot.text` to `path` atomically.
///
/// The bytes go to a temporary file in the same directory, which is then
/// renamed over the target. The original is never truncated in place, so a
/// crash mid-write leaves the old contents intact. An existing file keeps its
/// permissions, and a symlink keeps pointing at the file that was written.
pub fn save_text(path: &Path, snapshot: &Snapshot) -> Result<Saved, Error> {
  let target = resolve(path);
  let lock = lock_for(&target);
  let _guard = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

  let write_err = |source: io::Error| Error::Write { path: path.to_path_buf(), source };
  let existing = fs::metadata(&target).ok();
  if existing.as_ref().is_some_and(|meta| meta.permissions().readonly()) {
    return Err(write_err(io::Error::new(io::ErrorKind::PermissionDenied, "file is read-only")));
  }
  let dir = target
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  // Set on the open file, not requested at creation: a mode passed to `open`
  // is masked by the process umask, so a 0666 file would come back 0644.
  let fingerprint = write_atomic_with_result(
    &target,
    dir,
    |file| {
      existing
        .as_ref()
        .map_or(Ok(()), |meta| file.set_permissions(meta.permissions()))
    },
    |writer| {
      for chunk in snapshot.text.chunks() {
        writer.write_all(chunk.as_bytes())?;
      }
      Ok(())
    },
    Fingerprint::of_file,
  )
  .map_err(write_err)?;
  tracing::debug!(path = %target.display(), revision = ?snapshot.revision, "saved");
  Ok(Saved { revision: snapshot.revision, fingerprint })
}

/// Atomically replace `path` with `bytes`, the same temp-file-and-rename path
/// and per-path lock `save_text` uses.
pub fn save_bytes(path: &Path, revision: Revision, bytes: &[u8]) -> Result<Saved, Error> {
  let target = resolve(path);
  let lock = lock_for(&target);
  let _guard = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

  let write_err = |source: io::Error| Error::Write { path: path.to_path_buf(), source };
  let existing = fs::metadata(&target).ok();
  if existing.as_ref().is_some_and(|meta| meta.permissions().readonly()) {
    return Err(write_err(io::Error::new(io::ErrorKind::PermissionDenied, "file is read-only")));
  }
  let dir = target
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  let fingerprint = write_atomic_with_result(
    &target,
    dir,
    |file| {
      existing
        .as_ref()
        .map_or(Ok(()), |meta| file.set_permissions(meta.permissions()))
    },
    |writer| writer.write_all(bytes),
    Fingerprint::of_file,
  )
  .map_err(write_err)?;
  tracing::debug!(path = %target.display(), ?revision, bytes = bytes.len(), "saved bytes");
  Ok(Saved { revision, fingerprint })
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::sync::{Arc, Barrier};
  use std::thread;
  use std::time::Duration;

  use ropey::Rope;

  use super::{lock_for, resolve, save_bytes, save_text};
  use crate::document::{Revision, Snapshot};
  use crate::watch::Fingerprint;

  fn snapshot(text: &str, revision: Revision) -> Snapshot {
    Snapshot { revision, text: Rope::from_str(text) }
  }

  #[test]
  fn writes_the_snapshot_and_reports_its_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let revision = Revision::INITIAL.next().next();

    let saved = save_text(&path, &snapshot("new\n", revision)).unwrap();

    assert_eq!(saved.revision, revision);
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
  }

  #[test]
  fn save_bytes_replaces_the_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    fs::write(&path, b"old").unwrap();
    let revision = Revision::INITIAL.next();

    let saved = save_bytes(&path, revision, b"new bytes").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"new bytes");
    assert_eq!(saved.revision, revision);
    assert_eq!(saved.fingerprint, Fingerprint::of(&path).unwrap());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no temp file left behind");
  }

  #[test]
  fn creates_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh.md");

    save_text(&path, &snapshot("# hi\n", Revision::INITIAL)).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "# hi\n");
  }

  #[test]
  fn leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");

    save_text(&path, &snapshot("x", Revision::INITIAL)).unwrap();

    let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("a.txt")]);
  }

  #[test]
  fn reports_a_write_error_when_the_directory_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-dir").join("a.txt");

    let err = save_text(&path, &snapshot("x", Revision::INITIAL)).unwrap_err();

    assert!(matches!(err, crate::Error::Write { .. }));
    assert!(!path.exists());
  }

  #[test]
  fn concurrent_saves_leave_a_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = Arc::new(dir.path().join("a.txt"));
    let bodies = ["a".repeat(200_000), "b".repeat(200_000)];

    let handles: Vec<_> = bodies
      .iter()
      .cloned()
      .map(|body| {
        let path = Arc::clone(&path);
        thread::spawn(move || save_text(&path, &snapshot(&body, Revision::INITIAL)).unwrap())
      })
      .collect();
    for handle in handles {
      handle.join().unwrap();
    }

    let result = fs::read_to_string(&*path).unwrap();
    assert!(bodies.contains(&result), "file must be one whole body, not a mix");
  }

  #[cfg(unix)]
  #[test]
  fn preserves_the_original_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

    save_text(&path, &snapshot("new", Revision::INITIAL)).unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755, "an executable script must stay executable");

    // A mode the process umask would strip if it were only requested at
    // creation time instead of applied to the file.
    let shared = dir.path().join("shared.txt");
    fs::write(&shared, "old").unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o666)).unwrap();

    save_text(&shared, &snapshot("new", Revision::INITIAL)).unwrap();

    let mode = fs::metadata(&shared).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o666, "a group-writable file must stay group-writable");
  }

  #[cfg(unix)]
  #[test]
  fn refuses_to_overwrite_a_read_only_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

    let err = save_text(&path, &snapshot("new", Revision::INITIAL)).unwrap_err();
    let crate::Error::Write { source, .. } = err else {
      panic!("expected a write error");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
  }

  #[cfg(unix)]
  #[test]
  fn writes_through_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    fs::write(&real, "old").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    save_text(&link, &snapshot("new\n", Revision::INITIAL)).unwrap();

    assert!(
      fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
      "saving must not replace the link with a regular file"
    );
    assert_eq!(fs::read_to_string(&real).unwrap(), "new\n");
  }

  /// The lock is what keeps two savers from interleaving, so hold it and watch
  /// a save wait: without it the file changes during the sleep.
  #[test]
  fn a_save_waits_for_the_path_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let lock = lock_for(&resolve(&path));
    let held = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let gate = Arc::new(Barrier::new(2));

    let saver = {
      let path = path.clone();
      let gate = Arc::clone(&gate);
      thread::spawn(move || {
        gate.wait();
        save_text(&path, &snapshot("new", Revision::INITIAL)).unwrap();
      })
    };

    gate.wait();
    thread::sleep(Duration::from_millis(200));
    assert_eq!(fs::read_to_string(&path).unwrap(), "old", "the save must wait its turn");

    drop(held);
    saver.join().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
  }
  #[test]
  fn saved_fingerprint_matches_the_file_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");

    let saved = save_text(&path, &snapshot("new\n", Revision::INITIAL)).unwrap();

    assert_eq!(saved.fingerprint, Fingerprint::of(&path).unwrap());
  }
}
