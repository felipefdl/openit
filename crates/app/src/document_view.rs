use crate::actions::{CloseWindow, CodeFont, ColorTheme, GoToFile, Save, ToggleMode, UiFont};
use crate::drop::{apply_external_paths, external_paths_ring};
use crate::font_picker::{FontPicker, FontPickerEvent, FontSlot};
use crate::image_cache::{DocumentImageCache, PermissionAnswer, PermissionRequests};
use crate::nearby_picker::{NearbyPicker, NearbyPickerEvent};
use crate::schema_cache::DocumentSchemaCache;
use crate::schema_complete;
use crate::schema_validate::collect_issues;
use crate::session::{CHECKPOINT_DELAY, DocumentSession, PromptKind};
use crate::settings::{AppSettings, SettingsStore};
use crate::status_pickers::{
  GoToLine, GoToLineEvent, LanguagePicker, LanguagePickerEvent, SchemaPicker, SchemaPickerEvent, language_label,
};
use crate::theme::{ActivePalette, hsla, observe_appearance};
use crate::theme_picker::{ThemePicker, ThemePickerEvent};
use crate::title_bar::{file_name, toolbar_button};
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, RopeExt, TabSize};
use gpui_kit::component::text::{SelectionFormat, TextView, TextViewState, TextViewStyle};
use gpui_kit::component::{ActiveTheme, Icon, IconName, TitleBar};
use gpui_kit::prelude::{FluentBuilder, InteractiveElement, StatefulInteractiveElement};
use gpui_kit::{
  AnyElement, App, AppContext, ClickEvent, Context, Entity, ExternalPaths, FocusHandle, IntoElement, MouseButton,
  ParentElement, PathPromptOptions, PromptLevel, Render, Styled, Subscription, Task, Window, div,
};
use openit_core::document::{Loaded, Revision, Snapshot};
use openit_core::kind::DocumentKind;
use openit_core::recovery::Draft;
use openit_core::resource::DomainFamily;
use openit_core::schema::{self, JsonFamily};
use openit_core::select::SchemaSelection;
use openit_core::session::SessionId;
use openit_core::settings::{MarkdownMode, MarkdownPreviewWidth};
use openit_core::watch::Fingerprint;
use ropey::Rope;
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  Preview,
  Edit,
}
const MIN_PREVIEW_SIDE_PADDING: f32 = 24.;
const READABLE_PREVIEW_WIDTH: f32 = 700.;
const WIDE_PREVIEW_WIDTH: f32 = 960.;
/// Trailing space so the last preview line clears the window edge.
const PREVIEW_SPACER: &str = "\n\n<br>\n";

fn markdown_preview_width(width: MarkdownPreviewWidth, viewport: gpui_kit::Pixels) -> gpui_kit::Pixels {
  let available = (viewport - gpui_kit::px(MIN_PREVIEW_SIDE_PADDING * 2.)).max(gpui_kit::px(0.));
  match width {
    MarkdownPreviewWidth::Readable => available.min(gpui_kit::px(READABLE_PREVIEW_WIDTH)),
    MarkdownPreviewWidth::Wide => available.min(gpui_kit::px(WIDE_PREVIEW_WIDTH)),
    MarkdownPreviewWidth::FullWidth => available,
  }
}

type OpenParams = (PathBuf, DocumentKind, String, Option<Fingerprint>, SessionId);

fn rope_to_string(rope: &Rope) -> String {
  let mut text = String::with_capacity(rope.len());
  text.extend(rope.chunks());
  text
}

fn schema_settings_key(path: &Path) -> Option<String> {
  if path.as_os_str().is_empty() {
    return None;
  }
  let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
  let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
  Some(canonical.to_string_lossy().into_owned())
}

fn schema_pick_from_settings(path: &Path, cx: &App) -> Option<String> {
  let key = schema_settings_key(path)?;
  cx.try_global::<AppSettings>()
    .and_then(|settings| settings.0.schemas.get(&key).cloned())
}

/// The dialog floating over the document, at most one at a time.
enum Overlay {
  Theme(Entity<ThemePicker>),
  Font(Entity<FontPicker>),
  Language(Entity<LanguagePicker>),
  Schema(Entity<SchemaPicker>),
  GoToLine(Entity<GoToLine>),
  Nearby(Entity<NearbyPicker>),
}
impl Overlay {
  fn element(&self) -> AnyElement {
    match self {
      Self::Theme(view) => view.clone().into_any_element(),
      Self::Font(view) => view.clone().into_any_element(),
      Self::Language(view) => view.clone().into_any_element(),
      Self::Schema(view) => view.clone().into_any_element(),
      Self::GoToLine(view) => view.clone().into_any_element(),
      Self::Nearby(view) => view.clone().into_any_element(),
    }
  }
}

struct CursorStatus {
  editor: Entity<EditorState>,
  _observation: Subscription,
}

impl CursorStatus {
  fn new(editor: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
    Self {
      _observation: cx.observe(&editor, |_, _, cx| cx.notify()),
      editor,
    }
  }
}

impl Render for CursorStatus {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let position = self.editor.read(cx).cursor_position();
    format!(
      "Ln {}, Col {}",
      position.line.saturating_add(1),
      position.character.saturating_add(1)
    )
  }
}

