use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use futures::{FutureExt as _, future::Shared};
use gpui_kit::{
  App, AppContext, Context, Entity, ImageCache, ImageCacheError, RenderImage, Resource, Task, Window, hash,
};
use image::{Frame, Rgba, RgbaImage};
use openit_core::cache::MAX_RESOURCE_BYTES;
use openit_core::resource::{self, Decision, DenyReason, DomainFamily, Resolved};

use crate::cache::ResourceCacheHandle;
use crate::fetch::Fetcher;
use crate::image_decode::{self, Decoded};
use crate::settings::AppSettings;

#[cfg(test)]
static RELEASED_IMAGE_COUNT: AtomicUsize = AtomicUsize::new(0);

const _: () = assert!(MAX_RESOURCE_BYTES == image_decode::MAX_IMAGE_BYTES);

/// One permission prompt and all image resources waiting for its answer.
#[derive(Clone)]
pub struct PermissionRequest {
  /// The family being requested.
  pub(crate) family: DomainFamily,
  /// Resources that are waiting for this family.
  pub(crate) waiting: Vec<Resource>,
}

/// Pending remote-resource permissions for one document.
#[derive(Default)]
pub struct PermissionRequests {
  /// Families currently waiting for an answer.
  pub(crate) pending: Vec<PermissionRequest>,
  answers: Vec<PermissionAnswer>,
}

pub(crate) enum PermissionAnswer {
  Allow(DomainFamily),
  Always,
}

impl PermissionRequests {
  /// Add a resource to the prompt for its family, collapsing duplicate families.
  pub fn ask(&mut self, family: DomainFamily, resource: Resource, cx: &mut Context<Self>) {
    if let Some(request) = self.pending.iter_mut().find(|request| request.family == family) {
      if !request.waiting.iter().any(|waiting| waiting == &resource) {
        request.waiting.push(resource);
      }
    } else {
      self.pending.push(PermissionRequest { family, waiting: vec![resource] });
    }
    cx.notify();
  }

  /// Answer one family prompt and notify its owning view.
  pub fn answer_allow(&mut self, family: DomainFamily, cx: &mut Context<Self>) {
    if let Some(index) = self.pending.iter().position(|request| request.family == family) {
      self.pending.remove(index);
      self.answers.push(PermissionAnswer::Allow(family));
      cx.notify();
    }
  }

  /// Answer every pending prompt with the application-wide remote permission.
  pub fn answer_always(&mut self, cx: &mut Context<Self>) {
    if !self.pending.is_empty() {
      self.pending.clear();
      self.answers.push(PermissionAnswer::Always);
      cx.notify();
    }
  }
  /// Whether no family is waiting for permission.
  pub const fn is_empty(&self) -> bool {
    self.pending.is_empty()
  }

  pub(crate) fn dismiss_family(&mut self, family: &DomainFamily, cx: &mut Context<Self>) {
    self.pending.retain(|request| &request.family != family);
    cx.notify();
  }

  pub(crate) fn dismiss_all(&mut self, cx: &mut Context<Self>) {
    self.pending.clear();
    cx.notify();
  }

  pub(crate) fn take_answers(&mut self) -> Vec<PermissionAnswer> {
    std::mem::take(&mut self.answers)
  }
}

/// A placeholder kind retained for resources that are intentionally not rasterized.
pub enum PlaceholderKind {
  /// The resource was denied by the resolver or permission policy.
  Denied(DenyReason),

  /// SVG rendering is disabled for document resources.
  Svg,
}

/// One document image-cache entry.
pub enum Entry {
  /// A local or remote image is being read and decoded.
  Loading(Shared<Task<Result<LoadResult, ImageCacheError>>>),
  /// A decoded raster image.
  Ready(Arc<RenderImage>),
  /// The resource failed to read or decode.
  Failed(ImageCacheError),
  /// The resource is waiting for permission from this family.
  AwaitingPermission(DomainFamily),
  /// The resource renders a shared placeholder.
  Placeholder(PlaceholderKind),
}
#[derive(Clone)]
pub(crate) enum LoadResult {
  Image(Arc<RenderImage>),
  CacheMiss(Resolved),
}

/// Image cache and permission state for one Markdown document.
pub struct DocumentImageCache {
  pub(crate) base_dir: Option<PathBuf>,
  pub(crate) entries: HashMap<u64, Entry>,
  pub(crate) requests: Entity<PermissionRequests>,
  resources: HashMap<u64, Resource>,
  notifications: HashMap<u64, Task<()>>,
  placeholder: Option<Arc<RenderImage>>,
  placeholder_slot: Arc<OnceLock<Arc<RenderImage>>>,
}
impl DocumentImageCache {
  /// Create a cache for a document base directory.
  pub fn new<C: AppContext>(
    base_dir: Option<PathBuf>,
    requests: Entity<PermissionRequests>,
    cx: &mut C,
  ) -> Entity<Self> {
    cx.new(|_| Self {
      base_dir,
      entries: HashMap::new(),
      requests,
      resources: HashMap::new(),
      notifications: HashMap::new(),
      placeholder: None,
      placeholder_slot: Arc::new(OnceLock::new()),
    })
  }

