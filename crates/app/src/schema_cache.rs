//! Load JSON Schema documents through the document resource path.
//!
//! Local files and remote URLs go through `resolve`, `Fetcher`, `ResourceCache`,
//! and `PermissionRequests`. `$ref` hops use that path up to [`schema::REF_DEPTH`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::{FutureExt as _, future::Shared};
use gpui_kit::{App, AppContext, Entity, Resource, Task, Window};
use openit_core::resource::{self, Decision, DomainFamily, Resolved};
use openit_core::schema::{self, CompiledSchema, JsonFamily, ParsedJson, REF_DEPTH, SchemaDocuments};
use openit_core::select::{self, SchemaSelection};
use serde_json::Value;

use crate::cache::ResourceCacheHandle;
use crate::fetch::Fetcher;
use crate::image_cache::PermissionRequests;
use crate::settings::AppSettings;

/// In-memory schema documents for one JSON-family file.
pub struct DocumentSchemaCache {
  requests: Entity<PermissionRequests>,
  entries: HashMap<SchemaKey, Entry>,
  notifications: HashMap<SchemaKey, Task<()>>,
  selection: SchemaSelection,
  root: Option<SchemaKey>,
  compiled: Option<Arc<CompiledSchema>>,
  last_selection: Option<SelectionKey>,
}

#[derive(Clone, PartialEq, Eq)]
struct SelectionKey {
  path: PathBuf,
  schema: Option<String>,
  manual: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SchemaKey {
  Local(PathBuf),
  Remote(String),
}

enum Entry {
  Loading {
    task: Shared<Task<Result<LoadResult, String>>>,
    depth: u32,
    resolved: Resolved,
  },
  Ready(Arc<Value>),
  Failed,
  AwaitingPermission {
    family: DomainFamily,
    depth: u32,
    resolved: Resolved,
  },
}

#[derive(Clone)]
enum LoadResult {
  Cached(Vec<u8>),
  Fetched(Vec<u8>),
  CacheMiss(Resolved),
}

/// Whether the current schema documents can be compiled.
pub(crate) enum SchemaDocs {
  /// A `$ref` or root document is still loading.
  Pending,
  /// No schema, or the root document failed.
  None,
  /// Root and every reached `$ref` are in memory.
  Ready(SchemaDocuments),
}

impl SchemaKey {
  fn from_resolved(resolved: &Resolved) -> Option<Self> {
    match resolved {
      Resolved::Local(path) => Some(Self::Local(path.clone())),
      Resolved::Remote(url) => Some(Self::Remote(url.to_string())),
      Resolved::Denied(_) => None,
    }
  }

  fn as_resolved(&self) -> Resolved {
    match self {
      Self::Local(path) => Resolved::Local(path.clone()),
      Self::Remote(url) => resource::resolve(url, None),
    }
  }

  fn resource(&self) -> Option<Resource> {
    match self {
      Self::Remote(url) => Some(Resource::Uri(url.as_str().into())),
      Self::Local(_) => None,
    }
  }
}

impl DocumentSchemaCache {
  /// Create a cache that shares a document's permission requests.
  pub fn new<C: AppContext>(requests: Entity<PermissionRequests>, cx: &mut C) -> Entity<Self> {
    cx.new(|_| Self {
      requests,
      entries: HashMap::new(),
      notifications: HashMap::new(),
      selection: SchemaSelection::None,
      root: None,
      compiled: None,
      last_selection: None,
    })
  }

  /// Quiet status label for the current selection.
  pub fn status_name(&self) -> String {
    self.selection.status_name()
  }

  /// The current selection, including ambiguous catalog matches.
  pub const fn selection(&self) -> &SchemaSelection {
    &self.selection
  }

