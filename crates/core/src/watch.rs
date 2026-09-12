//! Noticing when a document changes on disk.

use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// A watch on one file. Dropping it stops the watcher.
pub struct FileWatch {
  _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for FileWatch {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("FileWatch")
  }
}

impl FileWatch {
  /// Call `on_change` whenever `path` is created, written, removed, or
  /// renamed. The parent directory is watched so replace-by-rename is seen.
  /// `on_change` runs on the watcher's thread; keep it to a channel send.
  pub fn new(path: &Path, on_change: impl Fn() + Send + 'static) -> Result<Self, Error> {
    let watch_err = |source| Error::Watch { path: path.to_path_buf(), source };
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
    let file_name = target.file_name().map(std::ffi::OsStr::to_owned);
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
      let Ok(event) = result else {
        return;
      };
      if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)) {
        return;
      }
      let hit = event
        .paths
        .iter()
        .any(|event_path| event_path == &target || event_path.file_name() == file_name.as_deref());
      if hit {
        on_change();
      }
    })
    .map_err(watch_err)?;
    watcher.watch(&parent, RecursiveMode::NonRecursive).map_err(watch_err)?;
    Ok(Self { _watcher: watcher })
  }
}

/// Cheap identity of a file's current contents: size plus modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
  len: u64,
  modified: Option<SystemTime>,
}

impl Fingerprint {
  /// Read the fingerprint of `path` from its metadata.
  pub fn of(path: &Path) -> std::io::Result<Self> {
    let meta = std::fs::metadata(path)?;
    Ok(Self {
      len: meta.len(),
      modified: meta.modified().ok(),
    })
  }
  /// Read the fingerprint of an open file from its metadata.
  pub fn of_file(file: &File) -> std::io::Result<Self> {
    Ok(Self::from_metadata(&file.metadata()?))
  }

  /// Build from metadata already read from an open handle.
  pub fn from_metadata(meta: &std::fs::Metadata) -> Self {
    Self {
      len: meta.len(),
      modified: meta.modified().ok(),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::sync::mpsc;
  use std::time::Duration;

  use super::{FileWatch, Fingerprint};

  #[test]
  fn a_write_to_the_watched_file_fires_the_callback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let (tx, rx) = mpsc::channel();
    let _watch = FileWatch::new(&path, move || {
      let _ = tx.send(());
    })
    .unwrap();

    // Give the platform watcher a moment to register before we mutate.
    std::thread::sleep(Duration::from_millis(200));
    fs::write(&path, "new").unwrap();

    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "no change event within 5 s");
  }

  #[cfg(unix)]
  #[test]
  fn a_write_through_a_symlinked_directory_fires() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    let link_dir = dir.path().join("linkdir");
    fs::create_dir(&real_dir).unwrap();
    let real_path = real_dir.join("a.txt");
    fs::write(&real_path, "old").unwrap();
    symlink(&real_dir, &link_dir).unwrap();
    let watched_path = link_dir.join("a.txt");
    let (tx, rx) = mpsc::channel();
    let _watch = FileWatch::new(&watched_path, move || {
      let _ = tx.send(());
    })
    .unwrap();

    std::thread::sleep(Duration::from_millis(200));
    fs::write(&real_path, "new").unwrap();

    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "no change event within 5 s");
  }

  #[test]
  fn a_write_to_a_sibling_does_not_fire() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let (tx, rx) = mpsc::channel();
    let _watch = FileWatch::new(&path, move || {
      let _ = tx.send(());
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    while rx.try_recv().is_ok() {}

    fs::write(dir.path().join("b.txt"), "other").unwrap();

    assert!(
      rx.recv_timeout(Duration::from_millis(800)).is_err(),
      "sibling write leaked through"
    );
  }

  #[test]
  fn dropping_the_watch_stops_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let (tx, rx) = mpsc::channel();
    let watch = FileWatch::new(&path, move || {
      let _ = tx.send(());
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    drop(watch);
    std::thread::sleep(Duration::from_millis(200));
    while rx.try_recv().is_ok() {}

    fs::write(&path, "new").unwrap();

    assert!(
      rx.recv_timeout(Duration::from_millis(800)).is_err(),
      "write after drop leaked through"
    );
  }

  #[test]
  fn fingerprint_changes_with_content_and_matches_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let first = Fingerprint::of(&path).unwrap();
    assert_eq!(first, Fingerprint::of(&path).unwrap());

    std::thread::sleep(Duration::from_millis(20));
    fs::write(&path, "longer content").unwrap();

    assert_ne!(first, Fingerprint::of(&path).unwrap());

    let encoded = serde_json::to_vec(&first).unwrap();
    assert_eq!(serde_json::from_slice::<Fingerprint>(&encoded).unwrap(), first);
  }

  #[test]
  fn watching_a_missing_parent_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope").join("a.txt");
    assert!(FileWatch::new(&path, || {}).is_err());
  }
}
