use std::sync::Arc;

use gpui_kit::Global;
use openit_core::cache::ResourceCache;
use tempfile::TempDir;

/// Application-wide cache for fetched remote resources.
pub(crate) struct ResourceCacheHandle(
  pub Arc<ResourceCache>,
  #[expect(
    dead_code,
    reason = "retains the fallback temporary directory for the handle lifetime"
  )]
  pub(crate) Option<TempDir>,
);

impl Global for ResourceCacheHandle {}

impl ResourceCacheHandle {
  /// Open the persistent cache, falling back to a temporary directory when needed.
  pub(crate) fn open() -> Option<Self> {
    if let Some(dir) = ResourceCache::default_dir() {
      match ResourceCache::open(dir) {
        Ok(cache) => return Some(Self(Arc::new(cache), None)),
        Err(error) => tracing::error!(%error, "resource cache directory unusable"),
      }
    }

    match tempfile::tempdir() {
      Ok(dir) => match ResourceCache::open(dir.path()) {
        Ok(cache) => Some(Self(Arc::new(cache), Some(dir))),
        Err(error) => {
          tracing::error!(%error, "temporary resource cache directory unusable");
          None
        },
      },
      Err(error) => {
        tracing::error!(%error, "could not create temporary resource cache directory");
        None
      },
    }
  }

  #[cfg(test)]
  pub(crate) fn from_temp_dir(dir: TempDir) -> Result<Self, openit_core::Error> {
    let cache = ResourceCache::open(dir.path())?;
    Ok(Self(Arc::new(cache), Some(dir)))
  }
}
