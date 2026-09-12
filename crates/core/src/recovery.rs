//! Drafts of unsaved work. One JSON file per session under the OpenIt data
//! directory, plus a blob file holding an image draft's pixels; the original
//! document is never touched from here.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::document::ImageFormat;
use crate::error::Error;
use crate::raster::Transform;
use crate::save::write_atomic;
use crate::session::SessionId;
use crate::watch::Fingerprint;

/// Maximum number of bytes read from one persisted draft.
pub const MAX_DRAFT_BYTES: u64 = 64 * 1024 * 1024;

/// Everything needed to bring an unsaved document back after a relaunch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
  /// Session the draft belongs to.
  pub session: SessionId,
  /// Source file, or `None` for a document that was never saved.
  #[serde(default)]
  pub path: Option<PathBuf>,
  /// Fingerprint of the source file when this draft was captured, or `None` for an untitled document.
  #[serde(default)]
  pub disk: Option<Fingerprint>,
  /// Full buffer text at checkpoint time; empty for an image document.
  pub text: String,
  /// Caret position as a UTF-8 byte offset into `text`.
  #[serde(default)]
  pub cursor: usize,
  /// Image content, when the draft is an image document. Its pixels live in
  /// the session's blob file beside this draft.
  #[serde(default)]
  pub image: Option<ImageDraft>,
  /// Manual schema pick (local file or URL) restored with this draft.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub schema: Option<String>,
}

/// The image half of a draft. The blob holds PNG bytes for a clipboard image
/// and the original file bytes for a document opened from disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDraft {
  /// Format of the bytes in the blob.
  pub format: ImageFormat,
  /// Rotation and flips the user applied.
  pub transform: Transform,
}

/// Directory of draft files.
#[derive(Debug, Clone)]
pub struct RecoveryStore {
  dir: PathBuf,
}

impl RecoveryStore {
  /// The platform default: `<data_local_dir>/openit/drafts`.
  pub fn default_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("openit").join("drafts"))
  }

  /// Open (creating if needed) the store at `dir`.
  pub fn open(dir: impl Into<PathBuf>) -> Result<Self, Error> {
    let dir = dir.into();
    fs::create_dir_all(&dir).map_err(|source| Error::Recovery { path: dir.clone(), source })?;
    Ok(Self { dir })
  }

  fn file_for(&self, session: SessionId) -> PathBuf {
    self.dir.join(format!("{session}.json"))
  }

  fn blob_for(&self, session: SessionId) -> PathBuf {
    self.dir.join(format!("{session}.blob"))
  }

  /// Write the image bytes belonging to `session`, replacing any earlier blob.
  pub fn write_blob(&self, session: SessionId, bytes: &[u8]) -> Result<(), Error> {
    let path = self.blob_for(session);
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DRAFT_BYTES {
      return Err(oversized(path));
    }
    write_atomic(&path, &self.dir, |_| Ok(()), |w| w.write_all(bytes))
      .map_err(|source| Error::Recovery { path, source })?;
    Ok(())
  }

  /// Read the image bytes belonging to `session`.
  pub fn read_blob(&self, session: SessionId) -> Result<Vec<u8>, Error> {
    let path = self.blob_for(session);
    let file = fs::File::open(&path).map_err(|source| Error::Recovery { path: path.clone(), source })?;
    let mut bytes = Vec::new();
    file
      .take(MAX_DRAFT_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|source| Error::Recovery { path: path.clone(), source })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DRAFT_BYTES {
      return Err(oversized(path));
    }
    Ok(bytes)
  }

  /// Write `draft`, replacing any earlier checkpoint for the same session.
  pub fn checkpoint(&self, draft: &Draft) -> Result<(), Error> {
    let path = self.file_for(draft.session);
    let json = serde_json::to_vec(draft).map_err(|source| Error::DraftFormat { path: path.clone(), source })?;
    if u64::try_from(json.len()).unwrap_or(u64::MAX) > MAX_DRAFT_BYTES {
      return Err(oversized(path));
    }
    write_atomic(&path, &self.dir, |_| Ok(()), |w| w.write_all(&json))
      .map_err(|source| Error::Recovery { path: path.clone(), source })?;
    tracing::debug!(session = %draft.session, "checkpoint written");
    Ok(())
  }

  /// Delete the draft for `session`, and its blob when it has one. A missing
  /// file is not an error.
  pub fn remove(&self, session: SessionId) -> Result<(), Error> {
    remove_file(self.file_for(session))?;
    remove_file(self.blob_for(session))
  }

  /// Every readable draft, oldest checkpoint first. Unreadable files are
  /// logged and skipped so one corrupt draft cannot hide the others.
  pub fn list(&self) -> Result<Vec<Draft>, Error> {
    let entries = fs::read_dir(&self.dir).map_err(|source| Error::Recovery { path: self.dir.clone(), source })?;
    let mut drafts: Vec<(SystemTime, Draft)> = Vec::new();
    for entry in entries {
      let entry = entry.map_err(|source| Error::Recovery { path: self.dir.clone(), source })?;
      let path = entry.path();
      if path.extension().and_then(|e| e.to_str()) != Some("json") {
        continue;
      }
      match read_draft(&path) {
        Ok(draft) if draft.image.is_some() && !self.blob_for(draft.session).is_file() => {
          tracing::warn!(session = %draft.session, "skipping an image draft whose pixels are missing");
        },
        Ok(draft) => {
          let modified = match entry.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(error) => {
              tracing::debug!(%error, path = %path.display(), "could not read draft modification time");
              SystemTime::UNIX_EPOCH
            },
          };
          drafts.push((modified, draft));
        },
        Err(error) => tracing::warn!(%error, path = %path.display(), "skipping unreadable draft"),
      }
    }
    drafts.sort_by_key(|(modified, _)| *modified);
    Ok(drafts.into_iter().map(|(_, d)| d).collect())
  }
}

