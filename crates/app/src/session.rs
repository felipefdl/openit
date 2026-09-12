use crate::document_view::DocumentView;
use crate::settings::{AUTOSAVE_DELAY, AppSettings};
use gpui_kit::{AnyWindowHandle, AppContext, BorrowAppContext, Context, Global, Task, Window};
use openit_core::document::{Loaded, Revision, Snapshot, load_text};
use openit_core::kind::DocumentKind;
use openit_core::recovery::{Draft, RecoveryStore};
use openit_core::save::{Saved, save_text};
use openit_core::session::SessionId;
use openit_core::watch::{FileWatch, Fingerprint};
use ropey::Rope;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
/// The application's draft store, shared by every window. `None` means all
/// recovery operations fail with a visible error.
pub struct Recovery(pub Option<Arc<RecoveryStore>>);
impl Global for Recovery {}
/// How long after the last edit a draft is written.
pub const CHECKPOINT_DELAY: Duration = Duration::from_secs(1);
/// Detached close cleanups kept alive until they finish or the application quits.
#[derive(Default)]
pub struct PendingCleanups(pub Vec<Task<()>>);
impl Global for PendingCleanups {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
  Close,
  Overwrite,
}
enum SaveOutcome {
  Saved(Saved),
  Conflict,
  Failed(String),
}
type Operation = Pin<Box<dyn Future<Output = ()> + 'static>>;
#[derive(Clone, Copy)]
pub(crate) struct ReloadParams {
  pub(crate) revision: Revision,
  pub(crate) disk: Option<Fingerprint>,
  pub(crate) observed_disk: Option<Fingerprint>,
}
#[expect(
  clippy::struct_excessive_bools,
  reason = "DocumentSession tracks independent persistence and close state"
)]
pub struct DocumentSession {
  pub(crate) path: PathBuf,
  pub(crate) window_handle: AnyWindowHandle,
  pub(crate) session: SessionId,
  pub(crate) kind: DocumentKind,
  pub(crate) revision: Revision,
  pub(crate) saved_revision: Revision,
  pub(crate) checkpoint_revision: Option<Revision>,
  checkpoint_task: Option<Task<()>>,
  autosave_task: Option<Task<()>>,
  op_chain: Option<Task<()>>,
  op_running: bool,
  op_queue: VecDeque<Operation>,
  op_wake: Option<async_channel::Sender<()>>,
  op_waiters: Vec<async_channel::Sender<()>>,
  pub(crate) saving: bool,
  pub(crate) save_failed: bool,
  pub(crate) draft_removal_failed: bool,
  pub(crate) pending_save: bool,
  pub(crate) close_confirmed: bool,
  pub(crate) prompt: Option<PromptKind>,
  pub(crate) close_prompt_pending: bool,
  pub(crate) close_decided: bool,
  pub(crate) prompt_task: Option<Task<()>>,
  pub(crate) closing: bool,
  pub(crate) close_after_save: bool,
  pub(crate) last_error: Option<String>,
  pub(crate) settings_observation: Option<gpui_kit::Subscription>,
  #[allow(dead_code, reason = "keeps the platform watcher alive")]
  watch: Option<FileWatch>,
  #[cfg(test)]
  // Tests.
  pub(crate) watch_sender: Option<async_channel::Sender<()>>,
  reload_task: Option<Task<()>>,
  watch_task: Option<Task<()>>,
  pub(crate) disk: Option<Fingerprint>,
  pub(crate) observed_disk: Option<Fingerprint>,
  pub(crate) source_missing: bool,
  pub(crate) suppress_next_change: bool,
  pub(crate) external_change: bool,
}
impl DocumentSession {
  pub(crate) fn new(
    path: PathBuf,
    kind: DocumentKind,
    session: SessionId,
    window_handle: AnyWindowHandle,
    disk: Option<Fingerprint>,
  ) -> Self {
    Self {
      path,
      window_handle,
      session,
      kind,
      revision: Revision::INITIAL,
      saved_revision: Revision::INITIAL,
      checkpoint_revision: None,
      checkpoint_task: None,
      autosave_task: None,
      op_chain: None,
      op_running: false,
      op_queue: VecDeque::new(),
      op_wake: None,
      op_waiters: Vec::new(),
      saving: false,
      save_failed: false,
      draft_removal_failed: false,
      pending_save: false,
      close_confirmed: false,
      prompt: None,
      close_prompt_pending: false,
      close_decided: false,
      prompt_task: None,
      closing: false,
      close_after_save: false,
      last_error: None,
      settings_observation: None,
      watch: None,
      #[cfg(test)]
      watch_sender: None,
      reload_task: None,
      watch_task: None,
      disk,
      observed_disk: disk,
      source_missing: false,
      suppress_next_change: false,
      external_change: false,
    }
  }
  pub(crate) fn restore_state(&mut self) {
    let current_disk = Fingerprint::of(&self.path).ok();
    let has_path = self.has_path();
    self.observed_disk = current_disk;
    self.source_missing = has_path && current_disk.is_none();
    self.external_change = self.disk != current_disk || (has_path && current_disk.is_none());
    self.revision = Revision::INITIAL.next();
    self.checkpoint_revision = Some(self.revision);
  }
  pub(crate) fn title(&self) -> String {
    self
      .path
      .file_name()
      .map_or_else(|| "Untitled".to_owned(), |name| name.to_string_lossy().into_owned())
  }
  pub(crate) fn is_dirty(&self) -> bool {
    self.revision != self.saved_revision
  }
  pub(crate) fn has_path(&self) -> bool {
    !self.path.as_os_str().is_empty()
  }
  pub(crate) fn base_dir(&self) -> Option<PathBuf> {
    self
      .has_path()
      .then(|| self.path.parent().map(std::path::Path::to_path_buf))
      .flatten()
  }
  pub(crate) const fn source_status(&self) -> Option<&'static str> {
    if !self.external_change {
      return None;
    }
    Some(if self.source_missing {
      "Source file missing"
    } else {
      "Source changed while closed"
    })
  }
  pub(crate) fn last_error(&self) -> Option<&str> {
    self.last_error.as_deref()
  }
  pub(crate) const fn revision(&self) -> Revision {
    self.revision
  }
  pub(crate) fn snapshot(&self, text: Option<Rope>) -> Option<Snapshot> {
    text.map_or_else(|| None, |text| Some(Snapshot { revision: self.revision, text }))
  }
  pub(crate) fn begin_closing(&mut self) {
    self.closing = true;
    self.close_confirmed = true;
    self.prompt = None;
    self.close_prompt_pending = false;
    self.prompt_task = None;
    self.checkpoint_task = None;
    self.autosave_task = None;
    self.reload_task = None;
  }
  pub(crate) fn drop_pending_recovery(&mut self, cx: &Context<DocumentView>) {
    self.checkpoint_task = None;
    self.autosave_task = None;
    self.enqueue_draft_removal(cx, false);
  }
  pub(crate) fn schedule_checkpoint(&mut self, cx: &Context<DocumentView>) {
    if self.closing {
      return;
    }
    self.checkpoint_task = Some(cx.spawn(async move |this, cx| {
      cx.background_executor().timer(CHECKPOINT_DELAY).await;
      let _ = this.update(cx, |view, cx| {
        let _ = Self::checkpoint_view(view, cx, false);
      });
    }));
  }
  pub(crate) fn checkpoint_view(view: &mut DocumentView, cx: &mut Context<DocumentView>, force: bool) -> bool {
    let cursor = view.editor.as_ref().map_or(0, |editor| editor.read(cx).cursor());
    view
      .session
      .checkpoint_now(cx, view.snapshot(cx), cursor, view.schema_pick.clone(), force)
  }
  pub(crate) fn write_view(view: &mut DocumentView, force_overwrite: bool, cx: &mut Context<DocumentView>) {
    view.session.write_now(force_overwrite, view.snapshot(cx), cx);
  }
  pub(crate) fn schedule_autosave(&mut self, cx: &Context<DocumentView>) {
    if self.closing {
      return;
    }
    let enabled = cx.global::<AppSettings>().0.autosave;
    if !enabled || !self.has_path() || !self.is_dirty() || self.prompt.is_some() {
      self.autosave_task = None;
      return;
    }
    self.autosave_task = Some(cx.spawn(async move |this, cx| {
      cx.background_executor().timer(AUTOSAVE_DELAY).await;
      let _ = this.update(cx, |view, cx| {
        let session = &view.session;
        if session.closing
          || session.prompt.is_some()
          || !session.is_dirty()
          || session.saving
          || session.external_change
        {
          return;
        }
        Self::write_view(view, false, cx);
      });
    }));
  }
  pub(crate) fn enqueue_op(&mut self, cx: &Context<DocumentView>, operation: Operation) {
    self.op_queue.push_back(operation);
    if self.op_chain.is_none() {
      let (wake, wake_receiver) = async_channel::bounded(1);
      self.op_wake = Some(wake);
      self.op_chain = Some(cx.spawn(async move |this, cx| {
        loop {
          let operation = this
            .update(cx, |view, _| {
              let operation = view.session.op_queue.pop_front();
              view.session.op_running = operation.is_some();
              operation
            })
            .ok()
            .flatten();
          if let Some(operation) = operation {
            operation.await;
            continue;
          }
          let waiters = this.update(cx, |view, _| {
            view.session.op_running = false;
            std::mem::take(&mut view.session.op_waiters)
          });
          let Ok(waiters) = waiters else {
            break;
          };
          for waiter in waiters {
            let _ = waiter.try_send(());
          }
          if wake_receiver.recv().await.is_err() {
            break;
          }
        }
      }));
    }
    if !self.op_running
      && let Some(wake) = self.op_wake.as_ref()
    {
      let _ = wake.try_send(());
    }
  }
  #[allow(
    clippy::needless_pass_by_ref_mut,
    reason = "The update callback requires mutable document and context references"
  )]
  pub(crate) fn checkpoint_now(
    &mut self,
    cx: &mut Context<DocumentView>,
    snapshot: Option<Snapshot>,
    cursor: usize,
    schema: Option<String>,
    force: bool,
  ) -> bool {
    if self.close_decided
      || (!force && self.closing)
      || !self.is_dirty()
      || self.checkpoint_revision == Some(self.revision)
    {
      return false;
    }
    let Some(snapshot) = snapshot else {
      return false;
    };
    let Some(store) = cx.global::<Recovery>().0.clone() else {
      self.last_error = Some("Could not save draft: recovery store unavailable".to_owned());
      return false;
    };
    let draft_path = self.has_path().then(|| self.path.clone());
    let disk = self.disk;
    let session = self.session;
    let revision = snapshot.revision;
    let entity = cx.entity().downgrade();
    let mut app = cx.to_async();
    let operation: Operation = Box::pin(async move {
      let result = app
        .background_spawn(async move {
          let draft = Draft {
            session,
            path: draft_path,
            disk,
            text: String::new(),
            cursor,
            image: None,
            schema,
          };
          store.checkpoint_text(&draft, &snapshot.text)
        })
        .await;
      let _ = entity.update(&mut app, |view, cx| match result {
        Ok(()) => {
          if view.is_dirty() {
            view.session.checkpoint_revision = Some(revision);
            if view
              .session
              .last_error
              .as_deref()
              .is_some_and(|error| error.starts_with("Could not save draft"))
            {
              view.session.last_error = None;
            }
          } else {
            view.session.enqueue_draft_removal(cx, false);
          }
          cx.notify();
        },
        Err(error) => {
          tracing::error!(%error, "checkpoint failed");
          view.session.last_error = Some(format!("Could not save draft: {error}"));
          cx.notify();
        },
      });
    });
    self.enqueue_op(cx, operation);
    true
  }
  pub(crate) fn flush_checkpoint(
    &mut self,
    cx: &mut Context<DocumentView>,
    snapshot: Option<Snapshot>,
    cursor: usize,
    schema: Option<String>,
  ) -> Task<Result<(), String>> {
    self.checkpoint_task = None;
    if self.close_decided {
      return Task::ready(Ok(()));
    }
    let _ = self.checkpoint_now(cx, snapshot, cursor, schema, true);
    let mut pending = self.chain_drained(cx);
    cx.spawn(async move |this, cx| {
      for _ in 0..10 {
        pending.await;
        let next = this
          .update(cx, |view, cx| {
            let session = &view.session;
            if session.close_decided || !session.is_dirty() || session.checkpoint_revision == Some(session.revision) {
              return None;
            }
            let _ = Self::checkpoint_view(view, cx, true);
            Some(view.chain_drained(cx))
          })
          .ok()
          .flatten();
        let Some(next) = next else {
          break;
        };
        pending = next;
      }
      this
        .read_with(cx, |view, _| match &view.session.last_error {
          _ if view.session.save_failed => Err(
            view
              .session
              .last_error
              .clone()
              .unwrap_or_else(|| "Could not save document".to_owned()),
          ),
          Some(error) if error.starts_with("Could not save draft") => Err(error.clone()),
          _ if view.session.is_dirty() && view.session.checkpoint_revision != Some(view.session.revision) => {
            Err("Could not flush draft after 10 checkpoints".to_owned())
          },
          _ => Ok(()),
        })
        .unwrap_or_else(|_| Err("window closed".to_owned()))
    })
  }
  pub(crate) fn chain_drained(&mut self, cx: &Context<DocumentView>) -> Task<()> {
    if !self.op_running && self.op_queue.is_empty() {
      return Task::ready(());
    }
    let (waiter, receiver) = async_channel::bounded(1);
    self.op_waiters.push(waiter);
    cx.spawn(async move |_, _| {
      let _ = receiver.recv().await;
    })
  }
  pub(crate) fn write_now(
    &mut self,
    force_overwrite: bool,
    snapshot: Option<Snapshot>,
    cx: &mut Context<DocumentView>,
  ) {
    if self.closing {
      return;
    }
    if self.saving {
      self.pending_save = true;
      return;
    }
    let Some(snapshot) = snapshot else {
      return;
    };
    self.saving = true;
    self.save_failed = false;
    let path = self.path.clone();
    let expected_disk = self.disk;
    let entity = cx.entity().downgrade();
    let mut app = cx.to_async();
    let operation: Operation = Box::pin(async move {
      let result = app
        .background_spawn(async move {
          let current_disk = Fingerprint::of(&path).ok();
          if !force_overwrite && current_disk != expected_disk {
            return SaveOutcome::Conflict;
          }
          save_text(&path, &snapshot).map_or_else(|error| SaveOutcome::Failed(error.to_string()), SaveOutcome::Saved)
        })
        .await;
      let _ = entity.update(&mut app, |view, cx| {
        view.session.saving = false;
        match result {
          SaveOutcome::Saved(saved) => Self::complete_save_result(view, saved, cx),
          SaveOutcome::Conflict => {
            view.session.external_change = true;
            view.session.source_missing = !view.session.path.exists();
            view.session.last_error = Some(format!("Source changed on disk; save canceled: {}", view.title()));
            Self::abandon_close(view, &*cx);
          },
          SaveOutcome::Failed(error) => {
            Self::abandon_close(view, &*cx);
            view.session.save_failed = true;
            tracing::error!(%error, "save failed");
            view.session.last_error = Some(error);
          },
        }
        cx.notify();
      });
    });
    self.enqueue_op(cx, operation);
    cx.notify();
  }
  pub(crate) fn apply_save_result(&mut self, saved: Saved, _: &Context<DocumentView>) {
    self.saved_revision = saved.revision;
    self.disk = Some(saved.fingerprint);
    self.observed_disk = Some(saved.fingerprint);
    self.source_missing = false;
    self.external_change = Fingerprint::of(&self.path).ok() != Some(saved.fingerprint);
    self.save_failed = false;
    if !self.is_dirty() {
      self.autosave_task = None;
    }
    self.last_error = None;
  }
  pub(crate) fn complete_save_result(view: &mut DocumentView, saved: Saved, cx: &mut Context<DocumentView>) {
    view.session.apply_save_result(saved, cx);
    if view.session.close_after_save {
      if !view.is_dirty() && !view.session.external_change {
        view.session.close_after_save = false;
        view.session.close_decided = true;
        view.session.begin_closing();
        view.session.detach_close(view.session.window_handle, cx);
        return;
      }
      Self::abandon_close(view, cx);
    }
    if !view.is_dirty() {
      view.session.enqueue_draft_removal(cx, false);
    }
    if view.session.pending_save {
      view.session.pending_save = false;
      if !view.session.closing && view.is_dirty() {
        Self::write_view(view, false, cx);
      }
    }
  }
  pub(crate) fn enqueue_draft_removal(&mut self, cx: &Context<DocumentView>, for_close: bool) {
    let Some(store) = cx.global::<Recovery>().0.clone() else {
      self.draft_removal_failed = for_close;
      self.last_error = Some("Could not remove draft: recovery store unavailable".to_owned());
      return;
    };
    let session = self.session;
    let entity = cx.entity().downgrade();
    let mut app = cx.to_async();
    let operation: Operation = Box::pin(async move {
      let result = app.background_spawn(async move { store.remove(session) }).await;
      if let Err(error) = result {
        tracing::warn!(%error, "draft removal failed");
        let _ = entity.update(&mut app, |view, cx| {
          view.session.draft_removal_failed = for_close;
          view.session.last_error = Some(format!("Could not remove draft: {error}"));
          cx.notify();
        });
      } else if for_close {
        let _ = entity.update(&mut app, |view, _| view.session.draft_removal_failed = false);
      }
    });
    self.enqueue_op(cx, operation);
  }
  pub(crate) fn start_watch(&mut self, window: &Window, cx: &Context<DocumentView>) {
    let (tx, rx) = async_channel::bounded::<()>(1);
    #[cfg(test)]
    {
      self.watch_sender = Some(tx);
    }
    #[cfg(not(test))]
    {
      match FileWatch::new(&self.path, move || {
        let _ = tx.try_send(());
      }) {
        Ok(watch) => self.watch = Some(watch),
        Err(error) => {
          tracing::warn!(%error, "file watch unavailable");
          return;
        },
      }
    }
    self.watch_task = Some(cx.spawn_in(window, async move |this, cx| {
      while cx
        .background_spawn({
          let rx = rx.clone();
          async move { rx.recv().await }
        })
        .await
        .is_ok()
      {
        cx.background_executor().timer(Duration::from_millis(150)).await;
        while rx.try_recv().is_ok() {}
        if this.update_in(cx, DocumentView::on_disk_change).is_err() {
          break;
        }
      }
    }));
  }
  pub(crate) fn on_disk_change(&mut self, window: &Window, cx: &mut Context<DocumentView>) {
    if self.closing {
      return;
    }
    let now = Fingerprint::of(&self.path).ok();
    if now == self.observed_disk {
      return;
    }
    self.observed_disk = now;
    self.source_missing = !self.path.as_os_str().is_empty() && now.is_none();
    if self.is_dirty() {
      self.external_change = true;
      cx.notify();
      return;
    }
    let captured = ReloadParams {
      revision: self.revision,
      disk: self.disk,
      observed_disk: self.observed_disk,
    };
    let path = self.path.clone();
    self.reload_task = Some(cx.spawn_in(window, async move |this, cx| {
      let result = cx
        .background_spawn(async move { load_text(&path) })
        .await
        .map_err(|error| error.to_string());
      let _ = this.update_in(cx, |view, window, cx| {
        Self::apply_reload_result(view, captured, result, window, cx);
      });
    }));
  }
  pub(crate) fn apply_reload_result(
    view: &mut DocumentView,
    captured: ReloadParams,
    result: Result<Loaded, String>,
    window: &mut Window,
    cx: &mut Context<DocumentView>,
  ) {
    let loaded = {
      let session = &mut view.session;
      if session.closing {
        return;
      }
      let current = session.revision == captured.revision
        && !session.is_dirty()
        && session.disk == captured.disk
        && session.observed_disk == captured.observed_disk;
      if !current {
        session.external_change = true;
      }
      match result {
        Ok(loaded) if current => {
          session.disk = Some(loaded.disk);
          session.observed_disk = Some(loaded.disk);
          Some(loaded)
        },
        Ok(_) => {
          cx.notify();
          None
        },
        Err(error) => {
          tracing::warn!(%error, "reload after disk change failed");
          session.source_missing = !session.path.as_os_str().is_empty() && !session.path.exists();
          session.external_change = true;
          session.last_error = Some(error);
          cx.notify();
          None
        },
      }
    };
    if let Some(loaded) = loaded {
      Self::replace_buffer(view, loaded.text, window, cx);
    }
  }
  pub(crate) fn replace_buffer(
    view: &mut DocumentView,
    text: String,
    window: &mut Window,
    cx: &mut Context<DocumentView>,
  ) {
    view.session.suppress_next_change = view.editor.is_some();
    match &view.editor {
      Some(editor) => editor.update(cx, |state, cx| state.replace_all(&text, window, cx)),
      None => view.initial_text = Some(Rope::from(text)),
    }
    view.session.mark_replaced();
    view.invalidate_preview();
    view.start_schema_validation(window, cx);
    cx.notify();
  }
  pub(crate) const fn mark_replaced(&mut self) {
    self.revision = self.revision.next();
    self.saved_revision = self.revision;
    self.checkpoint_revision = None;
    self.source_missing = false;
    self.external_change = false;
  }
  pub(crate) fn abandon_close(view: &mut DocumentView, cx: &Context<DocumentView>) {
    let session = &mut view.session;
    session.prompt = None;
    session.close_prompt_pending = false;
    session.close_decided = false;
    session.close_after_save = false;
    session.save_failed = false;
    session.schedule_autosave(cx);
  }
  pub(crate) fn discard_and_close(view: &mut DocumentView, cx: &mut Context<DocumentView>) {
    view.session.close_decided = true;
    view.session.begin_closing();
    view.session.detach_close(view.session.window_handle, cx);
  }
  pub(crate) fn save_then_close(view: &mut DocumentView, window: &mut Window, cx: &mut Context<DocumentView>) {
    view.session.close_after_save = true;
    view.save(&crate::actions::Save, window, cx);
  }
  pub(crate) fn detach_close(&mut self, window_handle: AnyWindowHandle, cx: &mut Context<DocumentView>) {
    self.draft_removal_failed = false;
    self.enqueue_draft_removal(cx, true);
    let chain_drained = self.chain_drained(cx);
    let entity = cx.entity().downgrade();
    let cleanup = cx.spawn(async move |_, cx| {
      chain_drained.await;
      let removal_succeeded = entity.update(cx, |view, _| !view.session.draft_removal_failed).unwrap_or(false);
      if removal_succeeded {
        let _ = cx.update_window(window_handle, |_, window, _| window.remove_window());
      } else {
        let _ = entity.update(cx, |view, cx| {
          view.session.closing = false;
          view.session.close_confirmed = false;
          view.session.close_decided = false;
          cx.notify();
        });
      }
    });
    cx.update_default_global::<PendingCleanups, _>(|pending, _| pending.0.push(cleanup));
  }
  pub(crate) fn begin_quit(&mut self) -> bool {
    let already_closing = self.closing;
    self.begin_closing();
    already_closing
  }
  pub(crate) fn abort_quit(&mut self, cx: &Context<DocumentView>) {
    self.closing = false;
    self.close_confirmed = false;
    self.prompt = None;
    self.close_prompt_pending = false;
    self.close_decided = false;
    self.close_after_save = false;
    self.save_failed = false;
    self.schedule_autosave(cx);
  }
}
