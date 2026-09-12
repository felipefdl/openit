//! On-disk cache for remote resources.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::PathBuf;
use std::time::SystemTime;

use sha2::{Digest as _, Sha256};
use url::Url;

use crate::error::Error;
use crate::save::write_atomic;

/// Largest resource that can be loaded from the cache.
pub const MAX_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest total size of the resource cache.
pub const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Persistent cache for resources fetched from remote URLs.
pub struct ResourceCache {
  dir: PathBuf,
  max_cache_bytes: u64,
}

impl ResourceCache {
  /// Return the platform cache directory for OpenIt resources.
  pub fn default_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("openit").join("resources"))
  }

  /// Open the cache at `dir`, creating the directory and its parents if needed.
  pub fn open(dir: impl Into<PathBuf>) -> Result<Self, Error> {
    Self::open_with_cap(dir.into(), MAX_CACHE_BYTES)
  }

  /// Read the resource cached for `url`, if it exists and fits the resource limit.
  pub fn get(&self, url: &Url) -> Result<Option<Vec<u8>>, Error> {
    let path = self.path_for(url);
    let path_metadata = match fs::metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(source) => return Err(Error::Cache { path, source }),
    };
    if !path_metadata.is_file() {
      tracing::debug!(path = %path.display(), "ignoring non-regular cache resource");
      return Ok(None);
    }
    let file = match File::open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(source) => return Err(Error::Cache { path, source }),
    };
    let metadata = file.metadata().map_err(|source| Error::Cache { path: path.clone(), source })?;
    if !metadata.is_file() {
      return Err(Error::Cache {
        path,
        source: io::Error::new(io::ErrorKind::InvalidInput, "cache entry is not a regular file"),
      });
    }
    if metadata.len() > MAX_RESOURCE_BYTES {
      tracing::debug!(path = %path.display(), limit = MAX_RESOURCE_BYTES, "ignoring oversized cache resource");
      return Ok(None);
    }

    let capacity = usize::try_from(metadata.len().min(MAX_RESOURCE_BYTES)).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    let bytes_read = file
      .take(MAX_RESOURCE_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|source| Error::Cache { path: path.clone(), source })?;
    let bytes_read = u64::try_from(bytes_read).unwrap_or(u64::MAX);
    if bytes_read > MAX_RESOURCE_BYTES {
      tracing::debug!(path = %path.display(), limit = MAX_RESOURCE_BYTES, "ignoring oversized cache resource");
      return Ok(None);
    }
    Ok(Some(bytes))
  }

  /// Store `bytes` under the URL's stable cache key.
  pub fn put(&self, url: &Url, bytes: &[u8]) -> Result<(), Error> {
    let path = self.path_for(url);
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > MAX_RESOURCE_BYTES {
      return Err(Error::Cache {
        path,
        source: io::Error::new(io::ErrorKind::InvalidData, "resource exceeds size limit"),
      });
    }
    write_atomic(&path, &self.dir, |_| Ok(()), |writer| writer.write_all(bytes))
      .map_err(|source| Error::Cache { path: path.clone(), source })?;
    tracing::debug!(path = %path.display(), bytes = bytes.len(), "resource cached");
    Ok(())
  }

  /// Remove the resource cached for `url`, if it exists.
  pub fn remove(&self, url: &Url) -> Result<(), Error> {
    let path = self.path_for(url);
    match fs::remove_file(&path) {
      Ok(()) => Ok(()),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
      Err(source) => Err(Error::Cache { path, source }),
    }
  }

  /// Remove oldest cache entries until the total size is within the cache cap.
  pub fn prune(&self) -> Result<(), Error> {
    let entries = fs::read_dir(&self.dir).map_err(|source| Error::Cache { path: self.dir.clone(), source })?;
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in entries {
      let entry = entry.map_err(|source| Error::Cache { path: self.dir.clone(), source })?;
      let path = entry.path();
      let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(source) => {
          tracing::debug!(%source, path = %path.display(), "could not inspect cache entry");
          continue;
        },
      };
      if !metadata.is_file() {
        continue;
      }
      let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
      let size = metadata.len();
      total = total.saturating_add(size);
      files.push((modified, path, size));
    }

    files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, path, size) in files {
      if total <= self.max_cache_bytes {
        break;
      }
      match fs::remove_file(&path) {
        Ok(()) => {
          total = total.saturating_sub(size);
          tracing::debug!(path = %path.display(), bytes = size, "pruned cached resource");
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
          total = total.saturating_sub(size);
        },
        Err(source) => return Err(Error::Cache { path, source }),
      }
    }
    Ok(())
  }

  #[cfg(test)]
  fn with_cap(dir: impl Into<PathBuf>, max_cache_bytes: u64) -> Self {
    Self::open_with_cap(dir.into(), max_cache_bytes).unwrap()
  }

  fn open_with_cap(dir: PathBuf, max_cache_bytes: u64) -> Result<Self, Error> {
    fs::create_dir_all(&dir).map_err(|source| Error::Cache { path: dir.clone(), source })?;
    Ok(Self { dir, max_cache_bytes })
  }

  fn path_for(&self, url: &Url) -> PathBuf {
    let digest = Sha256::digest(url.as_str().as_bytes());
    self.dir.join(format!("{digest:x}"))
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::thread;
  use std::time::Duration;

  use url::Url;

  use super::{MAX_RESOURCE_BYTES, ResourceCache};

  #[test]
  fn put_then_get_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let url = Url::parse("https://cdn.example.org/image.png").unwrap();

    cache.put(&url, b"image bytes").unwrap();

    assert_eq!(cache.get(&url).unwrap(), Some(b"image bytes".to_vec()));
  }

  #[test]
  fn get_of_an_unknown_url_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let url = Url::parse("https://cdn.example.org/missing.png").unwrap();

    assert_eq!(cache.get(&url).unwrap(), None);
  }

  #[test]
  fn keys_differ_by_url() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let first = Url::parse("https://cdn.example.org/first.png").unwrap();
    let second = Url::parse("https://cdn.example.org/second.png").unwrap();

    cache.put(&first, b"same bytes").unwrap();
    cache.put(&second, b"same bytes").unwrap();

    let mut keys: Vec<_> = fs::read_dir(dir.path())
      .unwrap()
      .map(|entry| entry.unwrap().file_name())
      .collect();
    keys.sort_unstable();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys.first(), keys.last());
  }

  #[test]
  fn prune_removes_oldest_past_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::with_cap(dir.path().to_path_buf(), 5);
    let oldest = Url::parse("https://cdn.example.org/oldest.png").unwrap();
    let newest = Url::parse("https://cdn.example.org/newest.png").unwrap();

    cache.put(&oldest, b"old").unwrap();
    thread::sleep(Duration::from_millis(20));
    cache.put(&newest, b"new").unwrap();

    cache.prune().unwrap();

    assert_eq!(cache.get(&oldest).unwrap(), None);
    assert_eq!(cache.get(&newest).unwrap(), Some(b"new".to_vec()));
  }

  #[test]
  fn an_oversized_cache_file_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let url = Url::parse("https://cdn.example.org/oversized.png").unwrap();
    cache.put(&url, b"small").unwrap();

    let path = fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap().path();
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(MAX_RESOURCE_BYTES + 1).unwrap();

    assert_eq!(cache.get(&url).unwrap(), None);
  }

  #[test]
  fn put_rejects_resources_over_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let url = Url::parse("https://cdn.example.org/oversized-put.png").unwrap();
    let bytes = vec![0; usize::try_from(MAX_RESOURCE_BYTES + 1).unwrap()];

    let error = cache.put(&url, &bytes).unwrap_err();

    assert!(
      matches!(error, crate::error::Error::Cache { source, .. } if source.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_eq!(cache.get(&url).unwrap(), None);
  }
  #[cfg(unix)]
  #[test]
  fn a_fifo_at_a_cache_key_is_ignored_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ResourceCache::open(dir.path().to_path_buf()).unwrap();
    let url = Url::parse("https://cdn.example.org/fifo.png").unwrap();
    let path = cache.path_for(&url);
    assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());

    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || sender.send(cache.get(&url)).unwrap());

    let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    handle.join().unwrap();
    assert_eq!(result, None);
  }
}