  pub(crate) fn observe_release(entity: &Entity<Self>, cx: &App) {
    cx.observe_release(entity, Self::release).detach();
  }

  /// Retry resources waiting for one permission family.
  pub fn retry_family(&mut self, family: &DomainFamily, window: &Window, cx: &mut App) {
    let hashes = self
      .entries
      .iter()
      .filter_map(|(hash, entry)| match entry {
        Entry::AwaitingPermission(waiting) if waiting == family => Some(*hash),
        _ => None,
      })
      .collect::<Vec<_>>();
    self.retry_hashes(hashes, window, cx);
  }

  /// Retry every resource waiting for permission and dismiss requests that are now allowed.
  pub fn retry_all(&mut self, window: &Window, cx: &mut App) {
    let mut families = Vec::new();
    let hashes = self
      .entries
      .iter()
      .filter_map(|(hash, entry)| match entry {
        Entry::AwaitingPermission(family) => {
          if !families.iter().any(|waiting| waiting == family) {
            families.push(family.clone());
          }
          Some(*hash)
        },
        _ => None,
      })
      .collect::<Vec<_>>();
    self.retry_hashes(hashes, window, cx);
    for family in families {
      let still_waiting = self
        .entries
        .values()
        .any(|entry| matches!(entry, Entry::AwaitingPermission(waiting) if waiting == &family));
      if !still_waiting {
        self.requests.update(cx, |requests, cx| requests.dismiss_family(&family, cx));
      }
    }
  }

  /// Load one resource from the cache.
  pub fn load(
    &mut self,
    resource: &Resource,
    window: &Window,
    cx: &mut App,
  ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    let resource_hash = hash(resource);
    if self.entries.contains_key(&resource_hash) {
      return self.load_existing(resource_hash, window, cx);
    }

    self.resources.insert(resource_hash, resource.clone());
    let reference = match resource {
      Resource::Uri(uri) => uri.to_string(),
      _ => {
        return Some(Ok(self.insert_denied(resource_hash, DenyReason::Malformed)));
      },
    };
    let resolved = resource::resolve(&reference, self.base_dir.as_deref());
    match resolved {
      Resolved::Remote(url) => {
        self.start_cache_probe(resource_hash, Resolved::Remote(url), window, cx);
        None
      },
      resolved => self.apply_decision(resource_hash, resolved, window, cx),
    }
  }

  fn load_existing(
    &mut self,
    resource_hash: u64,
    window: &Window,
    cx: &mut App,
  ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    let result = match self.entries.get_mut(&resource_hash) {
      Some(Entry::Loading(task)) => task.clone().now_or_never(),
      Some(Entry::Ready(image)) => return Some(Ok(image.clone())),
      Some(Entry::Failed(error)) => return Some(Err(error.clone())),
      Some(Entry::AwaitingPermission(_)) | None => return None,
      Some(Entry::Placeholder(kind)) => {
        match kind {
          PlaceholderKind::Denied(reason) => {
            let _ = reason;
          },
          PlaceholderKind::Svg => {},
        }
        return self.placeholder.clone().map(Ok);
      },
    }?;
    match result {
      Ok(LoadResult::CacheMiss(resolved)) => self.apply_decision(resource_hash, resolved, window, cx),
      Ok(LoadResult::Image(image)) => {
        let placeholder = self.placeholder.clone().or_else(|| self.placeholder_slot.get().cloned());
        let is_svg = placeholder.as_ref().is_some_and(|placeholder| Arc::ptr_eq(placeholder, &image));
        if is_svg {
          self.placeholder = placeholder;
          self.entries.insert(resource_hash, Entry::Placeholder(PlaceholderKind::Svg));
        } else {
          self.entries.insert(resource_hash, Entry::Ready(image.clone()));
        }
        Some(Ok(image))
      },
      Err(error) => {
        self.entries.insert(resource_hash, Entry::Failed(error.clone()));
        Some(Err(error))
      },
    }
  }