pub struct DocumentView {
  pub(crate) session: DocumentSession,
  mode: Mode,
  focus: FocusHandle,
  pub(crate) initial_text: Option<Rope>,
  pending_cursor: Option<usize>,
  pub(crate) editor: Option<Entity<EditorState>>,
  preview: Option<Entity<TextViewState>>,
  preview_revision: Option<Revision>,
  preview_observation: Option<Subscription>,
  editor_subscription: Option<Subscription>,
  cursor_status_view: Option<Entity<CursorStatus>>,
  requests_observation: Option<Subscription>,
  settings_store_observation: Option<Subscription>,
  appearance_observation: Option<Subscription>,
  permission_task: Option<Task<()>>,
  pending_answers: Vec<PermissionAnswer>,
  pub(crate) image_cache: Entity<DocumentImageCache>,
  pub(crate) schema_cache: Entity<DocumentSchemaCache>,
  schema_observation: Option<Subscription>,
  schema_validate_task: Option<Task<()>>,
  schema_apply_task: Option<Task<()>>,
  schema_waiting: bool,
  pub(crate) schema_pick: Option<String>,
  schema_ask_opened: bool,
  schema_ask_task: Option<Task<()>>,
  schema_file_task: Option<Task<()>>,
  pub(crate) requests: Entity<PermissionRequests>,
  overlay: Option<Overlay>,
  overlay_subscription: Option<Subscription>,
  /// Highlighting language chosen from the status bar, over the detected one.
  language_override: Option<&'static str>,
  /// Whether another view draws this window's title bar and close guard.
  embedded: bool,
}
impl DocumentView {
  pub fn open(path: PathBuf, loaded: Loaded, session: SessionId, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let Loaded { kind, text, disk } = loaded;
    Self::open_with_disk((path, kind, text, Some(disk), session), window, cx)
  }
  fn open_with_disk(params: OpenParams, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let (path, kind, text, disk, session) = params;
    let schema_pick = schema_pick_from_settings(&path, cx);
    let session = DocumentSession::new(path, kind, session, window.window_handle(), disk);
    let requests = cx.new(|_| PermissionRequests::default());
    let image_cache = DocumentImageCache::new(session.base_dir(), requests.clone(), cx);
    DocumentImageCache::observe_release(&image_cache, &*cx);
    let schema_cache = DocumentSchemaCache::new(requests.clone(), cx);
    let mut view = Self {
      session,
      mode: match (kind, cx.global::<AppSettings>().0.markdown_mode) {
        (DocumentKind::Markdown, MarkdownMode::Preview) => Mode::Preview,
        _ => Mode::Edit,
      },
      focus: cx.focus_handle(),
      initial_text: Some(Rope::from(text)),
      pending_cursor: None,
      editor: None,
      preview: None,
      preview_revision: None,
      preview_observation: None,
      editor_subscription: None,
      cursor_status_view: None,
      requests_observation: None,
      settings_store_observation: None,
      appearance_observation: None,
      permission_task: None,
      pending_answers: Vec::new(),
      image_cache,
      schema_cache,
      schema_observation: None,
      schema_validate_task: None,
      schema_apply_task: None,
      schema_waiting: false,
      schema_pick,
      schema_ask_opened: false,
      schema_ask_task: None,
      schema_file_task: None,
      requests: requests.clone(),
      overlay: None,
      overlay_subscription: None,
      language_override: None,
      embedded: false,
    };
    view.requests_observation = Some(cx.observe_in(&requests, window, |this, requests, window, cx| {
      let answers = requests.update(cx, |requests, _| requests.take_answers());
      this.pending_answers.extend(answers);
      this.schema_cache.update(cx, |cache, cx| cache.pump(window, cx));
      if this.schema_waiting {
        this.start_schema_validation(window, cx);
      }
      if !this.pending_answers.is_empty() && this.permission_task.is_none() {
        this.permission_task = Some(Self::drain_permission_answers(cx));
      }
      cx.notify();
    }));
    view.appearance_observation = Some(observe_appearance(window));
    view.session.settings_observation = Some(cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
      this.session.schedule_autosave(cx);
      this.image_cache.update(cx, |cache, cx| cache.retry_all(window, cx));
      this.schema_cache.update(cx, |cache, cx| cache.retry_all(window, cx));
      if this.json_family().is_some() {
        this.start_schema_validation(window, cx);
      }
      cx.notify();
    }));
    view.settings_store_observation = Some(cx.observe_global::<SettingsStore>(|_, cx| cx.notify()));
    view.session.start_watch(window, cx);
    if view.mode == Mode::Preview {
      window.focus(&view.focus, cx);
    } else {
      let editor = view.ensure_editor(window, cx);
      editor.update(cx, |state, cx| state.focus(window, cx));
    }
    view.begin_schema(window, cx);
    Self::install_close_guard(window, cx);
    view
  }

  /// Apply queued permission answers until the queue stays empty.
  fn drain_permission_answers(cx: &Context<Self>) -> Task<()> {
    cx.spawn(async move |this, cx| {
      loop {
        let answers = this
          .update(cx, |view, _| std::mem::take(&mut view.pending_answers))
          .unwrap_or_else(|_| Vec::new());
        if answers.is_empty() {
          let stop = this
            .update(cx, |view, _| {
              if view.pending_answers.is_empty() {
                view.permission_task = None;
                true
              } else {
                false
              }
            })
            .unwrap_or(true);
          if stop {
            break;
          }
          continue;
        }
        for answer in answers {
          let _ = this.update_in(cx, |view, window, cx| view.apply_permission_answer(answer, window, cx));
        }
      }
    })
  }

  fn begin_schema(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.schema_observation = Some(cx.observe_in(&self.schema_cache, window, |this, _, window, cx| {
      this.schema_cache.update(cx, |cache, cx| cache.pump(window, cx));
      if this.schema_waiting {
        this.start_schema_validation(window, cx);
      }
      cx.notify();
    }));
    self.start_schema_validation(window, cx);
  }
  pub fn restore(draft: Draft, kind: DocumentKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let Draft {
      session,
      path,
      disk,
      text,
      cursor,
      image: _,
      schema,
    } = draft;
    let path = path.unwrap_or_default();
    let mut view = Self::open_with_disk((path, kind, text, disk, session), window, cx);
    view.session.restore_state();
    view.pending_cursor = Some(cursor);
    if let Some(schema) = schema
      && view.schema_pick.as_deref() != Some(schema.as_str())
    {
      view.schema_pick = Some(schema);
      view.start_schema_validation(window, cx);
    }
    if let Some(editor) = view.editor.clone()
      && let Some(cursor) = view.pending_cursor.take()
    {
      editor.update(cx, |state, cx| state.set_selected_range(cursor..cursor, cx));
    }
    cx.notify();
    view
  }
  /// Open a document that another view hosts: it draws its own body, status
  /// bar, and overlays, but not the window title bar or the close guard.
  pub fn open_embedded(path: PathBuf, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let mut view = Self::open(path, loaded, SessionId::new(), window, cx);
    view.embedded = true;
    view
  }

  /// Whether the editor, rather than the preview, is showing.
  pub fn is_editing(&self) -> bool {
    self.mode == Mode::Edit
  }

  /// Move focus to whichever surface is showing.
  pub fn focus_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    match (&self.mode, self.editor.clone()) {
      (Mode::Edit, Some(editor)) => editor.update(cx, |state, cx| state.focus(window, cx)),
      (Mode::Edit, None) => {
        let editor = self.ensure_editor(window, cx);
        editor.update(cx, |state, cx| state.focus(window, cx));
      },
      (Mode::Preview, _) => window.focus(&self.focus, cx),
    }
  }

  pub fn title(&self) -> String {
    self.session.title()
  }
  pub(crate) fn path(&self) -> &Path {
    &self.session.path
  }
  /// Run the dirty-close prompt if needed, then `on_ready`. Cancel leaves the buffer.
  pub(crate) fn confirm_leave(
    &mut self,
    window: &mut Window,
    cx: &mut Context<Self>,
    on_ready: impl FnOnce(&mut Window, &mut App) + 'static,
  ) {
    if self.session.prompt.is_some() || self.session.close_prompt_pending || self.session.closing {
      return;
    }
    if !self.is_dirty() {
      on_ready(window, cx);
      return;
    }
    self.session.close_prompt_pending = true;
    self.session.prompt = Some(PromptKind::Close);
    let answer = window.prompt(
      PromptLevel::Warning,
      &format!("Save changes to {}?", self.title()),
      Some("Discard removes the recovered draft as well."),
      &["Save", "Discard", "Cancel"],
      cx,
    );
    self.session.prompt_task = Some(cx.spawn_in(window, async move |this, cx| match answer.await {
      Ok(0) => {
        let drained = this.update_in(cx, |this, _window, cx| {
          this.session.prompt = None;
          this.session.close_prompt_pending = false;
          DocumentSession::write_view(this, false, cx);
          this.session.chain_drained(cx)
        });
        let Ok(drained) = drained else {
          return;
        };
        drained.await;
        let _ = this.update_in(cx, |this, window, cx| {
          if this.is_dirty() || this.session.last_error.is_some() {
            return;
          }
          on_ready(window, cx);
        });
      },
      Ok(1) => {
        let _ = this.update_in(cx, |this, window, cx| {
          this.session.prompt = None;
          this.session.close_prompt_pending = false;
          this.session.drop_pending_recovery(cx);
          on_ready(window, cx);
        });
      },
      _ => {
        let _ = this.update_in(cx, |this, _, cx| DocumentSession::abandon_close(this, &*cx));
      },
    }));
  }
  pub(crate) fn end_session_without_draft(&mut self, cx: &Context<Self>) {
    self.session.begin_closing();
    self.session.enqueue_draft_removal(cx, false);
  }
  pub fn is_dirty(&self) -> bool {
    self.session.is_dirty()
  }
  const fn source_status(&self) -> Option<&'static str> {
    self.session.source_status()
  }
  pub fn last_error(&self) -> Option<&str> {
    self.session.last_error()
  }
  #[cfg_attr(
    not(test),
    expect(
      dead_code,
      reason = "The save flow consumes the current revision in a later task"
    )
  )]
  pub const fn revision(&self) -> Revision {
    self.session.revision()
  }
  fn buffer_text(&self, cx: &App) -> Option<Rope> {
    match (&self.editor, &self.initial_text) {
      (Some(editor), _) => Some(editor.read(cx).text().clone()),
      (None, Some(initial)) => Some(initial.clone()),
      (None, None) => None,
    }
  }
  pub fn snapshot(&self, cx: &App) -> Option<Snapshot> {
    self.session.snapshot(self.buffer_text(cx))
  }
  pub(crate) const fn invalidate_preview(&mut self) {
    self.preview_revision = None;
  }
  pub fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
    if self.session.closing || self.session.prompt.is_some() || self.session.close_prompt_pending {
      return;
    }
    if self.session.saving {
      self.session.pending_save = true;
      return;
    }
    if self.session.external_change {
      self.session.close_prompt_pending = false;
      self.session.prompt = Some(PromptKind::Overwrite);
      let answer = window.prompt(
        PromptLevel::Warning,
        &format!("{} changed on disk", self.title()),
        Some("Saving replaces the version on disk with this window's text."),
        &["Overwrite", "Cancel"],
        cx,
      );
      self.session.prompt_task = Some(cx.spawn_in(window, async move |this, cx| {
        let response = answer.await;
        let _ = this.update_in(cx, |this, _, cx| {
          this.session.prompt = None;
          if response == Ok(0) {
            DocumentSession::write_view(this, true, cx);
          } else {
            DocumentSession::abandon_close(this, &*cx);
          }
        });
      }));
      return;
    }
    DocumentSession::write_view(self, false, cx);
  }
  pub fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.request_close_decision(window, cx).detach();
  }
  /// Whether the window holds edits the user has not yet decided to save or drop.
  pub fn needs_close_decision(&self) -> bool {
    self.session.is_dirty() && !self.session.closing
  }
  /// Close, asking first when dirty. Resolves to whether the window is on its way
  /// out: `false` when the user cancels or another prompt already holds the window.
  pub(crate) fn request_close_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
    if self.session.prompt.is_some() || self.session.close_prompt_pending {
      return Task::ready(false);
    }
    if self.session.closing || self.session.close_after_save {
      return Task::ready(true);
    }
    if !self.is_dirty() || self.session.close_confirmed {
      self.session.begin_closing();
      self.session.detach_close(self.session.window_handle, cx);
      return Task::ready(true);
    }
    self.session.close_prompt_pending = true;
    self.session.prompt = Some(PromptKind::Close);
    let answer = window.prompt(
      PromptLevel::Warning,
      &format!("Save changes to {}?", self.title()),
      Some("Discard removes the recovered draft as well."),
      &["Save", "Discard", "Cancel"],
      cx,
    );
    let (decided, decision) = async_channel::bounded(1);
    self.session.prompt_task = Some(cx.spawn_in(window, async move |this, cx| {
      let proceed = match answer.await {
        Ok(0) => this
          .update_in(cx, |this, window, cx| {
            this.session.prompt = None;
            this.session.close_prompt_pending = false;
            DocumentSession::save_then_close(this, window, cx);
          })
          .is_ok(),
        Ok(1) => this
          .update_in(cx, |this, _window, cx| {
            this.session.prompt = None;
            this.session.close_prompt_pending = false;
            DocumentSession::discard_and_close(this, cx);
          })
          .is_ok(),
        _ => {
          let _ = this.update_in(cx, |this, _, cx| DocumentSession::abandon_close(this, &*cx));
          false
        },
      };
      let _ = decided.try_send(proceed);
    }));
    cx.spawn(async move |_, _| decision.recv().await.unwrap_or(false))
  }
  pub fn begin_quit(&mut self) -> bool {
    self.session.begin_quit()
  }
  /// Reopen a document when the quit gate cannot flush its recovery draft.
  pub fn abort_quit(&mut self, cx: &Context<Self>) {
    self.session.abort_quit(cx);
  }
  pub fn close(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
    self.request_close(window, cx);
  }
  fn install_close_guard(window: &Window, cx: &Context<Self>) {
    // Weak: the platform window outlives the close in gpui-pre, and a strong entity here would keep the document alive with it.
    let entity = cx.entity().downgrade();
    window.on_window_should_close(cx, move |window, cx| {
      entity
        .update(cx, |view, cx| {
          let quitting = cx.try_global::<crate::QuitCommitted>().is_some_and(|quit| quit.0);
          if quitting && view.session.closing {
            true
          } else {
            view.request_close(window, cx);
            false
          }
        })
        .unwrap_or(true)
    });
  }
  pub(crate) fn on_disk_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.session.on_disk_change(window, cx);
  }
  /// Whether the file changed on disk while this buffer had unsaved edits.
  pub const fn external_change(&self) -> bool {
    self.session.external_change
  }
  /// Whether this view has completed its durable close decision.
  pub const fn close_decided(&self) -> bool {
    self.session.close_decided
  }
  /// Whether this view had a durable close save pending when quit began.
  pub const fn close_after_save(&self) -> bool {
    self.session.close_after_save
  }
  fn ensure_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
    if let Some(editor) = &self.editor {
      return editor.clone();
    }
    let text = self.initial_text.take().map(|rope| rope_to_string(&rope)).unwrap_or_default();
    let language = Some(self.language());
    let family = self.json_family();
    let cache = self.schema_cache.clone();
    let editor = cx.new(|cx| {
      let mut state = EditorState::new(window, cx)
        .line_number(true)
        .soft_wrap(self.is_markdown())
        .tab_size(TabSize { tab_size: 2, hard_tabs: false })
        .default_value(text);
      if let Some(name) = language {
        state = state.language(name);
      }
      if let Some(family) = family {
        schema_complete::install(&mut state, cache, family);
        state.refresh(cx);
      }
      state
    });
    if let Some(cursor) = self.pending_cursor.take() {
      editor.update(cx, |state, cx| state.set_selected_range(cursor..cursor, cx));
    }
    self.editor_subscription = Some(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
      if matches!(event, InputEvent::Change) {
        if this.session.closing {
          return;
        }
        if this.session.suppress_next_change {
          this.session.suppress_next_change = false;
        } else {
          this.session.revision = this.session.revision.next();
          this.session.schedule_checkpoint(cx);
          this.session.schedule_autosave(cx);
          this.schedule_schema_validation(cx);
          cx.notify();
        }
      }
    }));
    self.cursor_status_view = Some(cx.new(|cx| CursorStatus::new(editor.clone(), cx)));
    self.editor = Some(editor.clone());
    editor
  }
  pub fn flush_checkpoint(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
    let cursor = self.editor.as_ref().map_or(0, |editor| editor.read(cx).cursor());
    self
      .session
      .flush_checkpoint(cx, self.snapshot(cx), cursor, self.schema_pick.clone())
  }
  pub(crate) fn chain_drained(&mut self, cx: &Context<Self>) -> Task<()> {
    self.session.chain_drained(cx)
  }
  /// Open the color theme picker, or refocus it when already open.
  pub fn open_theme_picker(&mut self, _: &ColorTheme, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::Theme(picker)) = &self.overlay {
      picker.update(cx, |picker, cx| picker.focus(window, cx));
      return;
    }
    let picker = cx.new(|cx| ThemePicker::new(window, cx));
    self.overlay_subscription = Some(cx.subscribe_in(&picker, window, |this, _, _: &ThemePickerEvent, window, cx| {
      this.close_overlay(window, cx);
    }));
    self.overlay = Some(Overlay::Theme(picker));
    cx.notify();
  }
  /// Open the UI font picker, or refocus it when already open.
  pub fn open_ui_font_picker(&mut self, _: &UiFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Ui, window, cx);
  }
  /// Open the code font picker, or refocus it when already open.
  pub fn open_code_font_picker(&mut self, _: &CodeFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Code, window, cx);
  }
  fn open_font_picker(&mut self, slot: FontSlot, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::Font(picker)) = &self.overlay {
      if picker.read(cx).slot() == slot {
        picker.update(cx, |picker, cx| picker.focus(window, cx));
        return;
      }
    }
    match &self.overlay {
      Some(Overlay::Theme(picker)) => picker.update(cx, |picker, cx| picker.finish(window, cx)),
      Some(Overlay::Font(picker)) => picker.update(cx, |picker, cx| picker.finish(window, cx)),
      _ => {},
    }
    let picker = cx.new(|cx| FontPicker::new(slot, window, cx));
    self.overlay_subscription = Some(cx.subscribe_in(&picker, window, |this, _, _: &FontPickerEvent, window, cx| {
      this.close_overlay(window, cx);
    }));
    self.overlay = Some(Overlay::Font(picker));
    cx.notify();
  }
  /// Open the nearby-files picker, or refocus it when already open.
  pub fn open_nearby_picker(&mut self, _: &GoToFile, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::Nearby(picker)) = &self.overlay {
      picker.update(cx, |picker, cx| picker.focus(window, cx));
      return;
    }
    let path = self.session.has_path().then_some(self.path());
    let picker = cx.new(|cx| NearbyPicker::new(path, window, cx));
    self.overlay_subscription = Some(cx.subscribe_in(&picker, window, |this, _, _: &NearbyPickerEvent, window, cx| {
      this.close_overlay(window, cx);
    }));
    self.overlay = Some(Overlay::Nearby(picker));
    cx.notify();
  }
  /// The highlighting language in effect.
  fn language(&self) -> &'static str {
    self
      .language_override
      .or_else(|| self.session.kind.language())
      .unwrap_or("text")
  }
  fn open_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::Language(picker)) = &self.overlay {
      picker.update(cx, |picker, cx| picker.focus(window, cx));
      return;
    }
    let current = self.language();
    let picker = cx.new(|cx| LanguagePicker::new(current, window, cx));
    self.overlay_subscription =
      Some(
        cx.subscribe_in(&picker, window, |this, _, event: &LanguagePickerEvent, window, cx| {
          if let LanguagePickerEvent::Picked(name) = event {
            this.set_language(name, window, cx);
          }
          this.close_overlay(window, cx);
        }),
      );
    self.overlay = Some(Overlay::Language(picker));
    cx.notify();
  }

  fn open_schema_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::Schema(picker)) = &self.overlay {
      picker.update(cx, |picker, cx| picker.focus(window, cx));
      return;
    }
    let preferred = match self.schema_cache.read(cx).selection() {
      SchemaSelection::Ask(urls) => urls.clone(),
      _ => Vec::new(),
    };
    let picker = cx.new(|cx| SchemaPicker::new(self.schema_pick.clone(), preferred, window, cx));
    self.overlay_subscription =
      Some(
        cx.subscribe_in(&picker, window, |this, _, event: &SchemaPickerEvent, window, cx| {
          match event {
            SchemaPickerEvent::Automatic => this.set_schema_pick(None, window, cx),
            SchemaPickerEvent::Picked(value) => this.set_schema_pick(Some(value.clone()), window, cx),
            SchemaPickerEvent::PickFile => {
              this.close_overlay(window, cx);
              this.pick_schema_file(cx);
              return;
            },
            SchemaPickerEvent::Close => {},
          }
          this.close_overlay(window, cx);
        }),
      );
    self.overlay = Some(Overlay::Schema(picker));
    cx.notify();
  }

  fn set_schema_pick(&mut self, pick: Option<String>, window: &Window, cx: &mut Context<Self>) {
    if self.schema_pick == pick {
      return;
    }
    self.schema_pick = pick;
    self.schema_ask_opened = true;
    self.persist_schema_pick(cx);
    if self.session.is_dirty() {
      let _ = DocumentSession::checkpoint_view(self, cx, true);
    }
    self.start_schema_validation(window, cx);
    cx.notify();
  }

  fn persist_schema_pick(&self, cx: &mut App) {
    let Some(key) = schema_settings_key(&self.session.path) else {
      return;
    };
    let pick = self.schema_pick.clone();
    SettingsStore::update(cx, |settings| match &pick {
      Some(value) => {
        settings.schemas.insert(key, value.clone());
      },
      None => {
        settings.schemas.remove(&key);
      },
    });
  }

  fn pick_schema_file(&mut self, cx: &Context<Self>) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Select schema".into()),
    });
    self.schema_file_task = Some(cx.spawn(async move |this, cx| {
      let path = match receiver.await {
        Ok(Ok(Some(paths))) => paths.into_iter().next(),
        Ok(Ok(None)) | Err(_) => None,
        Ok(Err(error)) => {
          tracing::error!(%error, "schema file picker failed");
          None
        },
      };
      let Some(path) = path else {
        return;
      };
      let _ = this.update_in(cx, |view, window, cx| {
        view.set_schema_pick(Some(path.to_string_lossy().into_owned()), window, cx);
      });
    }));
  }

  /// Apply a language from the picker without replacing the editor state.
  fn set_language(&mut self, name: &'static str, window: &mut Window, cx: &mut Context<Self>) {
    if name == self.language() {
      return;
    }
    self.language_override = Some(name);
    let is_markdown = self.is_markdown();
    if self.mode == Mode::Preview && !is_markdown {
      self.mode = Mode::Edit;
    }
    if let Some(editor) = &self.editor {
      editor.update(cx, |state, cx| {
        state.set_highlighter(name, cx);
        state.set_soft_wrap(is_markdown, window, cx);
      });
    }
    if self.mode == Mode::Edit {
      let editor = self.ensure_editor(window, cx);
      editor.update(cx, |state, cx| state.focus(window, cx));
    }
    cx.notify();
  }
  /// Whether the effective language is Markdown, which unlocks the preview.
  fn is_markdown(&self) -> bool {
    self.language() == "markdown"
  }
  fn open_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Overlay::GoToLine(prompt)) = &self.overlay {
      prompt.update(cx, |prompt, cx| prompt.focus(window, cx));
      return;
    }
    let Some(editor) = self.editor.clone() else {
      return;
    };
    let (current, line_count) = {
      let state = editor.read(cx);
      (state.cursor_position(), state.text().lines_len())
    };
    let prompt = cx.new(|cx| GoToLine::new(current, line_count, window, cx));
    self.overlay_subscription =
      Some(
        cx.subscribe_in(&prompt, window, move |this, _, event: &GoToLineEvent, window, cx| {
          this.close_overlay(window, cx);
          if let GoToLineEvent::Jump(position) = event {
            editor.update(cx, |state, cx| state.set_cursor_position(*position, window, cx));
          }
        }),
      );
    self.overlay = Some(Overlay::GoToLine(prompt));
    cx.notify();
  }
  fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.overlay = None;
    self.overlay_subscription = None;
    match (&self.mode, &self.editor) {
      (Mode::Edit, Some(editor)) => editor.update(cx, |state, cx| state.focus(window, cx)),
      _ => window.focus(&self.focus, cx),
    }
    cx.notify();
  }
  /// Switch between Preview and Edit. No-op unless the language is Markdown.
  pub fn toggle_mode(&mut self, _: &ToggleMode, window: &mut Window, cx: &mut Context<Self>) {
    if !self.is_markdown() {
      return;
    }
    let mode = match self.mode {
      Mode::Preview => {
        let editor = self.ensure_editor(window, cx);
        editor.update(cx, |state, cx| state.focus(window, cx));
        Mode::Edit
      },
      Mode::Edit => {
        window.focus(&self.focus, cx);
        Mode::Preview
      },
    };
    self.mode = mode;
    let markdown_mode = match mode {
      Mode::Preview => MarkdownMode::Preview,
      Mode::Edit => MarkdownMode::Edit,
    };
    SettingsStore::update(&mut *cx, |settings| settings.markdown_mode = markdown_mode);
    cx.notify();
  }
  fn answer_family(&self, family: DomainFamily, cx: &mut Context<Self>) {
    self.requests.update(cx, |requests, cx| requests.answer_allow(family, cx));
  }

  fn answer_always(&self, cx: &mut Context<Self>) {
    self.requests.update(cx, PermissionRequests::answer_always);
  }
  fn apply_permission_answer(&mut self, answer: PermissionAnswer, window: &Window, cx: &mut Context<Self>) {
    match answer {
      PermissionAnswer::Allow(family) => self.allow_family(&family, window, cx),
      PermissionAnswer::Always => self.allow_always(window, cx),
    }
  }

  fn allow_family(&mut self, family: &DomainFamily, window: &Window, cx: &mut Context<Self>) {
    SettingsStore::update(&mut *cx, |settings| settings.allow_family(family));
    self.requests.update(cx, |requests, cx| requests.dismiss_family(family, cx));
    self.image_cache.update(cx, |cache, cx| cache.retry_family(family, window, cx));
    self.schema_cache.update(cx, |cache, cx| cache.retry_family(family, window, cx));
    self.start_schema_validation(window, cx);
    cx.notify();
  }

  fn allow_always(&mut self, window: &Window, cx: &mut Context<Self>) {
    SettingsStore::update(&mut *cx, |settings| settings.allow_remote = true);
    self.requests.update(cx, PermissionRequests::dismiss_all);
    self.image_cache.update(cx, |cache, cx| cache.retry_all(window, cx));
    self.schema_cache.update(cx, |cache, cx| cache.retry_all(window, cx));
    self.start_schema_validation(window, cx);
    cx.notify();
  }
  fn ensure_preview(&mut self, cx: &mut Context<Self>) -> Option<Entity<TextViewState>> {
    if self.preview_revision == Some(self.session.revision) {
      return self.preview.clone();
    }
    let snapshot = self.snapshot(cx)?;
    let mut source = String::with_capacity(snapshot.text.len().saturating_add(PREVIEW_SPACER.len()));
    source.extend(snapshot.text.chunks());
    source.push_str(PREVIEW_SPACER);
    let preview = match &self.preview {
      Some(preview) => {
        preview.update(cx, |state, cx| state.set_text(&source, cx));
        preview.clone()
      },
      None => cx.new(|cx| TextViewState::markdown(&source, cx)),
    };
    if self.preview_observation.is_none() {
      self.preview_observation = Some(cx.observe(&preview, |_, _, cx| cx.notify()));
    }
    self.preview = Some(preview.clone());
    self.preview_revision = Some(snapshot.revision);
    Some(preview)
  }

  fn json_family(&self) -> Option<JsonFamily> {
    if self.session.has_path() {
      JsonFamily::from_path(&self.session.path)
    } else if self.session.kind.language() == Some("json") {
      Some(JsonFamily::Json)
    } else {
      None
    }
  }

  fn schema_status(&self, cx: &App) -> Option<String> {
    self.json_family()?;
    Some(self.schema_cache.read(cx).status_name())
  }

  fn schedule_schema_validation(&mut self, cx: &Context<Self>) {
    if self.json_family().is_none() {
      self.schema_waiting = false;
      self.schema_validate_task = None;
      return;
    }
    self.schema_waiting = false;
    self.schema_validate_task = Some(cx.spawn(async move |this, cx| {
      cx.background_executor().timer(CHECKPOINT_DELAY).await;
      let _ = this.update_in(cx, |view, window, cx| view.start_schema_validation(window, cx));
    }));
  }

  pub(crate) fn start_schema_validation(&mut self, window: &Window, cx: &mut Context<Self>) {
    let Some(family) = self.json_family() else {
      self.schema_waiting = false;
      return;
    };
    if self.session.closing {
      return;
    }
    let Some(rope) = self.buffer_text(cx) else {
      return;
    };
    let text = rope_to_string(&rope);
    let parsed = schema::parse(family, &text);
    let path = self.session.path.clone();
    let revision = self.session.revision;
    let pick = self.schema_pick.clone();
    let (pending, compiled) = self.schema_cache.update(cx, |cache, cx| {
      cache.load_document(&path, parsed.as_ref().ok(), &text, pick.as_deref(), window, cx);
      let pending = cache.prepare_compiled();
      (pending, cache.compiled())
    });
    if pending {
      self.schema_waiting = true;
      return;
    }
    self.schema_waiting = false;
    self.schema_apply_task = Some(cx.spawn(async move |this, cx| {
      let issues = cx
        .background_spawn(async move { collect_issues(parsed.as_ref(), compiled.as_deref()) })
        .await;
      let _ = this.update(cx, |view, cx| view.apply_schema_issues(revision, issues, cx));
    }));
  }

  fn apply_schema_issues(
    &self,
    revision: Revision,
    issues: Vec<openit_core::schema::SchemaDiagnostic>,
    cx: &mut Context<Self>,
  ) {
    if self.session.revision != revision {
      return;
    }
    let Some(editor) = &self.editor else {
      return;
    };
    editor.update(cx, |state, _cx| {
      let rope = state.text().clone();
      let Some(set) = state.diagnostics_mut() else {
        return;
      };
      set.reset(&rope);
      for issue in issues {
        let start = rope.offset_to_position(issue.range.start);
        let end = rope.offset_to_position(issue.range.end);
        set.push(Diagnostic::new(start..end, issue.message).with_severity(DiagnosticSeverity::Error));
      }
    });
    cx.notify();
  }

  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  /// Native-looking title bar: title beside the traffic lights, document actions on the right.
  fn render_title_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    let title = file_name(
      self.title(),
      self.is_dirty(),
      cx,
      cx.listener(|this, _, window, cx| this.open_nearby_picker(&GoToFile, window, cx)),
    );
    let mut actions = div()
      .flex()
      .items_center()
      .gap_2()
      // Clicks on these belong to the buttons: without this the title bar sees
      // them and macOS zooms the window on the second one.
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if self.is_markdown() {
      // The button carries the mode a click moves to: a pen to write, an eye to read.
      let (icon, tip) = match self.mode {
        Mode::Preview => (Icon::empty().path("icons/pencil.svg"), "Edit (Cmd+Shift+E)"),
        Mode::Edit => (Icon::new(IconName::Eye), "Preview (Cmd+Shift+E)"),
      };
      actions = actions.child(toolbar_button(
        "mode-toggle",
        icon,
        tip,
        cx,
        cx.listener(|this, _, window, cx| this.toggle_mode(&ToggleMode, window, cx)),
      ));
    }
    // The theme picker lives in View > Color Theme... and Cmd+K Cmd+T.
    // Match the document surface and remove the component's default divider.
    TitleBar::new().border_0().bg(theme.background).child(
      div()
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .h_full()
        .pr_2()
        .child(title)
        .child(actions),
    )
  }
  /// The caret readout entity, which only the editor shows. Preview reports
  /// nothing, where a stale line and column would sit there unchanged.
  fn caret_readout(&self) -> Option<Entity<CursorStatus>> {
    self.cursor_status_view.as_ref().filter(|_| self.mode == Mode::Edit).cloned()
  }
  fn settings_error_message(cx: &App) -> Option<String> {
    cx.try_global::<SettingsStore>()
      .and_then(|store| store.last_error())
      .map(|error| format!("Settings could not be saved: {error}"))
  }
  /// Whether the status bar carries a warning or an error the reader must see.
  fn status_bar_reports_a_problem(&self, cx: &App) -> bool {
    (self.external_change() && self.source_status().is_some())
      || self.last_error().is_some()
      || Self::settings_error_message(cx).is_some()
  }
  /// Whether the status bar is drawn. Preview keeps the page clean by hiding it,
  /// unless the reader pinned it visible or it has a problem to report.
  fn shows_status_bar(&self, cx: &App) -> bool {
    self.mode == Mode::Edit
      || cx
        .try_global::<AppSettings>()
        .is_some_and(|settings| settings.0.always_show_status_bar)
      || self.status_bar_reports_a_problem(cx)
  }
  /// Whether the status bar floats over the buffer. It stays in normal flow when
  /// the permission bar is up, so Allow is not covered.
  fn status_bar_overlays_the_buffer(&self, cx: &App) -> bool {
    self.mode == Mode::Edit && self.requests.read(cx).is_empty()
  }
  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cx.listener requires the mutable Context signature"
  )]
  fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    let language = language_label(self.language());
    let settings_error = Self::settings_error_message(cx);
    let segment = |id: &'static str, text: String| {
      div()
        .id(id)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|s| s.bg(theme.muted))
        .child(text)
    };
    // Over the text while editing, where a translucent bar reads well against
    // a caret and a scrolling buffer. Everywhere else it takes its own row, so
    // nothing lands underneath it, including the permission bar.
    let overlay = self.status_bar_overlays_the_buffer(cx);
    div()
      .when(overlay, |bar| bar.absolute().left_0().right_0().bottom_0())
      .flex_shrink_0()
      .flex()
      .items_center()
      .gap_4()
      .h_6()
      .px_3()
      .bg(if overlay {
        theme.background.opacity(0.92)
      } else {
        theme.background
      })
      .text_xs()
      .text_color(theme.muted_foreground)
      .children(self.caret_readout().map(|status| {
        div()
          .id("status-position")
          .px_1()
          .rounded_sm()
          .cursor_pointer()
          .hover(|s| s.bg(theme.muted))
          .child(status)
          .on_click(cx.listener(|this, _, window, cx| this.open_go_to_line(window, cx)))
      }))
      .when(self.external_change(), |bar| match self.source_status() {
        Some(message) => bar.child(div().text_color(theme.warning_foreground).child(message)),
        None => bar,
      })
      .when_some(self.last_error(), |bar, error| {
        bar.child(div().text_color(theme.danger).child(error.to_owned()))
      })
      .when_some(settings_error, |bar, error| {
        bar.child(div().text_color(theme.danger).child(error))
      })
      .child(div().flex_1())
      .children(self.schema_status(cx).map(|name| {
        segment("status-schema", name).on_click(cx.listener(|this, _, window, cx| this.open_schema_picker(window, cx)))
      }))
      .child(
        segment("status-language", language)
          .on_click(cx.listener(|this, _, window, cx| this.open_language_picker(window, cx))),
      )
  }
  fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
    match self.mode {
      Mode::Edit => {
        let editor = self.ensure_editor(window, cx);
        div()
          .flex_1()
          .min_h_0()
          .child(Editor::new(&editor).bordered(false).size_full())
          .into_any_element()
      },
      Mode::Preview => {
        let Some(preview) = self.ensure_preview(cx) else {
          return div().flex_1().into_any_element();
        };
        let palette = cx.global::<ActivePalette>().0;
        let style = TextViewStyle {
          paragraph_gap: gpui_kit::rems(1.25),
          heading_base_font_size: gpui_kit::px(16.),
          ..TextViewStyle::default()
        }
        .inline_code(gpui_kit::HighlightStyle {
          background_color: Some(hsla(palette.muted)),
          color: Some(hsla(palette.warning)),
          ..Default::default()
        })
        .code_block(
          gpui_kit::StyleRefinement::default()
            .bg(hsla(palette.sidebar))
            .border_1()
            .border_color(hsla(palette.border))
            .rounded_md()
            .p_3(),
        )
        .table(
          // Measured column widths with word wrapping (gpui-base's auto layout), opted in through overflow-x.
          {
            let mut table = gpui_kit::StyleRefinement::default();
            table.overflow.x = Some(gpui_kit::Overflow::Scroll);
            table
          }
          .border_1()
          .border_color(hsla(palette.border))
          .rounded_md(),
        )
        .table_head(gpui_kit::StyleRefinement::default().bg(hsla(palette.sidebar)))
        .table_cell(
          gpui_kit::StyleRefinement::default()
            .border_color(hsla(palette.border))
            .px_3()
            .py_1p5(),
        );
        let viewport = window.viewport_size().width;
        let preview_width = markdown_preview_width(cx.global::<AppSettings>().0.markdown_preview_width, viewport);
        // A div, not gpui's `image_cache` element: that one skips the cache during prepaint,
        // and inline images (`<img>` in a paragraph) lay out in prepaint, so they would fall
        // back to gpui's global loader and treat document-relative paths as URLs.
        div()
          .flex_1()
          .min_h_0()
          .text_size(gpui_kit::px(16.))
          .line_height(gpui_kit::relative(1.6))
          .child(
            div().image_cache(self.image_cache.clone()).size_full().child(
              TextView::new(&preview)
                .style(style)
                .selectable(true)
                .scrollable(true)
                .px((viewport - preview_width) / 2.)
                .selection_format(SelectionFormat::Source)
                .on_link_click(|url, _event, _window, cx| {
                  if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                    cx.open_url(url);
                  }
                }),
            ),
          )
          .into_any_element()
      },
    }
  }
  fn permission_button(
    id: &'static str,
    label: String,
    hover: gpui_kit::Hsla,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
  ) -> impl IntoElement {
    div()
      .id(id)
      .px_2()
      .py_0p5()
      .rounded_sm()
      .cursor_pointer()
      .hover(move |style| style.bg(hover))
      .on_click(on_click)
      .child(label)
  }

  fn render_permission_bar(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
    let requests = self.requests.read(cx);
    if requests.is_empty() {
      return None;
    }
    let first = requests.pending.first()?;
    let family = first.family.clone();
    let count = requests.pending.len();
    let theme = cx.theme();
    let more = if count > 1 {
      format!(" and {} more", count - 1)
    } else {
      String::new()
    };
    let allow_family = family.clone();
    let allow_once = Self::permission_button(
      "allow-family",
      format!("Allow {family}"),
      theme.muted,
      cx.listener(move |this, _, _, cx| this.answer_family(allow_family.clone(), cx)),
    );
    let allow_always = Self::permission_button(
      "allow-always",
      "Always allow remote content".to_owned(),
      theme.muted,
      cx.listener(|this, _, _, cx| this.answer_always(cx)),
    );
    Some(
      div()
        .flex()
        .items_center()
        .gap_3()
        .h_8()
        .px_3()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .text_sm()
        .child(format!("This document wants content from {family}{more}"))
        .child(div().flex_1())
        .child(allow_once)
        .child(allow_always),
    )
  }
}
impl Render for DocumentView {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.schema_cache.update(cx, |cache, cx| cache.pump(window, cx));
    if self.json_family().is_some()
      && !self.schema_ask_opened
      && self.overlay.is_none()
      && matches!(self.schema_cache.read(cx).selection(), SchemaSelection::Ask(_))
    {
      self.schema_ask_opened = true;
      self.schema_ask_task = Some(cx.spawn(async move |this, cx| {
        let _ = this.update_in(cx, Self::open_schema_picker);
      }));
    }
    let shows_status_bar = self.shows_status_bar(cx);
    let theme = cx.theme();
    div()
      .key_context("DocumentView")
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::toggle_mode))
      .on_action(cx.listener(Self::save))
      .on_action(cx.listener(Self::close))
      .on_action(cx.listener(Self::open_theme_picker))
      .on_action(cx.listener(Self::open_ui_font_picker))
      .on_action(cx.listener(Self::open_code_font_picker))
      .on_action(cx.listener(Self::open_nearby_picker))
      .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| apply_external_paths(paths, cx)))
      .drag_over::<ExternalPaths>(|style, _, _, cx| external_paths_ring(style, cx))
      .relative()
      .flex()
      .flex_col()
      .size_full()
      .bg(theme.background)
      .text_color(theme.foreground)
      .child(gpui_kit::base::TextSelectionLayer)
      .when(!self.embedded, |root| root.child(self.render_title_row(cx)))
      .child(self.render_body(window, cx))
      .children(self.render_permission_bar(cx))
      .when(shows_status_bar, |root| root.child(self.render_status_bar(cx)))
      .children(self.overlay.as_ref().map(Overlay::element))
  }
}
#[cfg(test)]
pub(crate) mod tests {
  use std::{fs, sync::Arc, time::Duration};