  /// Select the document schema and load it, following `$ref` up to [`REF_DEPTH`].
  ///
  /// `parsed` is the instance document already parsed by the caller. Catalog
  /// matching is skipped when `path`, `$schema`, and the manual pick are unchanged.
  #[expect(
    clippy::too_many_arguments,
    reason = "one entry point for the parsed instance and its raw text"
  )]
  pub fn load_document(
    &mut self,
    path: &Path,
    parsed: Option<&ParsedJson>,
    text: &str,
    manual: Option<&str>,
    window: &Window,
    cx: &mut App,
  ) {
    let has_path = !path.as_os_str().is_empty();
    let family = if has_path {
      JsonFamily::from_path(path)
    } else {
      Some(JsonFamily::Json)
    };
    if family.is_none() {
      self.selection = SchemaSelection::None;
      self.root = None;
      self.compiled = None;
      self.last_selection = None;
      return;
    }
    let schema = parsed
      .and_then(|parsed| parsed.value.get("$schema").and_then(Value::as_str))
      .map(str::to_owned)
      .or_else(|| schema::schema_keyword(text));
    let key = SelectionKey {
      path: path.to_path_buf(),
      schema: schema.clone(),
      manual: manual.map(str::to_owned),
    };
    if self.last_selection.as_ref() == Some(&key) {
      return;
    }
    self.last_selection = Some(key);
    self.compiled = None;
    let path_opt = has_path.then_some(path);
    let value = parsed.map_or_else(
      || schema.map_or(Value::Null, |url| serde_json::json!({ "$schema": url })),
      |parsed| parsed.value.clone(),
    );
    self.selection = select::select(path_opt, &value, manual);
    match self.selection.clone() {
      SchemaSelection::Local(schema_path) => {
        let resolved = Resolved::Local(schema_path);
        self.root = SchemaKey::from_resolved(&resolved);
        self.begin(resolved, 0, window, cx);
      },
      SchemaSelection::Remote(url) => {
        let resolved = Resolved::Remote(url);
        self.root = SchemaKey::from_resolved(&resolved);
        self.begin(resolved, 0, window, cx);
      },
      SchemaSelection::Ask(_) | SchemaSelection::None | SchemaSelection::Denied(_) => {
        self.root = None;
      },
    }
  }

  /// Compile once per schema identity. Returns whether a `$ref` is still loading.
  pub(crate) fn prepare_compiled(&mut self) -> bool {
    match self.schema_state() {
      SchemaDocs::Pending => true,
      SchemaDocs::None => {
        self.compiled = None;
        false
      },
      SchemaDocs::Ready(_) => {
        if self.compiled.is_none()
          && let SchemaDocs::Ready(documents) = self.documents()
        {
          self.compiled = CompiledSchema::get_or_compile(None, &documents).ok().map(Arc::new);
        }
        false
      },
    }
  }

  /// The compiled validator for the current schema set, when ready.
  pub(crate) fn compiled(&self) -> Option<Arc<CompiledSchema>> {
    self.compiled.clone()
  }

  fn schema_state(&self) -> SchemaDocs {
    let Some(root) = &self.root else {
      return SchemaDocs::None;
    };
    match self.entries.get(root) {
      Some(Entry::Loading { .. } | Entry::AwaitingPermission { .. }) => SchemaDocs::Pending,
      Some(Entry::Failed) | None => SchemaDocs::None,
      Some(Entry::Ready(_)) => {
        let mut pending = false;
        let mut seen = HashSet::new();
        self.collect_state(root, &mut seen, &mut pending);
        if pending {
          SchemaDocs::Pending
        } else {
          SchemaDocs::Ready(SchemaDocuments {
            root: schema_uri(root),
            documents: BTreeMap::new(),
          })
        }
      },
    }
  }

  fn collect_state(&self, key: &SchemaKey, seen: &mut HashSet<String>, pending: &mut bool) {
    let uri = schema_uri(key);
    if !seen.insert(uri) {
      return;
    }
    match self.entries.get(key) {
      Some(Entry::Loading { .. } | Entry::AwaitingPermission { .. }) => *pending = true,
      Some(Entry::Failed) | None => {},
      Some(Entry::Ready(value)) => {
        let base = key.as_resolved();
        for reference in schema::document_refs(value) {
          if let Some(resolved) = schema::resolve_ref(&reference, &base)
            && let Some(next) = SchemaKey::from_resolved(&resolved)
          {
            self.collect_state(&next, seen, pending);
          }
        }
      },
    }
  }

  /// Ready schema documents for the current root, when every `$ref` hop has finished.
  pub(crate) fn documents(&self) -> SchemaDocs {
    let Some(root) = &self.root else {
      return SchemaDocs::None;
    };
    match self.entries.get(root) {
      Some(Entry::Loading { .. } | Entry::AwaitingPermission { .. }) => SchemaDocs::Pending,
      Some(Entry::Failed) | None => SchemaDocs::None,
      Some(Entry::Ready(_)) => {
        let mut documents = BTreeMap::new();
        let mut pending = false;
        self.collect_ready(root, &mut documents, &mut pending);
        if pending {
          return SchemaDocs::Pending;
        }
        SchemaDocs::Ready(SchemaDocuments { root: schema_uri(root), documents })
      },
    }
  }

  fn collect_ready(&self, key: &SchemaKey, documents: &mut BTreeMap<String, Value>, pending: &mut bool) {
    let uri = schema_uri(key);
    if documents.contains_key(&uri) {
      return;
    }
    match self.entries.get(key) {
      Some(Entry::Loading { .. } | Entry::AwaitingPermission { .. }) => *pending = true,
      Some(Entry::Failed) | None => {},
      Some(Entry::Ready(value)) => {
        documents.insert(uri, (**value).clone());
        let base = key.as_resolved();
        for reference in schema::document_refs(value) {
          if let Some(resolved) = schema::resolve_ref(&reference, &base)
            && let Some(next) = SchemaKey::from_resolved(&resolved)
          {
            self.collect_ready(&next, documents, pending);
          }
        }
      },
    }
  }

  /// Advance in-flight loads and follow `$ref` on documents that just finished.
  pub fn pump(&mut self, window: &Window, cx: &mut App) {
    let keys = self.entries.keys().cloned().collect::<Vec<_>>();
    for key in keys {
      self.pump_one(&key, window, cx);
    }
  }

  /// Retry schema documents waiting for one permission family.
  pub fn retry_family(&mut self, family: &DomainFamily, window: &Window, cx: &mut App) {
    let waiting = self
      .entries
      .iter()
      .filter_map(|(key, entry)| match entry {
        Entry::AwaitingPermission { family: waiting, depth, resolved } if waiting == family => {
          Some((key.clone(), *depth, resolved.clone()))
        },
        _ => None,
      })
      .collect::<Vec<_>>();
    if waiting.is_empty() {
      return;
    }
    self.compiled = None;
    for (key, depth, resolved) in waiting {
      self.entries.remove(&key);
      self.notifications.remove(&key);
      self.begin(resolved, depth, window, cx);
    }
  }

  /// Retry every schema document waiting for permission.
  pub fn retry_all(&mut self, window: &Window, cx: &mut App) {
    let waiting = self
      .entries
      .iter()
      .filter_map(|(key, entry)| match entry {
        Entry::AwaitingPermission { depth, resolved, .. } => Some((key.clone(), *depth, resolved.clone())),
        _ => None,
      })
      .collect::<Vec<_>>();
    if !waiting.is_empty() {
      self.compiled = None;
    }
    let mut families = Vec::new();
    for (key, depth, resolved) in waiting {
      if let Resolved::Remote(url) = &resolved
        && let Some(host) = url.host_str()
      {
        let family = DomainFamily::of_host(host);
        if !families.iter().any(|waiting| waiting == &family) {
          families.push(family);
        }
      }
      self.entries.remove(&key);
      self.notifications.remove(&key);
      self.begin(resolved, depth, window, cx);
    }
    for family in families {
      let still_waiting = self
        .entries
        .values()
        .any(|entry| matches!(entry, Entry::AwaitingPermission { family: waiting, .. } if waiting == &family));
      if !still_waiting {
        self.requests.update(cx, |requests, cx| requests.dismiss_family(&family, cx));
      }
    }
  }

  fn begin(&mut self, resolved: Resolved, depth: u32, window: &Window, cx: &mut App) {
    if depth > REF_DEPTH {
      return;
    }
    let Some(key) = SchemaKey::from_resolved(&resolved) else {
      return;
    };
    if self.entries.contains_key(&key) {
      return;
    }
    match &resolved {
      Resolved::Remote(_) => self.start_cache_probe(key, resolved, depth, window, cx),
      Resolved::Local(_) => self.apply_decision(key, resolved, depth, window, cx),
      Resolved::Denied(_) => {},
    }
  }

  fn pump_one(&mut self, key: &SchemaKey, window: &Window, cx: &mut App) {
    let finished = match self.entries.get_mut(key) {
      Some(Entry::Loading { task, depth, resolved }) => {
        task.clone().now_or_never().map(|result| (*depth, resolved.clone(), result))
      },
      _ => None,
    };
    let Some((depth, resolved, result)) = finished else {
      return;
    };
    match result {
      Ok(LoadResult::Cached(bytes)) => match parse_schema(&bytes, schema_family(&resolved)) {
        Ok(value) => self.finish_ready(key, value, depth, window, cx),
        Err(error) => {
          tracing::debug!(%error, schema = %display_resolved(&resolved), "cached schema document is not JSON");
          if let Resolved::Remote(url) = &resolved
            && let Some(cache) = cx.try_global::<ResourceCacheHandle>().map(|cache| Arc::clone(&cache.0))
            && let Err(remove_error) = cache.remove(url)
          {
            tracing::warn!(%remove_error, url = %url, "could not remove invalid cached schema");
          }
          self.entries.remove(key);
          self.apply_decision(key.clone(), resolved, depth, window, cx);
        },
      },
      Ok(LoadResult::Fetched(bytes)) => match parse_schema(&bytes, schema_family(&resolved)) {
        Ok(value) => {
          if let Resolved::Remote(url) = &resolved
            && let Some(cache) = cx.try_global::<ResourceCacheHandle>().map(|cache| Arc::clone(&cache.0))
            && let Err(error) = cache.put(url, &bytes)
          {
            tracing::warn!(%error, url = %url, "could not cache fetched schema");
          }
          self.finish_ready(key, value, depth, window, cx);
        },
        Err(error) => {
          tracing::warn!(%error, schema = %display_resolved(&resolved), "schema document is not JSON");
          self.entries.insert(key.clone(), Entry::Failed);
        },
      },
      Ok(LoadResult::CacheMiss(resolved)) => {
        self.entries.remove(key);
        self.apply_decision(key.clone(), resolved, depth, window, cx);
      },
      Err(error) => {
        tracing::warn!(error = %error, schema = %display_resolved(&resolved), "schema fetch failed");
        self.entries.insert(key.clone(), Entry::Failed);
      },
    }
  }

  fn finish_ready(&mut self, key: &SchemaKey, value: Value, depth: u32, window: &Window, cx: &mut App) {
    self.compiled = None;
    self.entries.insert(key.clone(), Entry::Ready(Arc::new(value)));
    self.follow_refs(key, depth, window, cx);
  }

  fn follow_refs(&mut self, key: &SchemaKey, depth: u32, window: &Window, cx: &mut App) {
    if depth >= REF_DEPTH {
      return;
    }
    let Some(Entry::Ready(value)) = self.entries.get(key) else {
      return;
    };
    let value = Arc::clone(value);
    let base = key.as_resolved();
    for reference in schema::document_refs(&value) {
      if let Some(resolved) = schema::resolve_ref(&reference, &base) {
        self.begin(resolved, depth.saturating_add(1), window, cx);
      }
    }
  }

  fn apply_decision(&mut self, key: SchemaKey, resolved: Resolved, depth: u32, window: &Window, cx: &mut App) {
    match resource::decide(&resolved, &cx.global::<AppSettings>().0) {
      Decision::Deny(_) => {},
      Decision::Ask(family) => {
        if let Some(resource) = key.resource() {
          let requests = self.requests.clone();
          requests.update(cx, |requests, cx| requests.ask(family.clone(), resource, cx));
        }
        self.entries.insert(key, Entry::AwaitingPermission { family, depth, resolved });
      },
      Decision::Allow => self.start_load(key, resolved, depth, window, cx),
    }
  }

  fn start_cache_probe(&mut self, key: SchemaKey, resolved: Resolved, depth: u32, window: &Window, cx: &App) {
    let Resolved::Remote(url) = resolved.clone() else {
      return;
    };
    let cache = cx.try_global::<ResourceCacheHandle>().map(|cache| Arc::clone(&cache.0));
    let task = cx
      .background_spawn(async move {
        let Some(cache) = cache else {
          return Ok(LoadResult::CacheMiss(Resolved::Remote(url)));
        };
        match cache.get(&url) {
          Ok(Some(bytes)) => Ok(LoadResult::Cached(bytes)),
          Ok(None) => Ok(LoadResult::CacheMiss(Resolved::Remote(url))),
          Err(error) => {
            tracing::warn!(%error, url = %url, "could not read cached schema");
            Ok(LoadResult::CacheMiss(Resolved::Remote(url)))
          },
        }
      })
      .shared();
    self
      .entries
      .insert(key.clone(), Entry::Loading { task: task.clone(), depth, resolved });
    self.watch_task(key, task, window, cx);
  }

  fn start_load(&mut self, key: SchemaKey, resolved: Resolved, depth: u32, window: &Window, cx: &App) {
    let task = match resolved.clone() {
      Resolved::Local(path) => cx.background_spawn(async move {
        resource::read_local(&path)
          .map(LoadResult::Fetched)
          .map_err(|error| error.to_string())
      }),
      Resolved::Remote(url) => {
        let Some(host) = url.host_str() else {
          tracing::warn!(url = %url, "schema fetch failed");
          self.entries.insert(key, Entry::Failed);
          return;
        };
        let family = DomainFamily::of_host(host);
        let fetcher = Arc::clone(&cx.global::<Fetcher>().0);
        cx.background_spawn(async move {
          fetcher
            .fetch(url, family)
            .await
            .map(LoadResult::Fetched)
            .map_err(|error| error.to_string())
        })
      },
      Resolved::Denied(_) => return,
    }
    .shared();
    self
      .entries
      .insert(key.clone(), Entry::Loading { task: task.clone(), depth, resolved });
    self.watch_task(key, task, window, cx);
  }

  fn watch_task(&mut self, key: SchemaKey, task: Shared<Task<Result<LoadResult, String>>>, window: &Window, cx: &App) {
    // `Window::current_view` panics outside layout/paint. Schema load starts while
    // the window is opening, so notify the permission entity the view already observes.
    let entity = self.requests.entity_id();
    let notification = window.spawn(cx, async move |cx| {
      let _ = task.await;
      cx.on_next_frame(move |_, cx| cx.notify(entity));
    });
    self.notifications.insert(key, notification);
  }

  #[cfg(test)]
  pub(crate) fn is_ready(&self, resolved: &Resolved) -> bool {
    SchemaKey::from_resolved(resolved).is_some_and(|key| matches!(self.entries.get(&key), Some(Entry::Ready(_))))
  }

  #[cfg(test)]
  pub(crate) fn is_failed(&self, resolved: &Resolved) -> bool {
    SchemaKey::from_resolved(resolved).is_some_and(|key| matches!(self.entries.get(&key), Some(Entry::Failed)))
  }

  #[cfg(test)]
  pub(crate) fn is_awaiting(&self, resolved: &Resolved) -> bool {
    SchemaKey::from_resolved(resolved)
      .is_some_and(|key| matches!(self.entries.get(&key), Some(Entry::AwaitingPermission { .. })))
  }
}

fn schema_family(resolved: &Resolved) -> JsonFamily {
  match resolved {
    Resolved::Local(path) => JsonFamily::from_path(path).unwrap_or(JsonFamily::Jsonc),
    Resolved::Remote(_) | Resolved::Denied(_) => JsonFamily::Json,
  }
}

fn parse_schema(bytes: &[u8], family: JsonFamily) -> Result<Value, String> {
  let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
  schema::parse(family, text)
    .map(|parsed| parsed.value)
    .map_err(|error| error.message)
}

fn display_resolved(resolved: &Resolved) -> String {
  match resolved {
    Resolved::Local(path) => path.display().to_string(),
    Resolved::Remote(url) => url.to_string(),
    Resolved::Denied(reason) => reason.to_string(),
  }
}

fn schema_uri(key: &SchemaKey) -> String {
  match key {
    SchemaKey::Local(path) => path.to_string_lossy().into_owned(),
    SchemaKey::Remote(url) => url.clone(),
  }
}