  fn apply_decision(
    &mut self,
    resource_hash: u64,
    resolved: Resolved,
    window: &Window,
    cx: &mut App,
  ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    match resource::decide(&resolved, &cx.global::<AppSettings>().0) {
      Decision::Deny(reason) => Some(Ok(self.insert_denied(resource_hash, reason))),
      Decision::Ask(family) => {
        let Some(resource) = self.resources.get(&resource_hash).cloned() else {
          return Some(Err(other_error("resource was dropped before permission request")));
        };
        let requests = self.requests.clone();
        requests.update(cx, |requests, cx| requests.ask(family.clone(), resource, cx));
        self.entries.insert(resource_hash, Entry::AwaitingPermission(family));
        None
      },
      Decision::Allow => {
        let family = match &resolved {
          Resolved::Remote(url) => url.host_str().map(DomainFamily::of_host),
          Resolved::Local(_) | Resolved::Denied(_) => None,
        };
        self.start_load(resource_hash, resolved, family, window, cx);
        None
      },
    }
  }

  fn start_cache_probe(&mut self, resource_hash: u64, resolved: Resolved, window: &Window, cx: &App) {
    let Resolved::Remote(url) = resolved else {
      return;
    };
    let cache = cx.try_global::<ResourceCacheHandle>().map(|cache| Arc::clone(&cache.0));
    let placeholder_slot = Arc::clone(&self.placeholder_slot);
    let task = cx
      .background_spawn(async move {
        let Some(cache) = cache else {
          return Ok(LoadResult::CacheMiss(Resolved::Remote(url)));
        };
        match cache.get(&url) {
          Ok(Some(bytes)) => match decode_bytes(&bytes, &placeholder_slot) {
            Ok(image) => Ok(LoadResult::Image(image)),
            Err(error) => {
              if let Err(remove_error) = cache.remove(&url) {
                tracing::warn!(%remove_error, url = %url, "could not remove invalid cached resource");
              }
              tracing::debug!(%error, url = %url, "cached resource could not be decoded");
              Ok(LoadResult::CacheMiss(Resolved::Remote(url)))
            },
          },
          Ok(None) => Ok(LoadResult::CacheMiss(Resolved::Remote(url))),
          Err(error) => {
            tracing::warn!(%error, url = %url, "could not read cached resource");
            Ok(LoadResult::CacheMiss(Resolved::Remote(url)))
          },
        }
      })
      .shared();
    self.start_task(resource_hash, task, window, cx);
  }

  fn start_load(
    &mut self,
    resource_hash: u64,
    resolved: Resolved,
    family: Option<DomainFamily>,
    window: &Window,
    cx: &App,
  ) {
    let task = match resolved {
      Resolved::Local(path) => {
        let placeholder_slot = Arc::clone(&self.placeholder_slot);
        cx.background_spawn(async move { decode_local(&path, &placeholder_slot).map(LoadResult::Image) })
      },
      Resolved::Remote(url) => {
        let Some(family) = family else {
          self
            .entries
            .insert(resource_hash, Entry::Failed(other_error("remote URL has no host")));
          return;
        };
        let fetcher = Arc::clone(&cx.global::<Fetcher>().0);
        let cache = cx.try_global::<ResourceCacheHandle>().map(|cache| Arc::clone(&cache.0));
        let placeholder_slot = Arc::clone(&self.placeholder_slot);
        cx.background_spawn(async move {
          let bytes = fetcher
            .fetch(url.clone(), family)
            .await
            .map_err(|error| other_error(error.to_string()))?;
          let image = decode_bytes(&bytes, &placeholder_slot)?;
          if let Some(cache) = cache
            && let Err(error) = cache.put(&url, &bytes)
          {
            tracing::warn!(%error, url = %url, "could not cache fetched resource");
          }
          Ok(LoadResult::Image(image))
        })
      },
      Resolved::Denied(reason) => {
        self
          .entries
          .insert(resource_hash, Entry::Placeholder(PlaceholderKind::Denied(reason)));
        return;
      },
    }
    .shared();
    self.start_task(resource_hash, task, window, cx);
  }

  fn start_task(
    &mut self,
    resource_hash: u64,
    task: Shared<Task<Result<LoadResult, ImageCacheError>>>,
    window: &Window,
    cx: &App,
  ) {
    self.entries.insert(resource_hash, Entry::Loading(task.clone()));
    #[cfg(not(test))]
    let entity = window.current_view();
    #[cfg(test)]
    let entity = self.requests.entity_id();
    let notification = window.spawn(cx, async move |cx| {
      let _ = task.await;
      cx.on_next_frame(move |_, cx| cx.notify(entity));
    });
    self.notifications.insert(resource_hash, notification);
  }
  fn retry_hashes(&mut self, hashes: Vec<u64>, window: &Window, cx: &mut App) {
    for resource_hash in hashes {
      let Some(resource) = self.resources.get(&resource_hash).cloned() else {
        continue;
      };
      self.entries.remove(&resource_hash);
      self.notifications.remove(&resource_hash);
      let _ = self.load(&resource, window, cx);
    }
  }