  use crate::actions::CloseWindow;
  use crate::cache::ResourceCacheHandle;
  use crate::fetch::{FakeFetcher, FetchError, Fetcher};
  use crate::image_cache::{
    DocumentImageCache, Entry, PermissionRequests, PlaceholderKind, released_image_count_for_test,
    reset_released_image_count_for_test,
  };
  use crate::schema_cache::DocumentSchemaCache;
  use gpui_kit::component::highlighter::DiagnosticSeverity;
  use gpui_kit::component::theme::Theme;
  use gpui_kit::{
    AppContext, BorrowAppContext, Context, Entity, EntityId, ImageCacheError, KeyBinding, Keystroke, RenderImage,
    Resource, TestAppContext, VisualTestContext, Window, WindowAppearance,
  };
  use openit_core::document::{Loaded, Revision, load_text};
  use openit_core::kind::DocumentKind;
  use openit_core::recovery::{Draft, RecoveryStore};
  use openit_core::resource::{DenyReason, DomainFamily, Resolved};
  use openit_core::save::Saved;
  use openit_core::session::SessionId;
  use openit_core::settings::{MarkdownMode, MarkdownPreviewWidth, Settings, ThemeMode};
  use openit_core::watch::Fingerprint;
  use std::collections::HashMap;
  use std::path::PathBuf;