/// Delete one file, treating a missing file as done.
fn remove_file(path: PathBuf) -> Result<(), Error> {
  match fs::remove_file(&path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(source) => Err(Error::Recovery { path, source }),
  }
}

/// The refusal shared by drafts and blobs over [`MAX_DRAFT_BYTES`].
fn oversized(path: PathBuf) -> Error {
  Error::Recovery {
    path,
    source: io::Error::new(io::ErrorKind::InvalidData, "draft exceeds size limit"),
  }
}

fn read_draft(path: &Path) -> Result<Draft, Error> {
  let file = fs::File::open(path).map_err(|source| Error::Recovery { path: path.to_path_buf(), source })?;
  let metadata = file
    .metadata()
    .map_err(|source| Error::Recovery { path: path.to_path_buf(), source })?;
  if !metadata.is_file() {
    return Err(Error::Recovery {
      path: path.to_path_buf(),
      source: io::Error::new(io::ErrorKind::InvalidData, "draft is not a regular file"),
    });
  }

  let mut bytes = Vec::new();
  file
    .take(MAX_DRAFT_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|source| Error::Recovery { path: path.to_path_buf(), source })?;
  let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
  if size > MAX_DRAFT_BYTES {
    return Err(Error::Recovery {
      path: path.to_path_buf(),
      source: io::Error::new(io::ErrorKind::InvalidData, "draft exceeds size limit"),
    });
  }

  serde_json::from_slice(&bytes).map_err(|source| Error::DraftFormat { path: path.to_path_buf(), source })
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::{Path, PathBuf};

  use super::{Draft, ImageDraft, MAX_DRAFT_BYTES, RecoveryStore};
  use crate::document::ImageFormat;
  use crate::raster::Transform;
  use crate::session::SessionId;

  fn draft(text: &str) -> Draft {
    Draft {
      session: SessionId::new(),
      path: Some(PathBuf::from("/tmp/notes.md")),
      text: text.to_owned(),
      disk: None,
      cursor: 3,
      image: None,
      schema: None,
    }
  }

  fn image_draft(session: SessionId) -> Draft {
    Draft {
      session,
      path: None,
      text: String::new(),
      disk: None,
      cursor: 0,
      image: Some(ImageDraft {
        format: ImageFormat::Png,
        transform: Transform::IDENTITY.rotate_cw(),
      }),
      schema: None,
    }
  }

  #[test]
  fn an_image_draft_round_trips_with_its_blob() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let session = SessionId::new();
    let draft = image_draft(session);

    store.write_blob(session, b"png bytes").unwrap();
    store.checkpoint(&draft).unwrap();

    assert_eq!(store.list().unwrap(), vec![draft]);
    assert_eq!(store.read_blob(session).unwrap(), b"png bytes");

    store.remove(session).unwrap();
    assert!(store.read_blob(session).is_err());
    assert!(store.list().unwrap().is_empty());
  }

  #[test]
  fn an_image_draft_without_its_blob_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();

    store.checkpoint(&image_draft(SessionId::new())).unwrap();

    assert!(store.list().unwrap().is_empty());
  }

  #[test]
  fn an_oversized_blob_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let session = SessionId::new();

    let error = store
      .write_blob(session, &vec![0u8; usize::try_from(MAX_DRAFT_BYTES).unwrap() + 1])
      .unwrap_err();

    assert!(error.to_string().contains("limit"), "got {error}");
  }

  #[test]
  fn a_text_draft_written_before_images_still_parses() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let session = SessionId::new();
    fs::write(
      dir.path().join(format!("{session}.json")),
      format!(r#"{{"session":"{session}","text":"hi","cursor":0}}"#),
    )
    .unwrap();

    let drafts = store.list().unwrap();

    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "hi");
    assert!(drafts[0].image.is_none());
    assert!(drafts[0].schema.is_none());
  }

  #[test]
  fn a_schema_pick_round_trips_on_the_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let mut d = draft(
      "{}
",
    );
    d.schema = Some("https://www.schemastore.org/package.json".to_owned());

    store.checkpoint(&d).unwrap();

    assert_eq!(store.list().unwrap(), vec![d]);
  }

  #[test]
  fn checkpoint_then_list_round_trips_the_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path().join("drafts")).unwrap();
    let d = draft("# unsaved\n");

    store.checkpoint(&d).unwrap();

    assert_eq!(store.list().unwrap(), vec![d]);
  }

  #[test]
  fn list_orders_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let first = draft("first");
    let second = draft("second");

    store.checkpoint(&first).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    store.checkpoint(&second).unwrap();

    assert_eq!(store.list().unwrap(), vec![first, second]);
  }

  #[test]
  fn list_skips_an_oversized_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let good = draft("good");
    store.checkpoint(&good).unwrap();
    let oversized = fs::File::create(dir.path().join("oversized.json")).unwrap();
    oversized.set_len(MAX_DRAFT_BYTES + 1).unwrap();

    assert_eq!(store.list().unwrap(), vec![good]);
  }

  #[test]
  fn checkpoint_refuses_an_oversized_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let text = "x".repeat(usize::try_from(MAX_DRAFT_BYTES).unwrap());
    let d = draft(&text);
    let path = dir.path().join(format!("{}.json", d.session));

    let result = store.checkpoint(&d);

    assert!(matches!(
      result,
      Err(crate::error::Error::Recovery { source, .. })
        if source.kind() == std::io::ErrorKind::InvalidData
          && source.to_string() == "draft exceeds size limit"
    ));
    assert!(!path.exists());
  }

  #[test]
  fn a_second_checkpoint_replaces_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let mut d = draft("one");
    store.checkpoint(&d).unwrap();
    d.text = "two".to_owned();

    store.checkpoint(&d).unwrap();

    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].text, "two");
  }

  #[test]
  fn remove_deletes_the_draft_and_tolerates_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let d = draft("x");
    store.checkpoint(&d).unwrap();

    store.remove(d.session).unwrap();
    store.remove(d.session).unwrap();

    assert!(store.list().unwrap().is_empty());
  }

  #[test]
  fn list_skips_unreadable_files_and_keeps_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let d = draft("good");
    store.checkpoint(&d).unwrap();
    fs::write(dir.path().join("broken.json"), "{ not json").unwrap();

    assert_eq!(store.list().unwrap(), vec![d]);
  }

  #[test]
  fn checkpoint_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    store.checkpoint(&draft("x")).unwrap();

    let names: Vec<String> = fs::read_dir(dir.path())
      .unwrap()
      .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    assert_eq!(names.len(), 1);
    assert_eq!(Path::new(&names[0]).extension().and_then(|ext| ext.to_str()), Some("json"));
  }

  #[test]
  fn open_fails_when_the_directory_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "x").unwrap();

    assert!(RecoveryStore::open(file.join("drafts")).is_err());
  }
}