  fn insert_denied(&mut self, resource_hash: u64, reason: DenyReason) -> Arc<RenderImage> {
    let image = self.placeholder_image();
    self
      .entries
      .insert(resource_hash, Entry::Placeholder(PlaceholderKind::Denied(reason)));
    image
  }

  fn placeholder_image(&mut self) -> Arc<RenderImage> {
    let image = self.placeholder_slot.get_or_init(make_placeholder).clone();
    self.placeholder = Some(image.clone());
    image
  }

  pub(crate) fn release(&mut self, cx: &mut App) {
    for entry in std::mem::take(&mut self.entries).into_values() {
      if let Entry::Ready(image) = entry {
        cx.drop_image(image, None);
        #[cfg(test)]
        RELEASED_IMAGE_COUNT.fetch_add(1, Ordering::Relaxed);
      }
    }
    self.resources.clear();
    self.notifications.clear();
    let placeholder = self.placeholder.take().or_else(|| self.placeholder_slot.get().cloned());
    if let Some(image) = placeholder {
      cx.drop_image(image, None);
      #[cfg(test)]
      RELEASED_IMAGE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
  }

  #[cfg(test)]
  pub(crate) fn entry_for_test(&self, resource: &Resource) -> Option<&Entry> {
    self.entries.get(&hash(resource))
  }
}
#[cfg(test)]
pub(crate) fn reset_released_image_count_for_test() {
  RELEASED_IMAGE_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn released_image_count_for_test() -> usize {
  RELEASED_IMAGE_COUNT.load(Ordering::Relaxed)
}
fn make_placeholder() -> Arc<RenderImage> {
  let image = RgbaImage::from_fn(64, 64, |x, y| {
    let edge = x < 2 || y < 2 || x >= 62 || y >= 62;
    let diagonal = x.abs_diff(y) <= 1;
    let shade = if edge || diagonal { 80 } else { 160 };
    Rgba([shade, shade, shade, 255])
  });
  Arc::new(RenderImage::new(vec![Frame::new(image)]))
}

impl ImageCache for DocumentImageCache {
  fn load(
    &mut self,
    resource: &Resource,
    window: &mut Window,
    cx: &mut App,
  ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    Self::load(self, resource, window, cx)
  }
}

fn decode_local(
  path: &Path,
  placeholder_slot: &Arc<OnceLock<Arc<RenderImage>>>,
) -> Result<Arc<RenderImage>, ImageCacheError> {
  let bytes = read_local(path)?;
  decode_bytes(&bytes, placeholder_slot)
}

fn decode_bytes(
  bytes: &[u8],
  placeholder_slot: &Arc<OnceLock<Arc<RenderImage>>>,
) -> Result<Arc<RenderImage>, ImageCacheError> {
  match image_decode::decode(bytes) {
    Decoded::Raster(image) => Ok(image),
    Decoded::Svg => Ok(placeholder_slot.get_or_init(make_placeholder).clone()),
    Decoded::Unsupported => Err(other_error("unsupported image")),
  }
}

fn read_local(path: &Path) -> Result<Vec<u8>, ImageCacheError> {
  let path_metadata = fs::metadata(path).map_err(io_error)?;
  if !path_metadata.is_file() {
    return Err(io_error(std::io::Error::new(
      ErrorKind::InvalidInput,
      "image is not a regular file",
    )));
  }
  let file = File::open(path).map_err(io_error)?;
  let metadata = file.metadata().map_err(io_error)?;
  if !metadata.is_file() {
    return Err(io_error(std::io::Error::new(
      ErrorKind::InvalidInput,
      "image is not a regular file",
    )));
  }
  let mut bytes = Vec::new();

  file
    .take(image_decode::MAX_IMAGE_BYTES.saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(io_error)?;
  let Ok(size) = u64::try_from(bytes.len()) else {
    return Err(other_error("image exceeds the maximum size"));
  };
  if size > image_decode::MAX_IMAGE_BYTES {
    return Err(other_error("image exceeds the maximum size"));
  }
  Ok(bytes)
}

fn io_error(error: std::io::Error) -> ImageCacheError {
  ImageCacheError::Io(Arc::new(error))
}

fn other_error(message: impl Into<String>) -> ImageCacheError {
  ImageCacheError::Other(Arc::new(gpui_kit::private::anyhow::anyhow!(message.into())))
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;
  use std::thread;
  use std::time::Duration;

  #[cfg(unix)]
  #[test]
  fn a_fifo_local_image_is_rejected_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || sender.send(super::read_local(&path)).unwrap());

    let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    handle.join().unwrap();
    assert!(result.is_err());
  }
}