  use super::{DocumentSession, DocumentView, Mode, markdown_preview_width};
  use crate::session::{CHECKPOINT_DELAY, PendingCleanups, Recovery};
  use crate::settings::{AUTOSAVE_DELAY, AppSettings, SettingsStore, watch_settings_for_test};
  use crate::status_pickers::SchemaPickerEvent;

  #[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI test setup uses a mutable application context"
  )]
  pub(crate) fn install_globals(cx: &mut TestAppContext) -> (tempfile::TempDir, Arc<RecoveryStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(RecoveryStore::open(dir.path()).unwrap());
    let resource_cache_dir = tempfile::tempdir().unwrap();
    let resource_cache = ResourceCacheHandle::from_temp_dir(resource_cache_dir).unwrap();
    cx.update(|cx| {
      cx.set_global(Recovery(Some(store.clone())));
      cx.set_global(PendingCleanups::default());
      cx.set_global(crate::QuitInProgress::default());
      cx.set_global(crate::QuitCommitted::default());
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(None));
      cx.set_global(crate::theme::ThemeDirs { user: Some(dir.path().to_path_buf()) });
      crate::theme::init(cx);
      crate::install_app_settings_observer(cx);
      cx.set_global(resource_cache);
      cx.set_global(Fetcher(Arc::new(FakeFetcher::new(HashMap::new()))));
    });
    (dir, store)
  }

  fn select_all(cx: &mut VisualTestContext) {
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-a");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-a");
  }

  fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut output = std::io::Cursor::new(Vec::new());
    image.write_to(&mut output, image::ImageFormat::Png).unwrap();
    output.into_inner()
  }

  fn set_settings_path(_view: &Entity<DocumentView>, path: PathBuf, cx: &mut VisualTestContext) {
    cx.update(|_, cx| SettingsStore::set_path(cx, path));
  }

  fn install_fetcher(responses: HashMap<String, Result<Vec<u8>, FetchError>>, cx: &TestAppContext) -> Arc<FakeFetcher> {
    let fetcher = Arc::new(FakeFetcher::new(responses));
    cx.update(|cx| cx.set_global(Fetcher(fetcher.clone())));
    fetcher
  }

  fn image_cache(view: &Entity<DocumentView>, cx: &VisualTestContext) -> Entity<DocumentImageCache> {
    view.read_with(cx, |view, _| view.image_cache.clone())
  }

  fn requests(view: &Entity<DocumentView>, cx: &VisualTestContext) -> Entity<PermissionRequests> {
    view.read_with(cx, |view, _| view.requests.clone())
  }

  fn schema_cache(view: &Entity<DocumentView>, cx: &VisualTestContext) -> Entity<DocumentSchemaCache> {
    view.read_with(cx, |view, _| view.schema_cache.clone())
  }

  fn pump_schemas(view: &Entity<DocumentView>, cx: &mut VisualTestContext) {
    let cache = schema_cache(view, cx);
    cx.update(|window, cx| cache.update(cx, |cache, cx| cache.pump(window, cx)));
  }

  fn settle_schemas(view: &Entity<DocumentView>, cx: &mut VisualTestContext) {
    for _ in 0..40 {
      pump_schemas(view, cx);
      cx.run_until_parked();
    }
    cx.update(|window, cx| view.update(cx, |view, cx| view.start_schema_validation(window, cx)));
    cx.run_until_parked();
  }

  fn extra_key_json(schema: &str) -> String {
    format!("{{\n  \"$schema\": \"{schema}\",\n  \"extra\": true\n}}\n")
  }

  fn diagnostic_entries(
    view: &Entity<DocumentView>,
    cx: &VisualTestContext,
  ) -> Vec<(std::ops::Range<usize>, DiagnosticSeverity, String)> {
    view.read_with(cx, |view, cx| {
      view
        .editor
        .as_ref()
        .and_then(|editor| {
          editor.read(cx).diagnostics().map(|set| {
            set
              .iter()
              .map(|entry| (entry.range.clone(), entry.severity, entry.message.to_string()))
              .collect()
          })
        })
        .unwrap_or_default()
    })
  }

  fn schema_json(schema: &str) -> String {
    format!("{{\n  \"$schema\": \"{schema}\"\n}}\n")
  }

  fn schema_completion_labels(view: &Entity<DocumentView>, offset: usize, cx: &mut VisualTestContext) -> Vec<String> {
    use lsp_types::{CompletionContext, CompletionResponse, CompletionTriggerKind};
    let task = cx.update(|window, cx| {
      let editor = view.read(cx).editor.as_ref().expect("editor").clone();
      let provider = editor
        .read(cx)
        .lsp()
        .completion_provider
        .clone()
        .expect("schema completion provider");
      let rope = editor.read(cx).text().clone();
      provider.completions(
        &rope,
        offset,
        CompletionContext {
          trigger_kind: CompletionTriggerKind::INVOKED,
          trigger_character: None,
        },
        window,
        cx,
      )
    });
    let response = cx.foreground_executor().block_test(task).expect("completions");
    match response {
      CompletionResponse::Array(items) => items.into_iter().map(|item| item.label).collect(),
      CompletionResponse::List(list) => list.items.into_iter().map(|item| item.label).collect(),
    }
  }

  fn completion_schema_body() -> Vec<u8> {
    br#"{
      "type":"object",
      "properties":{
        "$schema":{"type":"string"},
        "status":{"enum":["draft","live"]},
        "author":{"type":"object","properties":{"name":{"type":"string"},"email":{"type":"string"}}}
      }
    }"#
      .to_vec()
  }

  fn load_image(
    view: &Entity<DocumentView>,
    resource: Resource,
    cx: &mut VisualTestContext,
  ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    let cache = image_cache(view, cx);
    let result = cx.update(|window, cx| cache.update(cx, |cache, cx| cache.load(&resource, window, cx)));
    drop(resource);
    result
  }

  fn has_entry(
    view: &Entity<DocumentView>,
    resource: &Resource,
    cx: &VisualTestContext,
    predicate: impl FnOnce(&Entry) -> bool,
  ) -> bool {
    let cache = image_cache(view, cx);
    cache.read_with(cx, |cache, _| cache.entry_for_test(resource).is_some_and(predicate))
  }

  #[gpui_kit::test]
  fn text_file_fills_the_editor_clean(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    let loaded = load_text(&path).unwrap();

    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    view.read_with(cx, |view, cx| {
      let snapshot = view.snapshot(cx).expect("text document has a buffer");
      assert_eq!(snapshot.text.to_string(), "fn main() {}\n");
      assert!(!view.is_dirty());
      assert_eq!(view.title(), "main.rs");
    });
  }

  #[gpui_kit::test]
  fn a_window_appearance_change_reapplies_the_theme(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| {
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.update(|window, cx| {
      crate::theme::apply_for_appearance(WindowAppearance::Dark, Some(window), cx);
    });
    assert!(cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));
    cx.update(|window, cx| {
      crate::theme::apply_for_appearance(WindowAppearance::Light, Some(window), cx);
    });
    assert!(!cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));
    cx.refresh().unwrap();
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert_eq!(view.mode(), Mode::Preview));
  }

  #[gpui_kit::test]
  fn changing_the_settings_theme_reapplies_without_restart(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let sender = watch_settings_for_test(settings_path.clone(), cx);
    cx.update(|cx| {
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
      cx.update_global::<AppSettings, _>(|settings, _| settings.0.theme.mode = ThemeMode::Dark);
      crate::theme::apply_for_appearance(WindowAppearance::Dark, None, cx);
    });
    let document_dir = tempfile::tempdir().unwrap();
    let path = document_dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert!(cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));

    fs::write(&settings_path, "[theme]\nmode = \"light\"\n").unwrap();
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    assert!(!cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));
  }
  #[gpui_kit::test]
  fn typing_after_open_edits_the_buffer_and_marks_dirty(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.txt");
    fs::write(&path, "").unwrap();
    let loaded = load_text(&path).unwrap();

    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_input("ab");
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      let snapshot = view.snapshot(cx).expect("text document has a buffer");
      assert!(snapshot.text.to_string().starts_with("ab"));
      assert!(view.is_dirty());
    });
  }
  #[gpui_kit::test]
  fn markdown_opens_in_preview_without_an_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();

    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    view.read_with(cx, |view, cx| {
      assert_eq!(view.mode(), super::Mode::Preview);
      assert!(!view.has_editor());
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "# Hi\n");
    });
  }

  #[gpui_kit::test]
  fn shortcut_toggles_mode_from_preview_and_back(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-shift-e", crate::actions::ToggleMode, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Edit);
    let first_editor = view.read_with(cx, |view, _| view.editor_entity_id());

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Preview);

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Edit);
    assert_eq!(view.read_with(cx, |view, _| view.editor_entity_id()), first_editor);
  }
  #[test]
  fn markdown_preview_width_clamps_presets_with_minimum_side_padding() {
    let viewport = gpui_kit::px(1200.);

    assert_eq!(
      markdown_preview_width(MarkdownPreviewWidth::Readable, viewport),
      gpui_kit::px(700.)
    );
    assert_eq!(markdown_preview_width(MarkdownPreviewWidth::Wide, viewport), gpui_kit::px(960.));
    assert_eq!(
      markdown_preview_width(MarkdownPreviewWidth::FullWidth, viewport),
      gpui_kit::px(1152.)
    );
    assert_eq!(
      markdown_preview_width(MarkdownPreviewWidth::Readable, gpui_kit::px(600.)),
      gpui_kit::px(552.)
    );
    assert_eq!(
      markdown_preview_width(MarkdownPreviewWidth::Wide, gpui_kit::px(600.)),
      gpui_kit::px(552.)
    );
  }

  #[gpui_kit::test]
  fn toggling_markdown_persists_the_default_for_the_next_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.md");
    let second_path = dir.path().join("second.md");
    fs::write(&first_path, "# First\n").unwrap();
    fs::write(&second_path, "# Second\n").unwrap();

    let first_loaded = load_text(&first_path).unwrap();
    let (first_view, cx) = cx
      .add_window_view(|window, cx| DocumentView::open(first_path.clone(), first_loaded, SessionId::new(), window, cx));
    set_settings_path(&first_view, settings_path.clone(), cx);

    assert_eq!(first_view.read_with(cx, |view, _| view.mode()), Mode::Preview);
    cx.update(|window, cx| first_view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    cx.run_until_parked();

    assert_eq!(first_view.read_with(cx, |view, _| view.mode()), Mode::Edit);
    assert_eq!(Settings::load(&settings_path).unwrap().markdown_mode, MarkdownMode::Edit);

    let second_loaded = load_text(&second_path).unwrap();
    let (second_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(second_path, second_loaded, SessionId::new(), window, cx));
    second_view.read_with(cx, |view, _| {
      assert_eq!(view.mode(), Mode::Edit);
      assert!(view.has_editor());
    });
    assert_eq!(first_view.read_with(cx, |view, _| view.mode()), Mode::Edit);
  }

  #[gpui_kit::test]
  fn opening_non_markdown_does_not_override_the_markdown_default(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| SettingsStore::update(cx, |settings| settings.markdown_mode = MarkdownMode::Edit));
    cx.run_until_parked();

    let dir = tempfile::tempdir().unwrap();
    let text_path = dir.path().join("notes.txt");
    let markdown_path = dir.path().join("notes.md");
    fs::write(&text_path, "plain text\n").unwrap();
    fs::write(&markdown_path, "# Markdown\n").unwrap();

    let text_loaded = load_text(&text_path).unwrap();
    let (text_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(text_path, text_loaded, SessionId::new(), window, cx));
    cx.update(|window, cx| text_view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    cx.run_until_parked();

    assert_eq!(text_view.read_with(cx, |view, _| view.mode()), Mode::Edit);
    assert_eq!(
      cx.read_global::<AppSettings, _>(|settings, _| settings.0.markdown_mode),
      MarkdownMode::Edit
    );

    let markdown_loaded = load_text(&markdown_path).unwrap();
    let (markdown_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(markdown_path, markdown_loaded, SessionId::new(), window, cx));
    assert_eq!(markdown_view.read_with(cx, |view, _| view.mode()), Mode::Edit);
  }

  #[gpui_kit::test]
  fn changing_the_markdown_default_does_not_switch_existing_views(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.md");
    let second_path = dir.path().join("second.md");
    fs::write(&first_path, "# First\n").unwrap();
    fs::write(&second_path, "# Second\n").unwrap();

    let first_loaded = load_text(&first_path).unwrap();
    let (first_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(first_path, first_loaded, SessionId::new(), window, cx));
    assert_eq!(first_view.read_with(cx, |view, _| view.mode()), Mode::Preview);

    cx.update(|_, cx| SettingsStore::update(cx, |settings| settings.markdown_mode = MarkdownMode::Edit));
    cx.run_until_parked();
    assert_eq!(first_view.read_with(cx, |view, _| view.mode()), Mode::Preview);

    let second_loaded = load_text(&second_path).unwrap();
    let (second_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(second_path, second_loaded, SessionId::new(), window, cx));
    assert_eq!(second_view.read_with(cx, |view, _| view.mode()), Mode::Edit);
  }

  #[gpui_kit::test]
  fn restored_markdown_uses_the_default_and_keeps_its_saved_cursor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| SettingsStore::update(cx, |settings| settings.markdown_mode = MarkdownMode::Edit));
    cx.run_until_parked();
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: "abc\ndef".to_owned(),
      cursor: 4,
      image: None,
      schema: None,
    };

    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Markdown, window, cx));
    view.read_with(cx, |view, cx| {
      assert_eq!(view.mode(), Mode::Edit);
      let editor = view.editor.as_ref().expect("edit default creates an editor");
      assert_eq!(editor.read(cx).cursor(), 4);
    });
  }

  #[gpui_kit::test]
  fn chord_opens_the_theme_picker_and_escape_closes_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-t", crate::actions::ColorTheme, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_keystrokes("cmd-k cmd-t");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| matches!(view.overlay, Some(super::Overlay::Theme(_)))));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.overlay.is_none()));
  }

  #[gpui_kit::test]
  fn ui_font_chord_opens_from_a_focused_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-u", crate::actions::UiFont, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "hello\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_keystrokes("cmd-k cmd-u");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, cx| match &view.overlay {
      Some(super::Overlay::Font(picker)) => picker.read(cx).slot() == crate::font_picker::FontSlot::Ui,
      _ => false,
    }));
    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "hello\n");
    });
  }

  #[gpui_kit::test]
  fn code_font_chord_opens_from_a_focused_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-c", crate::actions::CodeFont, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "hello\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_keystrokes("cmd-k cmd-c");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, cx| match &view.overlay {
      Some(super::Overlay::Font(picker)) => picker.read(cx).slot() == crate::font_picker::FontSlot::Code,
      _ => false,
    }));
    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "hello\n");
    });
  }

  #[gpui_kit::test]
  fn ui_font_opener_opens_the_font_picker_and_escape_closes_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.update(|window, cx| {
      view.update(cx, |view, cx| {
        view.open_font_picker(crate::font_picker::FontSlot::Ui, window, cx)
      });
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| matches!(view.overlay, Some(super::Overlay::Font(_)))));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.overlay.is_none()));
  }

  #[gpui_kit::test]
  fn cmd_p_opens_the_nearby_picker_from_a_focused_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-p", crate::actions::GoToFile, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "hello\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| matches!(view.overlay, Some(super::Overlay::Nearby(_)))));
    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "hello\n");
    });
  }

  #[gpui_kit::test]
  fn language_picker_changes_the_highlighting_language(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "fn main() {}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert_eq!(view.read_with(cx, |view, _| view.language()), "text");

    cx.update(|window, cx| view.update(cx, |view, cx| view.open_language_picker(window, cx)));
    let picker = view.read_with(cx, |view, _| match &view.overlay {
      Some(super::Overlay::Language(picker)) => picker.clone(),
      _ => panic!("language picker expected"),
    });
    cx.update(|_, cx| picker.update(cx, |_, cx| cx.emit(crate::status_pickers::LanguagePickerEvent::Picked("rust"))));
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.overlay.is_none()));
    assert_eq!(view.read_with(cx, |view, _| view.language()), "rust");
  }
  #[gpui_kit::test]
  fn changing_language_preserves_undo_history_and_selection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "hello").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.update(|_, cx| {
      view.update(cx, |view, cx| {
        view
          .editor
          .as_ref()
          .expect("text documents create an editor")
          .update(cx, |state, cx| state.set_selected_range(2..2, cx));
      });
    });
    cx.simulate_input("X");
    cx.run_until_parked();

    let (typed_selection, typed_revision) = view.read_with(cx, |view, cx| {
      let editor = view.editor.as_ref().expect("editor remains available");
      (editor.read(cx).selected_range(), view.revision())
    });
    assert_eq!(typed_selection, 3..3);
    cx.update(|_, cx| {
      view.update(cx, |view, cx| {
        view
          .editor
          .as_ref()
          .expect("editor remains available")
          .update(cx, |state, cx| state.set_selected_range(1..3, cx));
      });
    });
    let selected_range = view.read_with(cx, |view, cx| {
      view
        .editor
        .as_ref()
        .expect("editor remains available")
        .read(cx)
        .selected_range()
    });
    assert_eq!(selected_range, 1..3);

    cx.update(|window, cx| view.update(cx, |view, cx| view.set_language("rust", window, cx)));
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
      let editor = view.editor.as_ref().expect("language changes keep the selection");
      assert_eq!(editor.read(cx).value().to_string(), "heXllo");
      assert_eq!(editor.read(cx).selected_range(), selected_range);
      assert_eq!(view.revision(), typed_revision);
      assert!(view.is_dirty());
    });

    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-z");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
      let editor = view.editor.as_ref().expect("undo keeps the editor");
      assert_eq!(editor.read(cx).value().to_string(), "hello");
      assert_eq!(editor.read(cx).selected_range(), 2..2);
    });
  }

  #[gpui_kit::test]
  fn picking_a_non_markdown_language_leaves_preview_and_markdown_restores_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Preview);

    cx.update(|window, cx| view.update(cx, |view, cx| view.set_language("text", window, cx)));
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
      assert_eq!(view.mode(), super::Mode::Edit);
      assert!(!view.is_markdown());
      let editor = view.editor.as_ref().expect("editor exists after leaving preview");
      assert_eq!(editor.read(cx).value().to_string(), "# Hi\n");
    });

    cx.update(|window, cx| view.update(cx, |view, cx| view.set_language("markdown", window, cx)));
    cx.run_until_parked();
    view.read_with(cx, |view, cx| {
      assert!(view.is_markdown());
      assert_eq!(view.mode(), super::Mode::Edit);
      let editor = view.editor.as_ref().expect("editor survives the switch");
      assert_eq!(editor.read(cx).value().to_string(), "# Hi\n");
    });
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&super::ToggleMode, window, cx)));
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Preview);
  }

  #[gpui_kit::test]
  fn preview_hides_the_status_bar_unless_it_is_pinned_or_has_a_problem(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Preview);
    assert!(!view.read_with(cx, super::DocumentView::shows_status_bar));

    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&super::ToggleMode, window, cx)));
    assert!(
      view.read_with(cx, super::DocumentView::shows_status_bar),
      "edit keeps the status bar"
    );
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&super::ToggleMode, window, cx)));
    assert!(!view.read_with(cx, super::DocumentView::shows_status_bar));

    cx.update(|_, cx| view.update(cx, |view, _| view.session.last_error = Some("Could not save".to_owned())));
    assert!(
      view.read_with(cx, super::DocumentView::shows_status_bar),
      "an error stays visible in preview"
    );
    cx.update(|_, cx| view.update(cx, |view, _| view.session.last_error = None));
    assert!(!view.read_with(cx, super::DocumentView::shows_status_bar));

    cx.update(|_, cx| SettingsStore::update(cx, |settings| settings.always_show_status_bar = true));
    cx.run_until_parked();
    assert!(
      view.read_with(cx, super::DocumentView::shows_status_bar),
      "the pinned setting wins over preview"
    );
  }

  #[gpui_kit::test]
  fn the_caret_readout_is_absent_in_preview(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.update(|_, cx| SettingsStore::update(cx, |settings| settings.always_show_status_bar = true));
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.caret_readout().is_none()));

    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&super::ToggleMode, window, cx)));

    let readout = view
      .read_with(cx, |view, _| view.caret_readout())
      .expect("the editor shows the caret");
    assert_eq!(cx.update(|_, cx| readout.read(cx).editor.read(cx).cursor_position().line), 0);

    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&super::ToggleMode, window, cx)));

    assert!(
      view.read_with(cx, |view, _| view.caret_readout().is_none()),
      "returning to preview drops the caret readout"
    );
  }

  #[gpui_kit::test]
  fn go_to_line_moves_the_caret(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.update(|window, cx| view.update(cx, |view, cx| view.open_go_to_line(window, cx)));
    let prompt = view.read_with(cx, |view, _| match &view.overlay {
      Some(super::Overlay::GoToLine(prompt)) => prompt.clone(),
      _ => panic!("go-to-line prompt expected"),
    });
    cx.update(|_, cx| {
      prompt.update(cx, |_, cx| {
        cx.emit(crate::status_pickers::GoToLineEvent::Jump(
          gpui_kit::component::input::Position::new(2, 3),
        ));
      });
    });
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.overlay.is_none()));
    let position = view.read_with(cx, |view, cx| view.editor.as_ref().unwrap().read(cx).cursor_position());
    assert_eq!((position.line, position.character), (2, 3));
  }

  #[gpui_kit::test]
  fn toggling_creates_the_editor_once_and_keeps_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    let first = view.read_with(cx, |view, _| view.editor_entity_id());
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    let third = view.read_with(cx, |view, _| view.editor_entity_id());

    assert_eq!(view.read_with(cx, |view, _| view.mode()), super::Mode::Edit);
    assert!(first.is_some());
    assert_eq!(first, third, "the same EditorState survives Preview and back");
  }

  #[gpui_kit::test]
  fn preview_refreshes_only_when_the_revision_changed(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# Hi\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    let initial = view.read_with(cx, |view, _| view.preview_revision());
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    let after_no_edit = view.read_with(cx, |view, _| view.preview_revision());

    assert_eq!(initial, after_no_edit, "no edit, no reparse");
  }
  #[gpui_kit::test]
  fn large_markdown_preview_parses_in_the_background_and_notifies(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.md");
    let body = "# Heading\n\nA paragraph with enough content to exercise the background parser.\n\n".repeat(1024);
    fs::write(&path, body).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.run_until_parked();
    let item_count = view.read_with(cx, |view, cx| {
      view
        .preview
        .as_ref()
        .map_or(0, |preview| preview.read(cx).list_state().item_count())
    });

    assert!(item_count > 0, "background parsing should populate preview blocks");
  }

  #[gpui_kit::test]
  fn save_writes_the_buffer_and_clears_dirty(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("new text");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.is_dirty()));

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "new text");
    assert!(!view.read_with(cx, |view, _| view.is_dirty()));
  }

  #[gpui_kit::test]
  fn edits_during_a_save_stay_dirty(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("one");
    cx.run_until_parked();

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    // A second edit lands before the background write reports back.
    cx.simulate_input("two");
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "one");
    view.read_with(cx, |view, cx| {
      let text = view.snapshot(cx).unwrap().text.to_string();
      assert!(view.is_dirty(), "revision two is not on disk");
      assert!(text.starts_with("one"));
      assert_ne!(text, fs::read_to_string(&path).unwrap());
    });
  }

  #[gpui_kit::test]
  fn failed_save_keeps_the_buffer_and_reports(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("new");
    cx.run_until_parked();
    fs::remove_dir_all(dir.path()).unwrap();

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert!(view.is_dirty());
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "new");
      assert!(view.last_error().is_some_and(|e| e.contains("a.txt")));
    });
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn save_onto_a_read_only_file_reports_and_stays_dirty(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("new");
    cx.run_until_parked();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
      assert!(view.is_dirty());
      assert!(view.last_error().is_some_and(|error| error.contains("a.txt")));
    });
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn error_clears_after_a_successful_save(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("new");
    cx.run_until_parked();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.last_error().is_some()));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
      assert!(!view.is_dirty());
      assert!(view.last_error().is_none());
    });
  }
  #[gpui_kit::test]
  fn an_edit_checkpoints_after_the_delay_and_a_save_removes_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let session = SessionId::new();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, session, window, cx));

    cx.simulate_input("ab");
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty(), "no checkpoint before the delay");

    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    let drafts = store.list().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].session, session);
    assert_eq!(drafts[0].path.as_deref(), Some(path.as_path()));
    assert!(drafts[0].text.starts_with("ab"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "old", "recovery never writes the original");

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty(), "a clean document has no draft");
  }

  #[gpui_kit::test]
  fn rapid_edits_coalesce_into_one_checkpoint(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));

    cx.simulate_input("a");
    cx.executor().advance_clock(CHECKPOINT_DELAY / 2);
    cx.simulate_input("b");
    cx.executor().advance_clock(CHECKPOINT_DELAY / 2);
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty(), "the second edit restarted the delay");

    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    let drafts = store.list().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "ab");
  }

  #[gpui_kit::test]
  fn restore_opens_dirty_with_the_draft_text_and_never_touches_the_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("notes.md");
    fs::write(&path, "# on disk").unwrap();
    let draft = Draft {
      session: SessionId::new(),
      path: Some(path.clone()),
      disk: Some(Fingerprint::of(&path).unwrap()),
      text: "# edited\n".to_owned(),
      cursor: 2,
      image: None,
      schema: None,
    };

    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Markdown, window, cx));

    view.read_with(cx, |view, cx| {
      assert!(view.is_dirty());
      assert_eq!(view.mode(), super::Mode::Preview);
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "# edited\n");
    });
    assert_eq!(fs::read_to_string(&path).unwrap(), "# on disk");
  }

  #[gpui_kit::test]
  fn a_failed_checkpoint_shows_an_error_and_keeps_the_buffer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    fs::remove_dir_all(dir.path()).unwrap();

    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert!(view.is_dirty());
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "x");
      assert!(view.last_error().is_some_and(|e| e.contains("draft")));
    });
  }
  #[gpui_kit::test]
  fn a_checkpoint_that_lands_after_a_save_does_not_resurrect_the_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    cx.simulate_input("new");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "newold");
    assert!(store.list().unwrap().is_empty());
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty());
  }

  #[gpui_kit::test]
  fn restored_markdown_keeps_the_caret_after_switching_to_edit(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: "abc\ndef".to_owned(),
      cursor: 4,
      image: None,
      schema: None,
    };

    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Markdown, window, cx));
    cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_mode(&crate::actions::ToggleMode, window, cx)));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      let editor = view.editor.as_ref().expect("switching to edit creates an editor");
      assert_eq!(editor.read(cx).cursor(), 4);
    });
  }
  #[gpui_kit::test]
  fn a_save_of_an_older_revision_keeps_the_draft_for_newer_edits(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));

    cx.simulate_input("a");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.simulate_input("b");
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    let drafts = store.list().unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].text.starts_with("ab"));
  }
  #[gpui_kit::test]
  fn a_clean_document_reloads_when_the_file_changes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    fs::write(&path, "new from outside").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "new from outside");
      assert!(!view.is_dirty());
      assert!(!view.external_change());
    });
  }

  #[gpui_kit::test]
  fn a_clean_document_reloads_through_the_watch_pump(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    fs::write(&path, "new from outside").unwrap();
    let tx = view.read_with(cx, |view, _| view.watch_sender_for_test());
    tx.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "new from outside");
      assert!(!view.is_dirty());
      assert!(!view.external_change());
    });
  }

  #[gpui_kit::test]
  fn a_dirty_document_keeps_its_buffer_and_flags_the_conflict(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");

    fs::write(&path, "theirs").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "mine");
      assert!(view.is_dirty());
      assert!(view.external_change());
    });
  }

  #[gpui_kit::test]
  fn saving_over_an_external_change_asks_first(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");
    fs::write(&path, "theirs").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert_eq!(
      fs::read_to_string(&path).unwrap(),
      "theirs",
      "nothing written before the answer"
    );
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "theirs");
    assert!(view.read_with(cx, |v, _| v.is_dirty()));

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    cx.simulate_prompt_answer("Overwrite");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    view.read_with(cx, |v, _| {
      assert!(!v.is_dirty());
      assert!(!v.external_change());
    });
  }

  #[gpui_kit::test]
  fn our_own_save_is_not_an_external_change(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();

    view.read_with(cx, |v, cx| {
      assert!(!v.external_change());
      assert!(v.snapshot(cx).unwrap().text.to_string().starts_with('x'));
    });
  }
  #[gpui_kit::test]
  fn reload_result_for_a_stale_revision_flags_a_conflict(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("mine");
    cx.update(|window, cx| {
      view.update(cx, |view, cx| {
        view.apply_reload_for_test(Revision::INITIAL, "theirs".to_owned(), window, cx);
      });
    });

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "mine");
      assert!(view.is_dirty());
      assert!(view.external_change());
    });
  }

  #[gpui_kit::test]
  fn a_markdown_preview_reloads_without_an_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("notes.md");
    fs::write(&path, "# Old\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    fs::write(&path, "# New\n").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "# New\n");
      assert!(!view.is_dirty());
      assert_eq!(view.preview_revision(), Some(view.revision()));
    });
  }
  #[gpui_kit::test]
  fn a_reload_error_after_an_edit_flags_a_conflict(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("mine");
    cx.update(|window, cx| {
      view.update(cx, |view, cx| {
        view.apply_reload_error_for_test(Revision::INITIAL, "read failed".to_owned(), window, cx);
      });
    });

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "mine");
      assert!(view.is_dirty());
      assert!(view.external_change());
      assert_eq!(view.last_error(), Some("read failed"));
    });
  }

  #[gpui_kit::test]
  fn a_write_after_persist_is_still_a_conflict(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let saved = Saved {
      revision: Revision::INITIAL,
      fingerprint: Fingerprint::of(&path).unwrap(),
    };
    fs::write(&path, "theirs").unwrap();

    cx.update(|_, cx| view.update(cx, |view, cx| view.apply_save_result_for_test(saved, cx)));

    view.read_with(cx, |view, _| {
      assert!(!view.is_dirty());
      assert!(view.external_change());
    });
  }
  #[gpui_kit::test]
  fn closing_a_clean_document_needs_no_prompt(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn closing_a_dirty_document_offers_save_discard_cancel(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert_eq!(store.list().unwrap().len(), 1);

    // Cancel: nothing happens.
    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.is_dirty()));
    assert_eq!(cx.windows().len(), 1);

    // Discard: draft removed, window closes, file untouched.
    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    drop(view);
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn discard_cancels_a_pending_autosave(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    drop(view);
    cx.run_until_parked();
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn discard_cancels_a_pending_checkpoint(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    drop(view);
    cx.run_until_parked();
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn edits_and_saves_are_ignored_once_closing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    let window_handle = view.read_with(cx, |view, _| view.session.window_handle);
    cx.update(|window, app| {
      view.update(app, |view, cx| view.discard_and_close(window, cx));
    });
    cx.cx.dispatch_keystroke(window_handle, Keystroke::parse("z").unwrap());
    cx.update(|window, app| {
      view.update(app, |view, cx| view.save(&crate::actions::Save, window, cx));
    });
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn autosave_waits_while_the_close_prompt_is_pending(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");

    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();
    assert!(fs::read_to_string(&path).unwrap().starts_with('x'));
  }

  #[gpui_kit::test]
  fn repeated_close_requests_keep_one_prompt(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());

    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    assert_eq!(cx.windows().len(), 1);
  }

  #[gpui_kit::test]
  fn close_after_save_stays_open_when_the_file_changes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    let saved = Saved {
      revision: view.read_with(cx, |view, _| view.revision()),
      fingerprint: Fingerprint::of(&path).unwrap(),
    };
    fs::write(&path, "theirs").unwrap();

    cx.update(|_, cx| {
      view.update(cx, |view, cx| {
        view.session.close_after_save = true;
        view.apply_save_result_for_test(saved, cx);
      });
    });

    assert_eq!(cx.windows().len(), 1);
    assert!(view.read_with(cx, |view, _| view.external_change()));
    assert!(!view.read_with(cx, |view, _| view.session.close_after_save));
  }

  #[gpui_kit::test]
  fn quitting_with_unsaved_edits_asks_and_cancel_keeps_the_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();

    assert!(!cx.foreground_executor().block_test(task));
    assert_eq!(cx.windows().len(), 1);
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    assert_eq!(store.list().unwrap().len(), 1);

    // A later quit asks again instead of staying stuck behind the cancelled one.
    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();
    assert!(cx.foreground_executor().block_test(task));
    assert!(store.list().unwrap().is_empty());
  }

  #[gpui_kit::test]
  fn quitting_and_discarding_drops_the_draft_and_leaves_the_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert_eq!(store.list().unwrap().len(), 1);

    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();

    assert!(cx.foreground_executor().block_test(task));
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn quitting_and_saving_writes_the_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();

    assert!(cx.foreground_executor().block_test(task));
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn a_native_close_while_the_quit_prompt_is_up_keeps_one_prompt(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();

    assert!(!cx.has_pending_prompt());
    assert!(!cx.foreground_executor().block_test(task));
    assert_eq!(cx.windows().len(), 1);
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn quitting_with_save_keeps_a_failed_save_open(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();

    assert!(!cx.foreground_executor().block_test(task));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
  }

  #[gpui_kit::test]
  fn close_action_closes_a_clean_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));

    cx.dispatch_action(CloseWindow);
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn flush_checkpoint_captures_edits_during_a_checkpoint(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    let task = cx.update(|_, cx| view.update(cx, DocumentView::flush_checkpoint));
    cx.simulate_input("y");
    cx.run_until_parked();

    assert_eq!(cx.foreground_executor().block_test(task), Ok(()));
    let drafts = store.list().unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].text.starts_with("xy"));
  }

  #[gpui_kit::test]
  fn closing_with_save_writes_then_closes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Save");
    drop(view);
    cx.run_until_parked();

    assert!(fs::read_to_string(&path).unwrap().starts_with('x'));
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn flush_checkpoint_writes_immediately_and_reports_failure(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    let task = cx.update(|_, cx| view.update(cx, DocumentView::flush_checkpoint));
    cx.run_until_parked();
    assert_eq!(cx.foreground_executor().block_test(task), Ok(()));
    assert_eq!(store.list().unwrap().len(), 1);

    cx.simulate_input("y");
    fs::remove_dir_all(dir.path()).unwrap();
    let task = cx.update(|_, cx| view.update(cx, DocumentView::flush_checkpoint));
    cx.run_until_parked();
    assert!(cx.foreground_executor().block_test(task).is_err());
  }
  #[gpui_kit::test]
  fn autosave_off_by_default_leaves_the_file_alone(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings::default())));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    cx.executor().advance_clock(AUTOSAVE_DELAY * 3);
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
  }

  #[gpui_kit::test]
  fn autosave_on_writes_after_the_delay_and_clears_the_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old", "not before the delay");

    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();

    assert!(fs::read_to_string(&path).unwrap().starts_with('x'));
    assert!(!view.read_with(cx, |v, _| v.is_dirty()));
    assert!(store.list().unwrap().is_empty());
  }

  #[gpui_kit::test]
  fn turning_autosave_on_applies_to_an_open_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings::default())));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(AUTOSAVE_DELAY * 2);
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");

    cx.update(|_, cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    cx.run_until_parked();
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();

    assert!(
      fs::read_to_string(&path).unwrap().starts_with('x'),
      "a dirty window saves once autosave turns on"
    );
  }
  #[gpui_kit::test]
  fn a_manual_save_cancels_the_pending_autosave(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    let saved_fingerprint = Fingerprint::of(&path).unwrap();

    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();

    assert_eq!(Fingerprint::of(&path).unwrap(), saved_fingerprint);
  }

  #[gpui_kit::test]
  fn a_document_without_a_path_never_autosaves(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: "old".to_owned(),
      cursor: 0,
      image: None,
      schema: None,
    };

    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
      assert!(view.is_dirty());
      assert!(view.last_error().is_none());
    });
  }
  #[cfg(unix)]
  #[gpui_kit::test]
  fn autosave_re_arms_after_an_abandoned_close_path(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();
    assert!(fs::read_to_string(&path).unwrap().starts_with('x'));
  }
  #[gpui_kit::test]
  fn a_second_save_during_a_write_is_serviced_afterwards(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("one");
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.simulate_input("two");
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "onetwo");
    assert!(!view.read_with(cx, |view, _| view.is_dirty()));
  }

  #[gpui_kit::test]
  fn a_close_request_during_an_overwrite_prompt_is_ignored(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("mine");
    fs::write(&path, "theirs").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
  }

  #[gpui_kit::test]
  fn a_restored_draft_whose_source_changed_needs_overwrite_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let baseline = Fingerprint::of(&path).unwrap();
    fs::write(&path, "theirs").unwrap();
    let draft = Draft {
      session: SessionId::new(),
      path: Some(path),
      disk: Some(baseline),
      text: "mine".to_owned(),
      cursor: 0,
      image: None,
      schema: None,
    };
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx));

    assert!(view.read_with(cx, |view, _| view.external_change()));
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
  }

  #[gpui_kit::test]
  fn a_restored_draft_with_a_missing_source_says_so(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let baseline = Fingerprint::of(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let draft = Draft {
      session: SessionId::new(),
      path: Some(path),
      disk: Some(baseline),
      text: "mine".to_owned(),
      cursor: 0,
      image: None,
      schema: None,
    };
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx));

    assert!(view.read_with(cx, |view, _| view.external_change()));
  }
  #[gpui_kit::test]
  fn a_save_that_lands_after_discard_does_not_write(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("saved");
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    assert!(!cx.simulate_close());
    cx.simulate_prompt_answer("Discard");
    drop(view);
    cx.run_until_parked();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text == "old" || text == "saved");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn a_foreign_write_right_before_saving_is_a_conflict(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("mine");
    fs::write(&path, "foreign").unwrap();
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "foreign");
    view.read_with(cx, |view, _| {
      assert!(view.is_dirty());
      assert!(view.external_change());
    });
  }

  #[gpui_kit::test]
  fn save_and_close_survives_an_autosave_finishing_first(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    cx.update(|cx| cx.set_global(AppSettings(Settings { autosave: true, ..Settings::default() })));
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    select_all(cx);
    cx.simulate_input("mine");
    cx.executor().advance_clock(AUTOSAVE_DELAY);
    cx.run_until_parked();
    cx.update(|window, cx| view.update(cx, |view, cx| view.save_then_close(window, cx)));
    cx.run_until_parked();

    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn a_clean_close_waits_for_draft_removal(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let session = view.read_with(cx, |view, _| view.session.session);
    store
      .checkpoint(&Draft {
        session,
        path: Some(path),
        disk: None,
        text: "draft".to_owned(),
        cursor: 0,
        image: None,
        schema: None,
      })
      .unwrap();
    cx.update(|_, cx| {
      view.update(cx, |view, _| {
        view.session.saved_revision = view.session.revision;
      });
    });

    assert!(!cx.simulate_close());
    drop(view);
    cx.run_until_parked();
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn a_failed_draft_removal_keeps_the_window_open(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(cx.windows().len(), 1);
    assert!(view.read_with(cx, |view, _| view.last_error().is_some_and(|error| error.contains("draft"))));
    let _ = store.list();
  }
  #[gpui_kit::test]
  fn request_quit_keeps_close_guard_durable_until_commit(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));

    let task = cx.cx.update(crate::request_quit);

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.foreground_executor().block_test(task));
  }
  #[gpui_kit::test]
  fn quitting_after_discard_waits_for_cleanup_without_recreating_a_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    cx.update(|window, cx| view.update(cx, |view, cx| view.discard_and_close(window, cx)));
    let task = cx.cx.update(crate::request_quit);

    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
    assert!(cx.foreground_executor().block_test(task));
  }

  #[gpui_kit::test]
  fn quitting_while_a_close_prompt_is_up_is_refused_and_the_prompt_stays(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());

    let task = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(!cx.foreground_executor().block_test(task));

    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Discard");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
  }

  #[gpui_kit::test]
  fn save_then_close_finishing_during_quit_flush_removes_the_draft(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");

    cx.update(|window, cx| view.update(cx, |view, cx| view.save_then_close(window, cx)));
    let task = cx.cx.update(crate::request_quit);

    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
    assert!(store.list().unwrap().is_empty());
    assert_eq!(cx.windows().len(), 0);
    assert!(cx.foreground_executor().block_test(task));
  }

  #[gpui_kit::test]
  fn quit_is_single_flight_while_its_prompt_is_up(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (_view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");

    let first = cx.cx.update(crate::request_quit);
    let second = cx.cx.update(crate::request_quit);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert!(!cx.foreground_executor().block_test(second));

    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert!(!cx.foreground_executor().block_test(first));
    assert_eq!(cx.windows().len(), 1);
  }

  #[gpui_kit::test]
  fn a_clean_reload_adopts_the_fingerprint_from_its_read(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));

    fs::write(&path, "first external").unwrap();
    cx.update(|window, cx| view.update(cx, |view, cx| view.on_disk_change(window, cx)));
    fs::write(&path, "second external").unwrap();
    let expected = Fingerprint::of(&path).unwrap();
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "second external");
      assert_eq!(view.disk_for_test(), Some(expected));
    });
  }

  #[gpui_kit::test]
  fn a_dirty_checkpoint_keeps_the_original_disk_baseline_after_a_foreign_write(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("a.txt");
    fs::write(&path, "old").unwrap();
    let baseline = Fingerprint::of(&path).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    select_all(cx);
    cx.simulate_input("mine");

    fs::write(&path, "foreign").unwrap();
    cx.update(|_, cx| view.update(cx, DocumentView::simulate_disk_change));
    cx.run_until_parked();
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();

    let draft = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(draft.disk, Some(baseline));
    let (restored, cx) =
      cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx));
    assert!(restored.read_with(cx, |view, _| view.external_change()));
  }

  #[gpui_kit::test]
  fn a_restored_draft_without_a_disk_baseline_reports_a_missing_source(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("missing.txt");
    let draft = Draft {
      session: SessionId::new(),
      path: Some(path),
      disk: None,
      text: "mine".to_owned(),
      cursor: 0,
      image: None,
      schema: None,
    };

    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx));

    view.read_with(cx, |view, _| {
      assert!(view.external_change());
      assert_eq!(view.source_status(), Some("Source file missing"));
    });
  }
  #[gpui_kit::test]
  fn a_relative_local_image_loads_from_the_document_directory(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "![a](img.png)\n").unwrap();
    fs::write(doc.path().join("img.png"), png(4, 3)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    set_settings_path(&view, settings_dir.path().join("settings.toml"), cx);

    let resource = Resource::Uri("img.png".into());
    let _ = load_image(&view, resource.clone(), cx);
    cx.run_until_parked();
    let Some(Ok(image)) = load_image(&view, resource, cx) else {
      panic!("local image should load");
    };
    assert_eq!(
      image.size(0),
      gpui_kit::size(gpui_kit::DevicePixels(4), gpui_kit::DevicePixels(3))
    );
  }

  #[gpui_kit::test]
  fn a_remote_image_on_an_allowed_family_is_fetched_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://raw.githubusercontent.com/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());

    let _ = load_image(&view, resource.clone(), cx);
    cx.run_until_parked();
    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource, cx), Some(Ok(_))));
    assert_eq!(fetcher.calls().len(), 1);
  }
  #[gpui_kit::test]
  fn a_cached_remote_image_loads_without_asking_or_fetching(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://cdn.example.org/x.png";
    let cache = cx.update(|cx| Arc::clone(&cx.global::<ResourceCacheHandle>().0));
    let Resolved::Remote(remote) = openit_core::resource::resolve(url, None) else {
      panic!("test URL should resolve remotely");
    };
    cache.put(&remote, &png(4, 3)).unwrap();
    let fetcher = install_fetcher(HashMap::new(), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());

    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource, cx), Some(Ok(_))));
    let permission_requests = requests(&view, cx);
    let pending = cx.update(|_, app| permission_requests.read(app).pending.clone());
    assert!(pending.is_empty());
    assert_eq!(fetcher.calls().len(), 0);
  }

  #[gpui_kit::test]
  fn an_invalid_cached_image_is_removed_before_the_policy_fallback(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    cx.update(|cx| {
      cx.update_global::<AppSettings, _>(|settings, _| settings.0.allow_remote = true);
    });
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://cdn.example.org/x.png";
    let cache = cx.update(|cx| Arc::clone(&cx.global::<ResourceCacheHandle>().0));
    let Resolved::Remote(remote) = openit_core::resource::resolve(url, None) else {
      panic!("test URL should resolve remotely");
    };
    cache.put(&remote, b"not an image").unwrap();
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(b"still not an image".to_vec()))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());

    let mut result = load_image(&view, resource.clone(), cx);
    for _ in 0..4 {
      if result.is_some() {
        break;
      }
      cx.run_until_parked();
      result = load_image(&view, resource.clone(), cx);
    }
    assert!(matches!(result, Some(Err(_))));
    assert!(cache.get(&remote).unwrap().is_none());
    assert_eq!(fetcher.calls().len(), 1);
  }

  #[gpui_kit::test]
  fn a_fetched_image_is_cached(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://raw.githubusercontent.com/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());

    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource, cx), Some(Ok(_))));
    let cache = cx.update(|_, app| Arc::clone(&app.global::<ResourceCacheHandle>().0));
    let Resolved::Remote(remote) = openit_core::resource::resolve(url, None) else {
      panic!("test URL should resolve remotely");
    };
    assert!(cache.get(&remote).unwrap().is_some());
    assert_eq!(fetcher.calls().len(), 1);
  }
  #[gpui_kit::test]
  fn a_second_load_of_an_inflight_remote_image_does_not_fetch_twice(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://raw.githubusercontent.com/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());
    assert!(load_image(&view, resource.clone(), cx).is_none());
    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, resource, cx).is_none());
    cx.run_until_parked();
    assert_eq!(fetcher.calls().len(), 1);
  }

  #[gpui_kit::test]
  fn a_settings_change_retries_pending_permissions_in_every_view(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://cdn.example.org/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());

    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, resource.clone(), cx).is_none());
    let requests = requests(&view, cx);
    assert_eq!(cx.update(|_, app| requests.read(app).pending.len()), 1);

    cx.update(|_, cx| {
      let mut settings = Settings::default();
      settings.allowed_domains.push("example.org".to_owned());
      cx.set_global(AppSettings(settings));
    });
    cx.run_until_parked();

    assert!(cx.update(|_, app| requests.read(app).pending.is_empty()));
    let mut result = load_image(&view, resource.clone(), cx);
    for _ in 0..3 {
      if result.is_some() {
        break;
      }
      cx.run_until_parked();
      result = load_image(&view, resource.clone(), cx);
    }
    assert!(matches!(result, Some(Ok(_))));
    assert_eq!(fetcher.calls().len(), 1);
  }

  #[gpui_kit::test]
  fn a_remote_image_on_an_unknown_family_asks_and_does_not_fetch(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://cdn.example.org/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri(url.into());
    assert!(load_image(&view, resource, cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, Resource::Uri(url.into()), cx).is_none());
    let requests = requests(&view, cx);
    let pending = cx.update(|_, app| requests.read(app).pending.clone());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].family, DomainFamily::of_host("cdn.example.org"));
    assert_eq!(fetcher.calls().len(), 0);
  }

  #[gpui_kit::test]
  fn allowing_the_family_persists_and_retries(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let url = "https://cdn.example.org/x.png";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(png(4, 3)))]), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let settings_path = settings_dir.path().join("settings.toml");
    set_settings_path(&view, settings_path.clone(), cx);
    let resource = Resource::Uri(url.into());
    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, resource.clone(), cx).is_none());
    let family = DomainFamily::of_host("cdn.example.org");

    let requests = requests(&view, cx);
    cx.update(|_, app| requests.update(app, |requests, app| requests.answer_allow(family.clone(), app)));
    cx.run_until_parked();

    assert!(cx.read_global::<AppSettings, _>(|settings, _| {
      settings.0.allowed_domains.iter().any(|domain| domain == "example.org")
    }));
    assert!(
      Settings::load(&settings_path)
        .unwrap()
        .allowed_domains
        .iter()
        .any(|domain| domain == "example.org")
    );
    assert!(load_image(&view, resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource, cx), Some(Ok(_))));
    assert_eq!(fetcher.calls().len(), 1);
  }

  #[gpui_kit::test]
  fn two_permission_answers_retry_both_families(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let first_url = "https://a.example.org/x.png";
    let second_url = "https://b.example.net/y.png";
    let fetcher = install_fetcher(
      HashMap::from([(first_url.to_owned(), Ok(png(4, 3))), (second_url.to_owned(), Ok(png(2, 2)))]),
      cx,
    );
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let first = Resource::Uri(first_url.into());
    let second = Resource::Uri(second_url.into());
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    let requests = requests(&view, cx);
    let first_family = DomainFamily::of_host("a.example.org");
    let second_family = DomainFamily::of_host("b.example.net");
    cx.update(|_, app| requests.update(app, |requests, app| requests.answer_allow(first_family, app)));
    cx.update(|_, app| requests.update(app, |requests, app| requests.answer_allow(second_family, app)));
    cx.run_until_parked();
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, first, cx), Some(Ok(_))));
    assert!(matches!(load_image(&view, second, cx), Some(Ok(_))));
    assert_eq!(fetcher.calls().len(), 2);
  }

  #[gpui_kit::test]
  fn two_images_from_one_family_make_one_request(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let first = Resource::Uri("https://a.example.org/x.png".into());
    let second = Resource::Uri("https://b.example.org/y.png".into());

    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, first, cx).is_none());
    assert!(load_image(&view, second, cx).is_none());
    let requests = requests(&view, cx);
    let pending = cx.update(|_, app| requests.read(app).pending.clone());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].waiting.len(), 2);
  }

  #[gpui_kit::test]
  fn two_views_share_one_coalesced_settings_write(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let first_doc = tempfile::tempdir().unwrap();
    let first_path = first_doc.path().join("first.md");
    fs::write(&first_path, "# Images\n").unwrap();
    let second_doc = tempfile::tempdir().unwrap();
    let second_path = second_doc.path().join("second.md");
    fs::write(&second_path, "# Images\n").unwrap();
    let first_url = "https://a.example.org/x.png";
    let second_url = "https://b.example.net/y.png";
    let _fetcher = install_fetcher(
      HashMap::from([(first_url.to_owned(), Ok(png(4, 3))), (second_url.to_owned(), Ok(png(2, 2)))]),
      cx,
    );
    let first_loaded = load_text(&first_path).unwrap();
    let (first_view, cx) = cx
      .add_window_view(|window, cx| DocumentView::open(first_path.clone(), first_loaded, SessionId::new(), window, cx));
    let second_loaded = load_text(&second_path).unwrap();
    let (second_view, cx) = cx.add_window_view(|window, cx| {
      DocumentView::open(second_path.clone(), second_loaded, SessionId::new(), window, cx)
    });
    let settings_path = settings_dir.path().join("settings.toml");
    set_settings_path(&first_view, settings_path.clone(), cx);
    let first_resource = Resource::Uri(first_url.into());
    let second_resource = Resource::Uri(second_url.into());

    assert!(load_image(&first_view, first_resource.clone(), cx).is_none());
    assert!(load_image(&second_view, second_resource.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&first_view, first_resource, cx).is_none());
    assert!(load_image(&second_view, second_resource, cx).is_none());

    let first_requests = requests(&first_view, cx);
    let second_requests = requests(&second_view, cx);
    cx.update(|_, app| {
      first_requests.update(app, |requests, cx| {
        requests.answer_allow(DomainFamily::of_host("a.example.org"), cx);
      });
      second_requests.update(app, |requests, cx| {
        requests.answer_allow(DomainFamily::of_host("b.example.net"), cx);
      });
    });
    cx.run_until_parked();

    let settings = Settings::load(&settings_path).unwrap();
    assert!(settings.allowed_domains.iter().any(|domain| domain == "example.org"));
    assert!(settings.allowed_domains.iter().any(|domain| domain == "example.net"));
  }

  #[gpui_kit::test]
  fn always_allow_sets_the_setting_and_retries_everything(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let first_url = "https://a.example.org/x.png";
    let second_url = "https://b.example.net/y.png";
    let fetcher = install_fetcher(
      HashMap::from([(first_url.to_owned(), Ok(png(4, 3))), (second_url.to_owned(), Ok(png(2, 2)))]),
      cx,
    );
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    set_settings_path(&view, settings_dir.path().join("settings.toml"), cx);
    let first = Resource::Uri(first_url.into());
    let second = Resource::Uri(second_url.into());
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());

    let requests = requests(&view, cx);
    cx.update(|_, app| requests.update(app, PermissionRequests::answer_always));
    cx.run_until_parked();

    assert!(cx.read_global::<AppSettings, _>(|settings, _| settings.0.allow_remote));
    assert!(load_image(&view, first.clone(), cx).is_none());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(matches!(load_image(&view, first, cx), Some(Ok(_))));
    assert!(matches!(load_image(&view, second, cx), Some(Ok(_))));
    assert_eq!(fetcher.calls().len(), 2);
  }

  #[gpui_kit::test]
  fn denied_schemes_show_a_placeholder_without_fetching(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let fetcher = install_fetcher(HashMap::new(), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let file = Resource::Uri("file:///etc/hosts".into());
    let data = Resource::Uri("data:image/png;base64,AAAA".into());

    assert!(matches!(load_image(&view, file.clone(), cx), Some(Ok(_))));
    assert!(matches!(load_image(&view, data.clone(), cx), Some(Ok(_))));
    assert!(has_entry(&view, &file, cx, |entry| matches!(
      entry,
      Entry::Placeholder(PlaceholderKind::Denied(_))
    )));
    assert!(has_entry(&view, &data, cx, |entry| matches!(
      entry,
      Entry::Placeholder(PlaceholderKind::Denied(_))
    )));
    assert_eq!(fetcher.calls().len(), 0);
  }

  #[gpui_kit::test]
  fn svg_shows_a_placeholder(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    fs::write(doc.path().join("logo.svg"), "<svg></svg>").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri("logo.svg".into());

    let _ = load_image(&view, resource.clone(), cx);
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource.clone(), cx), Some(Ok(_))));
    assert!(has_entry(&view, &resource, cx, |entry| matches!(
      entry,
      Entry::Placeholder(PlaceholderKind::Svg)
    )));
  }

  #[gpui_kit::test]
  fn an_allowed_family_schema_fetches_once_and_the_second_load_is_cache(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let url = "https://raw.githubusercontent.com/schema.json";
    let body = br#"{"type":"object"}"#.to_vec();
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(body))]), cx);
    let path = doc.path().join("doc.json");
    fs::write(&path, schema_json(url)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let resolved = openit_core::resource::resolve(url, None);
    assert!(schema_cache(&view, cx).read_with(cx, |cache, _| cache.is_ready(&resolved)));
    assert_eq!(fetcher.calls().len(), 1);
    let cache = cx.update(|_, app| Arc::clone(&app.global::<ResourceCacheHandle>().0));
    let Resolved::Remote(remote) = &resolved else {
      panic!("test URL should resolve remotely");
    };
    assert!(cache.get(remote).unwrap().is_some());

    let path2 = doc.path().join("other.json");
    fs::write(&path2, schema_json(url)).unwrap();
    let loaded = load_text(&path2).unwrap();
    let (view2, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path2.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view2, cx);
    assert!(schema_cache(&view2, cx).read_with(cx, |cache, _| cache.is_ready(&resolved)));
    assert_eq!(fetcher.calls().len(), 1);
  }

  #[gpui_kit::test]
  fn an_unknown_family_schema_asks_then_allow_retries(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let url = "https://cdn.example.org/schema.json";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Ok(br#"{"type":"object"}"#.to_vec()))]), cx);
    let path = doc.path().join("doc.json");
    fs::write(&path, schema_json(url)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    set_settings_path(&view, settings_dir.path().join("settings.toml"), cx);
    settle_schemas(&view, cx);
    let resolved = openit_core::resource::resolve(url, None);
    assert!(schema_cache(&view, cx).read_with(cx, |cache, _| cache.is_awaiting(&resolved)));
    assert!(
      !view.read_with(cx, DocumentView::status_bar_overlays_the_buffer),
      "the permission bar must stay visible while editing"
    );
    assert_eq!(fetcher.calls().len(), 0);
    let requests = requests(&view, cx);
    let pending = cx.update(|_, app| requests.read(app).pending.clone());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].family, DomainFamily::of_host("cdn.example.org"));

    let family = DomainFamily::of_host("cdn.example.org");
    cx.update(|_, app| requests.update(app, |requests, app| requests.answer_allow(family, app)));
    cx.run_until_parked();
    settle_schemas(&view, cx);
    assert!(schema_cache(&view, cx).read_with(cx, |cache, _| cache.is_ready(&resolved)));
    assert_eq!(fetcher.calls().len(), 1);
    assert!(cx.update(|_, app| requests.read(app).pending.is_empty()));
  }

  #[gpui_kit::test]
  fn a_failed_schema_fetch_after_allow_logs_only(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let url = "https://raw.githubusercontent.com/missing.json";
    let fetcher = install_fetcher(HashMap::from([(url.to_owned(), Err(FetchError::Status(500)))]), cx);
    let path = doc.path().join("doc.json");
    fs::write(&path, schema_json(url)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let resolved = openit_core::resource::resolve(url, None);
    assert!(schema_cache(&view, cx).read_with(cx, |cache, _| cache.is_failed(&resolved)));
    assert_eq!(fetcher.calls().len(), 1);
    let permission_requests = requests(&view, cx);
    assert!(cx.update(|_, app| permission_requests.read(app).pending.is_empty()));
    view.read_with(cx, |view, _| {
      assert!(view.last_error().is_none());
      assert!(view.source_status().is_none());
    });
  }

  #[gpui_kit::test]
  fn a_local_schema_relative_to_the_document_loads(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    fs::write(doc.path().join("schema.json"), r#"{"type":"object"}"#).unwrap();
    let path = doc.path().join("doc.json");
    fs::write(&path, schema_json("schema.json")).unwrap();
    let fetcher = install_fetcher(HashMap::new(), cx);
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let resolved = openit_core::resource::resolve("schema.json", Some(doc.path()));
    assert!(schema_cache(&view, cx).read_with(cx, |cache, _| cache.is_ready(&resolved)));
    assert_eq!(fetcher.calls().len(), 0);
  }

  #[gpui_kit::test]
  fn an_untitled_relative_schema_never_fetches(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let fetcher = install_fetcher(HashMap::new(), cx);
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: schema_json("./schema.json"),
      cursor: 0,
      image: None,
      schema: None,
    };
    let (view, cx) = cx.add_window_view(|window, cx| {
      DocumentView::restore(draft, DocumentKind::Text { language: Some("json") }, window, cx)
    });
    settle_schemas(&view, cx);
    assert_eq!(fetcher.calls().len(), 0);
    let permission_requests = requests(&view, cx);
    assert!(cx.update(|_, app| permission_requests.read(app).pending.is_empty()));
  }

  #[gpui_kit::test]
  fn schema_ref_hops_stop_at_depth_eight(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let mut responses = HashMap::new();
    let root = "https://raw.githubusercontent.com/root.json";
    responses.insert(root.to_owned(), Ok(br#"{"$ref":"1.json"}"#.to_vec()));
    for hop in 1..=8 {
      let url = format!("https://raw.githubusercontent.com/{hop}.json");
      let next = hop + 1;
      responses.insert(url, Ok(format!(r#"{{"$ref":"{next}.json"}}"#).into_bytes()));
    }
    responses.insert(
      "https://raw.githubusercontent.com/9.json".to_owned(),
      Ok(br#"{"type":"string"}"#.to_vec()),
    );
    let fetcher = install_fetcher(responses, cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    fs::write(&path, schema_json(root)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let calls = fetcher.calls();
    assert!(calls.contains(&root.to_owned()), "{calls:?}");
    assert!(
      calls.contains(&"https://raw.githubusercontent.com/8.json".to_owned()),
      "{calls:?}"
    );
    assert!(!calls.iter().any(|url| url.ends_with("/9.json")), "{calls:?}");
  }

  #[gpui_kit::test]
  fn json_with_schema_reports_an_unknown_property(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/schema.json";
    let body = br#"{"type":"object","properties":{"$schema":true},"additionalProperties":false}"#.to_vec();
    install_fetcher(HashMap::from([(url.to_owned(), Ok(body))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    let text = extra_key_json(url);
    fs::write(&path, &text).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let extra = text.find("\"extra\"").unwrap();
    let entries = diagnostic_entries(&view, cx);
    assert!(
      entries.iter().any(|(range, severity, _)| {
        *severity == DiagnosticSeverity::Error && range.start <= extra && extra < range.end
      }),
      "{entries:?}"
    );
    view.read_with(cx, |view, cx| {
      assert_eq!(view.schema_status(cx).as_deref(), Some("schema.json"));
      assert!(view.last_error().is_none());
      assert!(view.source_status().is_none());
    });
  }

  #[gpui_kit::test]
  fn schema_status_is_no_schema_without_a_match(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    install_fetcher(HashMap::new(), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("zzzz-openit-unknown.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    view.read_with(cx, |view, cx| {
      assert_eq!(view.schema_status(cx).as_deref(), Some("No schema"));
      assert!(view.last_error().is_none());
    });
  }

  #[gpui_kit::test]
  fn an_edit_clears_diagnostics_and_the_matching_revision_restores_them(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/schema.json";
    let body = br#"{"type":"object","properties":{"$schema":true},"additionalProperties":false}"#.to_vec();
    install_fetcher(HashMap::from([(url.to_owned(), Ok(body))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    fs::write(&path, extra_key_json(url)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    assert!(!diagnostic_entries(&view, cx).is_empty());
    cx.simulate_input(" ");
    cx.run_until_parked();
    assert!(diagnostic_entries(&view, cx).is_empty(), "edits clear the set");
    cx.executor().advance_clock(CHECKPOINT_DELAY);
    cx.run_until_parked();
    assert!(
      !diagnostic_entries(&view, cx).is_empty(),
      "matching revision puts diagnostics back"
    );
  }

  #[gpui_kit::test]
  fn a_stale_schema_result_is_ignored(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    install_fetcher(HashMap::new(), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("zzzz-openit-unknown.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    cx.simulate_input(" ");
    cx.run_until_parked();
    let stale = vec![openit_core::schema::SchemaDiagnostic {
      range: openit_core::schema::SourceRange { start: 0, end: 1 },
      message: "stale".to_owned(),
    }];
    cx.update(|_, cx| {
      view.update(cx, |view, cx| view.apply_schema_issues(Revision::INITIAL, stale, cx));
    });
    assert!(diagnostic_entries(&view, cx).is_empty());
  }

  #[gpui_kit::test]
  fn schema_errors_do_not_block_save(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/schema.json";
    let body = br#"{"type":"object","properties":{"$schema":true},"additionalProperties":false}"#.to_vec();
    install_fetcher(HashMap::from([(url.to_owned(), Ok(body))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    let text = extra_key_json(url);
    fs::write(&path, &text).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    assert!(!diagnostic_entries(&view, cx).is_empty());
    cx.update(|window, cx| view.update(cx, |view, cx| view.save(&crate::actions::Save, window, cx)));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
      assert!(!view.is_dirty());
      assert!(view.last_error().is_none());
    });
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
  }

  #[gpui_kit::test]
  fn a_failed_schema_fetch_is_not_a_diagnostic(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/missing.json";
    install_fetcher(HashMap::from([(url.to_owned(), Err(FetchError::Status(500)))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    fs::write(&path, extra_key_json(url)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    assert!(diagnostic_entries(&view, cx).is_empty());
    view.read_with(cx, |view, cx| {
      assert_eq!(view.schema_status(cx).as_deref(), Some("missing.json"));
      assert!(view.last_error().is_none());
    });
  }

  #[gpui_kit::test]
  fn a_loaded_schema_offers_properties_and_enum_values(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/schema.json";
    install_fetcher(HashMap::from([(url.to_owned(), Ok(completion_schema_body()))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    let text = format!("{{\n  \"$schema\": \"{url}\",\n  \"status\": \n}}\n");
    fs::write(&path, &text).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    view.read_with(cx, |view, cx| {
      assert!(
        view
          .editor
          .as_ref()
          .is_some_and(|editor| editor.read(cx).lsp().completion_provider.is_some())
      );
    });
    let root = text.find("{\n  ").unwrap() + "{\n  ".len();
    let properties = schema_completion_labels(&view, root, cx);
    assert!(properties.contains(&"status".to_owned()), "{properties:?}");
    assert!(properties.contains(&"author".to_owned()), "{properties:?}");
    let value = text.find("\"status\": ").unwrap() + "\"status\": ".len();
    let enums = schema_completion_labels(&view, value, cx);
    assert!(enums.contains(&"\"draft\"".to_owned()), "{enums:?}");
    assert!(enums.contains(&"\"live\"".to_owned()), "{enums:?}");
  }

  #[gpui_kit::test]
  fn nested_object_properties_appear_inside_that_object(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let url = "https://raw.githubusercontent.com/schema.json";
    install_fetcher(HashMap::from([(url.to_owned(), Ok(completion_schema_body()))]), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.json");
    let text = format!("{{\n  \"$schema\": \"{url}\",\n  \"author\": {{\n    \n  }}\n}}\n");
    fs::write(&path, &text).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    let nested = text.find("\"author\": {\n    ").unwrap() + "\"author\": {\n    ".len();
    let labels = schema_completion_labels(&view, nested, cx);
    assert!(labels.contains(&"name".to_owned()), "{labels:?}");
    assert!(labels.contains(&"email".to_owned()), "{labels:?}");
    assert!(!labels.contains(&"status".to_owned()), "{labels:?}");
  }

  #[gpui_kit::test]
  fn no_schema_means_the_provider_contributes_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    install_fetcher(HashMap::new(), cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("zzzz-openit-unknown.json");
    let text = "{\n  \n}\n";
    fs::write(&path, text).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    settle_schemas(&view, cx);
    view.read_with(cx, |view, cx| {
      assert!(
        view
          .editor
          .as_ref()
          .is_some_and(|editor| editor.read(cx).lsp().completion_provider.is_some())
      );
    });
    let offset = text.find("{\n  ").unwrap() + "{\n  ".len();
    let labels = schema_completion_labels(&view, offset, cx);
    assert!(labels.is_empty(), "{labels:?}");
  }

  #[gpui_kit::test]
  fn schema_status_click_opens_the_picker(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zzzz-openit-unknown.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_schema_picker(window, cx)));
    assert!(view.read_with(cx, |view, _| matches!(view.overlay, Some(super::Overlay::Schema(_)))));
  }

  #[gpui_kit::test]
  fn picking_a_schema_writes_settings_and_is_used_on_the_next_open(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let url = "https://www.schemastore.org/package.json";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zzzz-openit-unknown.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert_eq!(
      view.read_with(cx, |view, cx| view.schema_status(cx).as_deref().map(str::to_owned)),
      Some("No schema".to_owned())
    );
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_schema_picker(window, cx)));
    let picker = view.read_with(cx, |view, _| match &view.overlay {
      Some(super::Overlay::Schema(picker)) => picker.clone(),
      _ => panic!("schema picker expected"),
    });
    cx.update(|_, cx| picker.update(cx, |_, cx| cx.emit(SchemaPickerEvent::Picked(url.to_owned()))));
    cx.run_until_parked();
    let key = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
    assert_eq!(
      cx.read_global::<AppSettings, _>(|settings, _| settings.0.schemas.get(&key).cloned()),
      Some(url.to_owned())
    );
    assert_eq!(
      view.read_with(cx, |view, cx| view.schema_status(cx).as_deref().map(str::to_owned)),
      Some("package.json".to_owned())
    );
    let loaded = load_text(&path).unwrap();
    let (next, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    assert_eq!(
      next.read_with(cx, |view, cx| view.schema_status(cx).as_deref().map(str::to_owned)),
      Some("package.json".to_owned())
    );
  }

  #[gpui_kit::test]
  fn a_recovered_draft_restores_the_schema_pick(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let url = "https://www.schemastore.org/package.json";
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: "{}\n".to_owned(),
      cursor: 0,
      image: None,
      schema: Some(url.to_owned()),
    };
    let (view, cx) = cx.add_window_view(|window, cx| {
      DocumentView::restore(draft, DocumentKind::Text { language: Some("json") }, window, cx)
    });
    assert_eq!(
      view.read_with(cx, |view, cx| view.schema_status(cx).as_deref().map(str::to_owned)),
      Some("package.json".to_owned())
    );
  }

  #[gpui_kit::test]
  fn automatic_clears_the_manual_schema_pick(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let url = "https://www.schemastore.org/package.json";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zzzz-openit-unknown.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.update(|window, cx| view.update(cx, |view, cx| view.set_schema_pick(Some(url.to_owned()), window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_schema_picker(window, cx)));
    let picker = view.read_with(cx, |view, _| match &view.overlay {
      Some(super::Overlay::Schema(picker)) => picker.clone(),
      _ => panic!("schema picker expected"),
    });
    cx.update(|_, cx| picker.update(cx, |_, cx| cx.emit(SchemaPickerEvent::Automatic)));
    cx.run_until_parked();
    let key = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
    assert!(cx.read_global::<AppSettings, _>(|settings, _| !settings.0.schemas.contains_key(&key)));
    assert_eq!(
      view.read_with(cx, |view, cx| view.schema_status(cx).as_deref().map(str::to_owned)),
      Some("No schema".to_owned())
    );
  }

  #[gpui_kit::test]
  fn an_ambiguous_catalog_match_opens_the_picker(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    fs::write(&path, "{}\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| matches!(view.overlay, Some(super::Overlay::Schema(_)))));
  }

  #[gpui_kit::test]
  fn an_untitled_document_denies_relative_paths(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let draft = Draft {
      session: SessionId::new(),
      path: None,
      disk: None,
      text: "# Images\n".to_owned(),
      cursor: 0,
      image: None,
      schema: None,
    };
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::restore(draft, DocumentKind::Markdown, window, cx));
    let resource = Resource::Uri("img.png".into());

    assert!(matches!(load_image(&view, resource.clone(), cx), Some(Ok(_))));
    assert!(has_entry(&view, &resource, cx, |entry| matches!(
      entry,
      Entry::Placeholder(PlaceholderKind::Denied(DenyReason::NoLocalBase))
    )));
  }

  #[gpui_kit::test]
  fn an_oversized_local_image_is_unsupported(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let image_path = doc.path().join("huge.png");
    let file = fs::File::create(&image_path).unwrap();
    file.set_len(crate::image_decode::MAX_IMAGE_BYTES + 1).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri("huge.png".into());

    let _ = load_image(&view, resource.clone(), cx);
    cx.run_until_parked();

    assert!(matches!(load_image(&view, resource.clone(), cx), Some(Err(_))));
    assert!(has_entry(&view, &resource, cx, |entry| matches!(entry, Entry::Failed(_))));
  }
  #[cfg(unix)]
  #[gpui_kit::test]
  fn a_settings_write_error_is_shown_and_clears_after_success(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    cx.update(gpui_kit::init);
    let (settings_dir, _store) = install_globals(cx);
    let read_only = settings_dir.path().join("read-only");
    fs::create_dir(&read_only).unwrap();
    let settings_path = read_only.join("settings.toml");
    fs::create_dir(&settings_path).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    let first_url = "https://cdn.example.org/x.png";
    let second_url = "https://cdn.example.net/y.png";
    install_fetcher(
      HashMap::from([(first_url.to_owned(), Ok(png(4, 3))), (second_url.to_owned(), Ok(png(2, 2)))]),
      cx,
    );
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    set_settings_path(&view, settings_path.clone(), cx);
    let first = Resource::Uri(first_url.into());
    assert!(load_image(&view, first.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, first, cx).is_none());
    let requests = requests(&view, cx);
    cx.update(|_, app| {
      requests.update(app, |requests, cx| {
        requests.answer_allow(DomainFamily::of_host("cdn.example.org"), cx);
      });
    });
    cx.run_until_parked();

    let status = view.read_with(cx, |_, cx| super::DocumentView::settings_error_message(cx));
    assert!(status.is_some_and(|message| message.starts_with("Settings could not be saved: ")));

    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir(&settings_path).unwrap();
    let second = Resource::Uri(second_url.into());
    assert!(load_image(&view, second.clone(), cx).is_none());
    cx.run_until_parked();
    assert!(load_image(&view, second, cx).is_none());
    cx.update(|_, app| {
      requests.update(app, |requests, cx| {
        requests.answer_allow(DomainFamily::of_host("cdn.example.net"), cx);
      });
    });
    cx.run_until_parked();

    assert!(view.read_with(cx, |_, cx| super::DocumentView::settings_error_message(cx).is_none()));
    assert!(
      Settings::load(&settings_path)
        .unwrap()
        .allowed_domains
        .iter()
        .any(|domain| domain == "example.net")
    );
  }
  #[gpui_kit::test]
  fn closing_a_document_drops_its_cached_images(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_settings_dir, _store) = install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("doc.md");
    fs::write(&path, "# Images\n").unwrap();
    fs::write(doc.path().join("img.png"), png(4, 3)).unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let resource = Resource::Uri("img.png".into());
    let _ = load_image(&view, resource.clone(), cx);
    cx.run_until_parked();
    assert!(matches!(load_image(&view, resource, cx), Some(Ok(_))));
    reset_released_image_count_for_test();
    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    assert!(released_image_count_for_test() >= 1);
  }
  #[cfg(test)]
  impl DocumentView {
    pub(crate) fn discard_and_close(&mut self, _: &Window, cx: &mut Context<Self>) {
      DocumentSession::discard_and_close(self, cx);
    }
    pub(crate) fn simulate_disk_change(&mut self, cx: &mut Context<Self>) {
      let window_handle = self.session.window_handle;
      cx.spawn(async move |this, cx| {
        let _ = cx.update_window(window_handle, |_, window, cx| {
          let _ = this.update(cx, |this, cx| this.on_disk_change(window, cx));
        });
      })
      .detach();
    }

    #[cfg(test)]
    pub(crate) fn apply_reload_for_test(
      &mut self,
      captured: Revision,
      text: String,
      window: &mut Window,
      cx: &mut Context<Self>,
    ) {
      let captured_disk = self.session.disk;
      let captured_observed_disk = self.session.observed_disk;
      let Some(disk) = self.session.observed_disk.or(self.session.disk) else {
        return;
      };
      DocumentSession::apply_reload_result(
        self,
        crate::session::ReloadParams {
          revision: captured,
          disk: captured_disk,
          observed_disk: captured_observed_disk,
        },
        Ok(Loaded { kind: self.session.kind, text, disk }),
        window,
        cx,
      );
    }

    #[cfg(test)]
    pub(crate) fn apply_reload_error_for_test(
      &mut self,
      captured: Revision,
      error: String,
      window: &mut Window,
      cx: &mut Context<Self>,
    ) {
      let captured_disk = self.session.disk;
      let captured_observed_disk = self.session.observed_disk;
      DocumentSession::apply_reload_result(
        self,
        crate::session::ReloadParams {
          revision: captured,
          disk: captured_disk,
          observed_disk: captured_observed_disk,
        },
        Err(error),
        window,
        cx,
      );
    }

    #[cfg(test)]
    pub(crate) fn apply_save_result_for_test(&mut self, saved: openit_core::save::Saved, cx: &mut Context<Self>) {
      DocumentSession::complete_save_result(self, saved, cx);
    }

    #[cfg(test)]
    pub(crate) fn watch_sender_for_test(&self) -> async_channel::Sender<()> {
      self.session.watch_sender.clone().expect("test watcher sender is initialized")
    }

    #[cfg(test)]
    pub(crate) const fn disk_for_test(&self) -> Option<Fingerprint> {
      self.session.disk
    }
  }
  #[cfg(test)]
  impl DocumentView {
    pub(crate) fn save_then_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
      DocumentSession::save_then_close(self, window, cx);
    }
    pub(crate) const fn mode(&self) -> Mode {
      self.mode
    }

    /// Whether the editor has been created.
    #[cfg(test)]
    pub(crate) const fn has_editor(&self) -> bool {
      self.editor.is_some()
    }

    /// Identity of the editor entity, for tests that assert it is retained.
    #[cfg(test)]
    pub(crate) fn editor_entity_id(&self) -> Option<EntityId> {
      self.editor.as_ref().map(Entity::entity_id)
    }

    /// Revision the preview was last parsed from.
    #[cfg(test)]
    pub(crate) const fn preview_revision(&self) -> Option<Revision> {
      self.preview_revision
    }
  }
}
