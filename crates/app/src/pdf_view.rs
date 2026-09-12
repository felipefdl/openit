//! The PDF document surface: continuous pages, zoom, page navigation, and the
//! password prompt.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::drop::{apply_external_paths, external_paths_ring};
use gpui_kit::base::{Scrollbar, ScrollbarHandle};
use gpui_kit::component::{ActiveTheme as _, Icon, TitleBar};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
  AnyElement, AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Bounds, ContentMask, Context, Corners,
  Entity, ExternalPaths, FocusHandle, IntoElement, MouseButton, ParentElement as _, PinchEvent, Pixels, Point, Render,
  ScrollDelta, ScrollWheelEvent, Size, Styled as _, Subscription, Task, Window, canvas, div, px, size,
};
use openit_core::document::{Loaded, LoadedPdf, Revision, load_pdf, load_text};
use openit_core::pdf::{DisplayRect, PageGeometry, PdfDocument, PdfOpenError, open_pdf, render_page};
use openit_core::pdf_markdown::{Conversion, ConvertOutcome, ConvertRequest, convert_to_markdown};
use openit_core::pdf_text;
use openit_core::pdf_text::{Match, TextLayer, TextPos};
use openit_core::save::save_bytes;
use openit_core::watch::{FileWatch, Fingerprint};

use crate::actions::{
  ActualSize, CloseWindow, CodeFont, ColorTheme, ConvertToMarkdown, Copy, Find, FirstPage, GoToFile, GoToPage,
  LastPage, NextMatch, PageDown, PageUp, PdfPages, PreviousMatch, SelectAll, ToggleMode, UiFont, ZoomIn, ZoomOut,
  ZoomToFit,
};
use crate::document_view::DocumentView;
use crate::font_picker::{FontPicker, FontPickerEvent, FontSlot};
use crate::image_decode;
use crate::nearby_picker::{NearbyPicker, NearbyPickerEvent};
use crate::pdf_find::{FindBar, FindBarEvent};
use crate::pdf_prompts::{GoToPage as GoToPagePrompt, GoToPageEvent, PasswordPrompt, PasswordPromptEvent};
use crate::session::PendingCleanups;
use crate::theme_picker::{ThemePicker, ThemePickerEvent};
use crate::title_bar::{file_name, toolbar_button};

/// Space above, below, and between pages, in window pixels.
const PAGE_GAP: f32 = 16.0;
/// Zoom bounds, so a wheel flick cannot leave a page invisible or fill memory.
const MIN_SCALE: f32 = 0.1;
const MAX_SCALE: f32 = 8.0;
/// One zoom step.
const ZOOM_STEP: f32 = 1.25;
/// Rendered pages kept per window, in bytes of BGRA.
const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;
/// Pages rendered ahead of and behind the visible ones.
const PREFETCH: usize = 1;
/// In-flight `render_page` jobs kept at once.
const MAX_RENDER_JOBS: usize = 4;

/// How the pages are sized.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zoom {
  /// The widest page fills the window.
  FitWidth,
  /// One PDF point per this many window pixels.
  Scale(f32),
}

/// Where the document stands.
enum Load {
  /// Parsing off the UI thread.
  Opening,
  /// Waiting for a password; `wrong` after a rejected attempt.
  Locked { wrong: bool },
  /// Ready to read.
  Ready(Arc<PdfDocument>),
  /// Nothing to show but the reason.
  Failed(String),
}

/// The scroll position the pages and their scrollbar share. GPUI's scroll
/// offsets grow negative downwards; the reader's own `scroll_y` grows positive,
/// so the two differ by a sign.
#[derive(Clone, Default)]
struct PageScroll(Rc<PageScrollInner>);

#[derive(Default)]
struct PageScrollInner {
  viewport: Cell<Bounds<Pixels>>,
  content: Cell<Size<Pixels>>,
  offset: Cell<Point<Pixels>>,
}

impl PageScroll {
  /// What the scrollbar asks for, as a downward distance.
  fn requested(&self) -> Pixels {
    -self.0.offset.get().y
  }

  /// Publish the geometry the thumb is sized against.
  fn publish(&self, viewport: Bounds<Pixels>, content: Size<Pixels>) {
    self.0.viewport.set(viewport);
    self.0.content.set(content);
  }

  /// Publish where the reader is now, so the next frame does not read a
  /// position the reader has already left.
  fn set_position(&self, scroll_y: Pixels) {
    self.0.offset.set(Point { x: px(0.), y: -scroll_y });
  }
}

impl ScrollbarHandle for PageScroll {
  fn viewport_bounds(&self) -> Bounds<Pixels> {
    self.0.viewport.get()
  }

  fn offset(&self) -> Point<Pixels> {
    self.0.offset.get()
  }

  fn set_offset(&self, offset: Point<Pixels>) {
    self.0.offset.set(offset);
  }

  fn content_size(&self) -> Size<Pixels> {
    self.0.content.get()
  }
}

/// Which of the window's three views is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
  /// The pages themselves.
  Pdf,
  /// The generated Markdown, in one of its two modes.
  Markdown,
}

/// A conversion in flight.
struct ConvertJob {
  #[allow(dead_code, reason = "dropping the task cancels the UI half of the job")]
  task: Task<()>,
  cancelled: Arc<std::sync::atomic::AtomicBool>,
  progress: Option<(u32, u32)>,
}

/// How far the text layer has come.
enum Text {
  /// Waiting for the document to parse.
  Pending,
  /// Reading the runs off the UI thread.
  Indexing,
  /// Every run, ready for search and selection.
  Ready(Arc<TextLayer>),
  /// The runs could not be read.
  Failed(String),
}

/// The overlay floating over the pages, at most one at a time.
enum Prompt {
  /// The protected-document password field.
  Password(Entity<PasswordPrompt>),
  /// The go-to-page field.
  GoToPage(Entity<GoToPagePrompt>),
  /// The theme picker.
  Theme(Entity<ThemePicker>),
  /// The font picker.
  Font(Entity<FontPicker>),
  /// The nearby-files picker.
  Nearby(Entity<NearbyPicker>),
}

impl Prompt {
  fn element(&self) -> AnyElement {
    match self {
      Self::Password(view) => view.clone().into_any_element(),
      Self::GoToPage(view) => view.clone().into_any_element(),
      Self::Theme(view) => view.clone().into_any_element(),
      Self::Font(view) => view.clone().into_any_element(),
      Self::Nearby(view) => view.clone().into_any_element(),
    }
  }
}

/// One PDF document in its own window.
pub struct PdfView {
  path: PathBuf,
  bytes: Arc<Vec<u8>>,
  disk: Fingerprint,
  password: Option<String>,
  load: Load,
  open_task: Option<Task<()>>,
  /// Bumped on every reload and zoom change; every render carries it.
  generation: u64,
  zoom: Zoom,
  scroll_y: Pixels,
  viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
  scroll: PageScroll,
  cache: PageCache,
  render_tasks: HashMap<(usize, u32), Task<()>>,
  render_scale_key: Option<u32>,
  layout_cache: RefCell<Option<CachedLayout>>,
  overlay_cache: Option<OverlayCache>,
  text: Text,
  text_task: Option<Task<()>>,
  find: Option<Entity<FindBar>>,
  find_subscription: Option<Subscription>,
  markdown_subscription: Option<Subscription>,
  matches: Arc<[Match]>,
  current_match: Option<usize>,
  search_task: Option<Task<()>>,
  selection: Option<(TextPos, TextPos)>,
  drag_anchor: Option<TextPos>,
  convert: Option<ConvertJob>,
  notice: Option<String>,
  view: View,
  /// The generated Markdown document, once it exists on disk.
  markdown: Option<Entity<DocumentView>>,
  prompt: Option<Prompt>,
  prompt_subscription: Option<Subscription>,
  closing: bool,
  close_decided: bool,
  watch_task: Option<Task<()>>,
  reload_task: Option<Task<()>>,
  #[allow(dead_code, reason = "the handle keeps the platform watcher alive")]
  watch: Option<FileWatch>,
  #[cfg(test)]
  watch_sender: Option<async_channel::Sender<()>>,
  window_handle: AnyWindowHandle,
  focus: FocusHandle,
  #[allow(dead_code, reason = "the subscription keeps the appearance observer alive")]
  appearance_observation: Option<Subscription>,
}

impl PdfView {
  /// Open a PDF document read from disk. Parsing runs off the UI thread, so
  /// the window appears at once and fills in when the pages are known.
  pub fn open(path: PathBuf, loaded: LoadedPdf, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let mut view = Self {
      path,
      bytes: loaded.bytes,
      disk: loaded.disk,
      password: None,
      load: Load::Opening,
      open_task: None,
      generation: 0,
      zoom: Zoom::FitWidth,
      scroll_y: px(0.),
      viewport: Rc::new(Cell::new(None)),
      scroll: PageScroll::default(),
      cache: PageCache::default(),
      render_tasks: HashMap::new(),
      render_scale_key: None,
      layout_cache: RefCell::new(None),
      overlay_cache: None,
      text: Text::Pending,
      text_task: None,
      find: None,
      find_subscription: None,
      markdown_subscription: None,
      matches: Arc::from([]),
      current_match: None,
      search_task: None,
      selection: None,
      drag_anchor: None,
      convert: None,
      notice: None,
      view: View::Pdf,
      markdown: None,
      prompt: None,
      prompt_subscription: None,
      closing: false,
      close_decided: false,
      watch_task: None,
      reload_task: None,
      watch: None,
      #[cfg(test)]
      watch_sender: None,
      window_handle: window.window_handle(),
      focus: cx.focus_handle(),
      appearance_observation: Some(crate::theme::observe_appearance(window)),
    };
    window.focus(&view.focus, cx);
    Self::install_close_guard(window, cx);
    view.start_open(window, cx);
    view.start_watch(window, cx);
    view
  }

  /// The file name, for the title bar.
  pub fn title(&self) -> String {
    self
      .path
      .file_name()
      .map_or_else(|| "Untitled".to_owned(), |name| name.to_string_lossy().into_owned())
  }
  pub(crate) fn path(&self) -> &Path {
    &self.path
  }

  /// Prompt if generated Markdown is dirty, then `on_ready`. Cancel leaves the buffer.
  pub(crate) fn confirm_leave(
    &self,
    window: &mut Window,
    cx: &mut Context<Self>,
    on_ready: impl FnOnce(&mut Window, &mut App) + 'static,
  ) {
    if let Some(markdown) = self.markdown.clone() {
      markdown.update(cx, |document, cx| document.confirm_leave(window, cx, on_ready));
      return;
    }
    on_ready(window, cx);
  }

  pub(crate) fn end_session_without_draft(&self, cx: &mut Context<Self>) {
    if let Some(markdown) = self.markdown.clone() {
      markdown.update(cx, |document, cx| document.end_session_without_draft(cx));
    }
  }

  /// The parsed document, once it is ready.
  const fn document(&self) -> Option<&Arc<PdfDocument>> {
    match &self.load {
      Load::Ready(document) => Some(document),
      _ => None,
    }
  }

  /// How many pages the document has.
  pub fn page_count(&self) -> Option<usize> {
    self.document().map(|document| document.page_count())
  }

  /// The page filling the middle of the window.
  pub fn current_page(&self) -> Option<usize> {
    let document = self.document()?;
    let viewport = self.viewport.get()?;
    let layout = layout(document.pages(), self.scale(), viewport.size.width);
    Some(current_page(&layout, self.scroll_y, viewport.size.height))
  }

  /// The left side of the status bar.
  pub fn status_text(&self) -> Option<String> {
    let count = self.page_count()?;
    let page = self.current_page().unwrap_or(0).saturating_add(1);
    Some(format!("Page {page} of {count}"))
  }

  /// The right side of the status bar.
  pub fn zoom_percent(&self) -> Option<u32> {
    self.document().map(|_| percent(self.scale()))
  }

  /// The message shown in place of the pages, when the document cannot be read.
  pub const fn failure(&self) -> Option<&str> {
    match &self.load {
      Load::Failed(reason) => Some(reason.as_str()),
      _ => None,
    }
  }

  /// Whether the window is waiting for a password.
  pub const fn is_locked(&self) -> bool {
    matches!(self.load, Load::Locked { .. })
  }

  /// Parse the document, prompting for a password when it needs one.
  fn start_open(&mut self, window: &Window, cx: &Context<Self>) {
    let bytes = Arc::clone(&self.bytes);
    let password = self.password.clone();
    let generation = self.generation;
    self.open_task = Some(cx.spawn_in(window, async move |view, cx| {
      let opened = cx.background_spawn(async move { open_pdf(bytes, password.as_deref()) }).await;
      let _ = view.update_in(cx, |view, window, cx| {
        if view.generation != generation {
          return;
        }
        match opened {
          Ok(document) => {
            view.load = Load::Ready(Arc::new(document));
            view.close_prompt(window, cx);
            view.start_indexing(window, cx);
          },
          Err(PdfOpenError::NeedsPassword) => {
            view.load = Load::Locked { wrong: false };
            view.open_password_prompt(window, cx);
          },
          Err(PdfOpenError::WrongPassword) => {
            view.load = Load::Locked { wrong: true };
            view.open_password_prompt(window, cx);
          },
          Err(error) => {
            view.load = Load::Failed(error.to_string());
            view.close_prompt(window, cx);
          },
        }
        cx.notify();
      });
    }));
  }

  /// Try `password` on a protected document.
  pub fn submit_password(&mut self, password: String, window: &Window, cx: &mut Context<Self>) {
    self.password = Some(password);
    self.load = Load::Opening;
    self.cache.clear();
    self.render_tasks.clear();
    self.render_scale_key = None;
    self.layout_cache.replace(None);
    self.overlay_cache = None;
    self.start_open(window, cx);
    cx.notify();
  }

  fn open_password_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let wrong = matches!(self.load, Load::Locked { wrong: true });
    if let Some(Prompt::Password(prompt)) = &self.prompt
      && !wrong
    {
      prompt.update(cx, |prompt, cx| prompt.focus(window, cx));
      return;
    }
    let prompt = cx.new(|cx| PasswordPrompt::new(wrong, window, cx));
    self.prompt_subscription = Some(cx.subscribe_in(
      &prompt,
      window,
      move |view, _, event: &PasswordPromptEvent, window, cx| match event {
        PasswordPromptEvent::Submit(password) => {
          view.prompt = None;
          view.prompt_subscription = None;
          view.submit_password(password.clone(), window, cx);
        },
        PasswordPromptEvent::Cancel => {
          view.load = Load::Failed("This PDF is password protected".to_owned());
          view.close_prompt(window, cx);
        },
      },
    ));
    self.prompt = Some(Prompt::Password(prompt));
    cx.notify();
  }

  fn open_go_to_page(&mut self, _: &GoToPage, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Prompt::GoToPage(prompt)) = &self.prompt {
      prompt.update(cx, |prompt, cx| prompt.focus(window, cx));
      return;
    }
    let Some(count) = self.page_count() else {
      return;
    };
    let prompt = cx.new(|cx| GoToPagePrompt::new(self.current_page().unwrap_or(0), count, window, cx));
    self.prompt_subscription =
      Some(
        cx.subscribe_in(&prompt, window, move |view, _, event: &GoToPageEvent, window, cx| {
          if let GoToPageEvent::Jump(page) = event {
            view.scroll_to_page(*page, cx);
          }
          view.close_prompt(window, cx);
        }),
      );
    self.prompt = Some(Prompt::GoToPage(prompt));
    cx.notify();
  }

  fn open_theme_picker(&mut self, _: &ColorTheme, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Prompt::Theme(_)) = &self.prompt {
      return;
    }
    if let Some(Prompt::Font(picker)) = &self.prompt {
      picker.update(cx, |picker, cx| picker.finish(window, cx));
    }
    let picker = cx.new(|cx| ThemePicker::new(window, cx));
    self.prompt_subscription =
      Some(
        cx.subscribe_in(&picker, window, move |view, _, event: &ThemePickerEvent, window, cx| {
          if matches!(event, ThemePickerEvent::Close) {
            view.close_prompt(window, cx);
          }
        }),
      );
    self.prompt = Some(Prompt::Theme(picker));
    cx.notify();
  }

  fn open_ui_font_picker(&mut self, _: &UiFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Ui, window, cx);
  }

  fn open_code_font_picker(&mut self, _: &CodeFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Code, window, cx);
  }

  fn open_font_picker(&mut self, slot: FontSlot, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Prompt::Font(picker)) = &self.prompt {
      if picker.read(cx).slot() == slot {
        picker.update(cx, |picker, cx| picker.focus(window, cx));
        return;
      }
      picker.update(cx, |picker, cx| picker.finish(window, cx));
    } else if let Some(Prompt::Theme(picker)) = &self.prompt {
      picker.update(cx, |picker, cx| picker.finish(window, cx));
    }
    let picker = cx.new(|cx| FontPicker::new(slot, window, cx));
    self.prompt_subscription =
      Some(
        cx.subscribe_in(&picker, window, move |view, _, event: &FontPickerEvent, window, cx| {
          if matches!(event, FontPickerEvent::Close) {
            view.close_prompt(window, cx);
          }
        }),
      );
    self.prompt = Some(Prompt::Font(picker));
    cx.notify();
  }

  fn open_nearby_picker(&mut self, _: &GoToFile, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(Prompt::Nearby(_)) = &self.prompt {
      return;
    }
    let picker = cx.new(|cx| NearbyPicker::new(Some(&self.path), window, cx));
    self.prompt_subscription =
      Some(
        cx.subscribe_in(&picker, window, move |view, _, event: &NearbyPickerEvent, window, cx| {
          if matches!(event, NearbyPickerEvent::Close) {
            view.close_prompt(window, cx);
          }
        }),
      );
    self.prompt = Some(Prompt::Nearby(picker));
    cx.notify();
  }

  fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.prompt = None;
    self.prompt_subscription = None;
    window.focus(&self.focus, cx);
    cx.notify();
  }

  /// The scale in effect, resolving `FitWidth` against the current viewport.
  fn scale(&self) -> f32 {
    match self.zoom {
      Zoom::Scale(scale) => scale,
      Zoom::FitWidth => {
        let Some((document, viewport)) = self.document().zip(self.viewport.get()) else {
          return 1.0;
        };
        fit_width_scale(document.pages(), viewport.size.width)
      },
    }
  }

  /// Every page's place at the scale and viewport in effect.
  fn layout(&self) -> Option<(Layout, Bounds<Pixels>)> {
    let document = self.document()?;
    let viewport = self.viewport.get()?;
    let page_count = document.page_count();
    let scale = self.scale();
    let key = scale_key(scale);
    let viewport_width = round_to_u32(f32::from(viewport.size.width));
    if let Some(cached) = self.layout_cache.borrow().as_ref()
      && cached.page_count == page_count
      && cached.scale_key == key
      && cached.viewport_width == viewport_width
    {
      return Some((cached.layout.clone(), viewport));
    }
    let laid = layout(document.pages(), scale, viewport.size.width);
    self.layout_cache.replace(Some(CachedLayout {
      page_count,
      scale_key: key,
      viewport_width,
      layout: laid.clone(),
    }));
    Some((laid, viewport))
  }

  fn set_scroll(&mut self, y: Pixels, cx: &mut Context<Self>) {
    let limit = self.layout().map_or(px(0.), |(layout, viewport)| {
      (layout.total_height - viewport.size.height).max(px(0.))
    });
    let clamped = y.clamp(px(0.), limit);
    if clamped != self.scroll_y {
      self.scroll_y = clamped;
      self.scroll.set_position(clamped);
      cx.notify();
    }
  }

  /// Adopt a position the scrollbar's thumb asked for.
  fn follow_scrollbar(&mut self, cx: &mut Context<Self>) {
    let requested = self.scroll.requested();
    if (requested - self.scroll_y).abs() > px(0.5) {
      self.set_scroll(requested, cx);
    }
  }

  /// Scroll so `page` sits at the top of the window.
  pub fn scroll_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
    let Some((layout, _)) = self.layout() else {
      return;
    };
    let Some(bounds) = layout.pages.get(page) else {
      return;
    };
    self.set_scroll(bounds.origin.y - px(PAGE_GAP), cx);
  }

  /// Read every text run in the background, so search and selection have
  /// something to work with.
  fn start_indexing(&mut self, window: &Window, cx: &Context<Self>) {
    let Some(page_count) = self.page_count() else {
      return;
    };
    self.text = Text::Indexing;
    let path = self.path.clone();
    let bytes = Arc::clone(&self.bytes);
    let password = self.password.clone();
    let generation = self.generation;
    self.text_task = Some(cx.spawn_in(window, async move |view, cx| {
      let read = cx
        .background_spawn(async move { pdf_text::text_layer(&path, &bytes, password.as_deref(), page_count) })
        .await;
      let _ = view.update_in(cx, |view, window, cx| {
        if view.generation != generation {
          return;
        }
        match read {
          Ok(layer) => {
            view.text = Text::Ready(Arc::new(layer));
            // A query typed while indexing ran gets its answer now.
            if let Some(find) = view.find.clone() {
              let query = find.read(cx).query(cx);
              view.run_search(query, window, cx);
            }
          },
          Err(error) => view.text = Text::Failed(error.to_string()),
        }
        cx.notify();
      });
    }));
  }

  /// The text layer, once it is read.
  const fn layer(&self) -> Option<&Arc<TextLayer>> {
    match &self.text {
      Text::Ready(layer) => Some(layer),
      _ => None,
    }
  }

  /// Whether the text layer is still being read.
  pub const fn is_indexing(&self) -> bool {
    matches!(self.text, Text::Indexing)
  }

  /// Why the text layer could not be read, when it could not.
  pub const fn text_failure(&self) -> Option<&str> {
    match &self.text {
      Text::Failed(reason) => Some(reason.as_str()),
      _ => None,
    }
  }

  /// Every match of the current query.
  #[cfg(test)]
  pub(crate) fn matches(&self) -> &[Match] {
    self.matches.as_ref()
  }

  /// Which match the reader is on.
  #[cfg(test)]
  pub(crate) const fn current_match(&self) -> Option<usize> {
    self.current_match
  }

  /// The selected text, when a selection exists.
  pub fn selected_text(&self) -> Option<String> {
    let (layer, (start, end)) = self.layer().zip(self.selection)?;
    Some(pdf_text::text_between(layer, start, end))
  }

  /// The boxes of every match, keyed by page, in displayed points.
  fn match_rects(&self, visible: Range<usize>) -> Overlays {
    let skip = self.current_match;
    self.overlay_rects(
      self
        .matches
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != skip)
        .map(|(_, hit)| (hit.start, hit.end)),
      visible,
    )
  }

  /// The boxes of the match the reader is on.
  fn current_match_rects(&self, visible: Range<usize>) -> Overlays {
    self.overlay_rects(
      self
        .current_match
        .and_then(|index| self.matches.get(index))
        .map(|hit| (hit.start, hit.end))
        .into_iter(),
      visible,
    )
  }

  /// The boxes of the selection.
  fn selection_rects(&self, visible: Range<usize>) -> Overlays {
    self.overlay_rects(self.selection.into_iter(), visible)
  }

  /// Cached overlay boxes for the pages currently in view.
  fn overlays_for(&mut self, visible: Range<usize>) -> (Overlays, Overlays, Overlays) {
    let key = scale_key(self.scale());
    let reuse = self.overlay_cache.as_ref().is_some_and(|cache| {
      Arc::ptr_eq(&cache.matches, &self.matches)
        && cache.current == self.current_match
        && cache.selection == self.selection
        && cache.scale_key == key
        && cache.visible == visible
    });
    if reuse && let Some(cache) = &self.overlay_cache {
      return (
        cache.match_overlays.clone(),
        cache.current_overlays.clone(),
        cache.selection_overlays.clone(),
      );
    }
    let match_overlays = self.match_rects(visible.clone());
    let current_overlays = self.current_match_rects(visible.clone());
    let selection_overlays = self.selection_rects(visible.clone());
    self.overlay_cache = Some(OverlayCache {
      matches: Arc::clone(&self.matches),
      current: self.current_match,
      selection: self.selection,
      scale_key: key,
      visible,
      match_overlays: match_overlays.clone(),
      current_overlays: current_overlays.clone(),
      selection_overlays: selection_overlays.clone(),
    });
    (match_overlays, current_overlays, selection_overlays)
  }

  /// Map item-frame ranges into displayed boxes per visible page.
  fn overlay_rects(&self, ranges: impl Iterator<Item = (TextPos, TextPos)>, visible: Range<usize>) -> Overlays {
    let mut overlays: Overlays = HashMap::new();
    let Some((layer, document)) = self.layer().zip(self.document()) else {
      return overlays;
    };
    for (start, end) in ranges {
      if end.page < visible.start || start.page >= visible.end {
        continue;
      }
      for (page, rect) in pdf_text::rects_between(layer, start, end) {
        if !visible.contains(&page) {
          continue;
        }
        let Some(geometry) = document.pages().get(page) else {
          continue;
        };
        overlays.entry(page).or_default().push(geometry.to_display(rect));
      }
    }
    overlays
  }

  /// Whether a conversion is running.
  pub const fn is_converting(&self) -> bool {
    self.convert.is_some()
  }

  /// The short message the status bar shows after a conversion.
  pub fn notice(&self) -> Option<&str> {
    self.notice.as_deref()
  }

  /// Whether the window is showing the generated Markdown.
  pub const fn shows_markdown(&self) -> bool {
    matches!(self.view, View::Markdown)
  }

  /// The generated Markdown document, once it exists.
  #[cfg(test)]
  pub(crate) const fn markdown(&self) -> Option<&Entity<DocumentView>> {
    self.markdown.as_ref()
  }

  /// The name the title bar shows: the PDF, or the Markdown when that view is up.
  fn window_title(&self, cx: &App) -> String {
    self
      .markdown
      .as_ref()
      .filter(|_| self.shows_markdown())
      .map_or_else(|| self.title(), |document| document.read(cx).title())
  }

  /// The Markdown button's icon and tooltip for the state it is in: the pen
  /// generates, the open book edits, the eye returns to the preview.
  fn markdown_button(&self, cx: &App) -> (Icon, &'static str) {
    if self.is_converting() {
      return (Icon::empty().path("icons/notebook-pen.svg"), "Generating… (click to cancel)");
    }
    let Some(document) = self.markdown.as_ref() else {
      return (Icon::empty().path("icons/notebook-pen.svg"), "Generate Markdown (Cmd+Shift+M)");
    };
    if self.shows_markdown() && document.read(cx).is_editing() {
      (Icon::new(gpui_kit::component::IconName::Eye), "Markdown preview (Cmd+Shift+E)")
    } else if self.shows_markdown() {
      (
        Icon::empty().path("icons/book-open-text.svg"),
        "Edit the Markdown (Cmd+Shift+E)",
      )
    } else {
      (Icon::new(gpui_kit::component::IconName::Eye), "Show the Markdown")
    }
  }

  /// Show the pages again.
  fn show_pdf(&mut self, _: &PdfPages, window: &mut Window, cx: &mut Context<Self>) {
    self.view = View::Pdf;
    window.focus(&self.focus, cx);
    cx.notify();
  }

  /// The Markdown button: generate the document, then move between its preview
  /// and its editor.
  fn show_markdown(&mut self, _: &ConvertToMarkdown, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(job) = self.convert.take() {
      job.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
      cx.notify();
      return;
    }
    let Some(markdown) = self.markdown.clone() else {
      self.generate_markdown(window, cx);
      return;
    };
    if self.view == View::Markdown {
      markdown.update(cx, |document, cx| document.toggle_mode(&ToggleMode, window, cx));
    } else {
      self.view = View::Markdown;
      markdown.update(cx, |document, cx| document.focus_surface(window, cx));
    }
    cx.notify();
  }

  /// Generate the Markdown, write it beside the PDF, and show it.
  fn generate_markdown(&mut self, window: &Window, cx: &mut Context<Self>) {
    let Some(document) = self.document().map(Arc::clone) else {
      return;
    };
    let stem = self
      .path
      .file_stem()
      .map_or_else(|| "Untitled".to_owned(), |stem| stem.to_string_lossy().into_owned());
    let folder = self
      .path
      .parent()
      .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
    let path = self.path.clone();
    let bytes = Arc::clone(&self.bytes);
    let password = self.password.clone();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let generation = self.generation;
    let (sender, receiver) = async_channel::unbounded::<(u32, u32)>();
    let flag = Arc::clone(&cancelled);
    let task = cx.spawn_in(window, async move |view, cx| {
      let progress_task = report_progress(view.clone(), receiver, cx);
      let generated = cx
        .background_spawn({
          let stem = stem.clone();
          async move {
            let request = ConvertRequest {
              document: &document,
              path: &path,
              bytes: &bytes,
              password: password.as_deref(),
              stem: &stem,
            };
            let outcome = convert_to_markdown(
              &request,
              &mut |done, total| {
                let _ = sender.try_send((done, total));
              },
              &flag,
            )?;
            match outcome {
              ConvertOutcome::NoText => Ok(None),
              ConvertOutcome::Converted(conversion) => write_markdown(&folder, &stem, &conversion).map(Some),
            }
          }
        })
        .await;
      drop(progress_task);
      let _ = view.update_in(cx, |view, window, cx| {
        view.convert = None;
        if view.generation != generation {
          return;
        }
        match generated {
          Ok(Some(written)) => view.adopt_markdown(written, window, cx),
          Ok(None) => view.notice = Some("This PDF has no text to convert.".to_owned()),
          Err(error) => {
            let reason = error.to_string();
            if reason != "Conversion cancelled" {
              view.notice = Some(format!("{} could not be converted: {reason}", view.title()));
            }
          },
        }
        cx.notify();
      });
    });
    self.convert = Some(ConvertJob { task, cancelled, progress: None });
    self.notice = None;
    cx.notify();
  }

  /// Open the file the generation wrote and show it.
  fn adopt_markdown(&mut self, written: Written, window: &mut Window, cx: &mut Context<Self>) {
    let name = written.path.file_name().map_or_else(
      || written.path.display().to_string(),
      |name| name.to_string_lossy().into_owned(),
    );
    self.notice = Some(match (written.in_temp_folder, written.skipped_pages) {
      (false, 0) => format!("Saved {name}"),
      (false, skipped) => format!("Saved {name} · {skipped} pages skipped (no text)"),
      (true, 0) => format!("Saved {} (the document's folder is read-only)", written.path.display()),
      (true, skipped) => format!(
        "Saved {} (the document's folder is read-only) · {skipped} pages skipped (no text)",
        written.path.display()
      ),
    });
    let document = cx.new(|cx| DocumentView::open_embedded(written.path, written.loaded, window, cx));
    self.markdown_subscription = Some(cx.observe_in(&document, window, |_, _, _, cx| cx.notify()));
    self.markdown = Some(document);
    self.view = View::Markdown;
    cx.notify();
  }

  fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(find) = &self.find {
      find.update(cx, |find, cx| find.focus(window, cx));
      return;
    }
    let bar = cx.new(|cx| FindBar::new(window, cx));
    self.find_subscription =
      Some(
        cx.subscribe_in(&bar, window, move |view, _, event: &FindBarEvent, window, cx| match event {
          FindBarEvent::QueryChanged(query) => view.run_search(query.clone(), window, cx),
          FindBarEvent::Pick(index) => view.set_current_match(Some(*index), cx),
          FindBarEvent::Close => view.close_find(window, cx),
        }),
      );
    self.find = Some(bar);
    cx.notify();
  }

  fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.find = None;
    self.find_subscription = None;
    self.matches = Arc::from([]);
    self.current_match = None;
    self.search_task = None;
    self.overlay_cache = None;
    window.focus(&self.focus, cx);
    cx.notify();
  }

  /// Search the text layer for `query`, off the UI thread.
  fn run_search(&mut self, query: String, window: &Window, cx: &Context<Self>) {
    let Some(layer) = self.layer().map(Arc::clone) else {
      return;
    };
    let generation = self.generation;
    self.search_task = Some(cx.spawn_in(window, async move |view, cx| {
      let hits = cx.background_spawn(async move { pdf_text::search(&layer, &query) }).await;
      let _ = view.update(cx, |view, cx| {
        if view.generation != generation {
          return;
        }
        let first = (!hits.is_empty()).then_some(0);
        view.matches = Arc::from(hits);
        view.overlay_cache = None;
        view.set_current_match(first, cx);
      });
    }));
  }

  /// Make `index` the current match, scroll it into view, and tell the bar.
  fn set_current_match(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
    self.current_match = index.filter(|index| *index < self.matches.len());
    if let Some(start) = self
      .current_match
      .and_then(|index| self.matches.get(index))
      .map(|hit| hit.start)
    {
      self.scroll_into_view(start, cx);
    }
    if let Some(find) = self.find.clone() {
      let matches = Arc::clone(&self.matches);
      let current = self.current_match;
      find.update(cx, |find, cx| find.set_results(matches, current, cx));
    }
    cx.notify();
  }

  /// Step forward or back through the matches, wrapping at both ends.
  fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
    if self.matches.is_empty() {
      return;
    }
    let total = self.matches.len();
    let current = self.current_match.unwrap_or(0);
    let next = if delta >= 0 {
      current.saturating_add(1) % total
    } else if current == 0 {
      total.saturating_sub(1)
    } else {
      current.saturating_sub(1)
    };
    self.set_current_match(Some(next), cx);
  }

  fn next_match(&mut self, _: &NextMatch, _window: &mut Window, cx: &mut Context<Self>) {
    self.step_match(1, cx);
  }

  fn previous_match(&mut self, _: &PreviousMatch, _window: &mut Window, cx: &mut Context<Self>) {
    self.step_match(-1, cx);
  }

  /// Scroll so the character at `position` sits a third of the way down the
  /// window, clear of the find bar.
  fn scroll_into_view(&mut self, position: TextPos, cx: &mut Context<Self>) {
    let Some((layout, viewport)) = self.layout() else {
      return;
    };
    let Some(document) = self.document() else {
      return;
    };
    let Some(rect) = self.layer().and_then(|layer| pdf_text::char_rect(layer, position)) else {
      return;
    };
    let Some(geometry) = document.pages().get(position.page).copied() else {
      return;
    };
    let Some(bounds) = layout.pages.get(position.page) else {
      return;
    };
    let shown = geometry.to_display(rect);
    let top = bounds.origin.y + px(shown.y * self.scale());
    // The find bar covers the top of the pages, so a match lands lower while
    // it is open.
    let fraction = if self.find.is_some() { 0.6 } else { 1. / 3. };
    self.set_scroll(top - viewport.size.height * fraction, cx);
  }

  fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(range) = self.layer().and_then(|layer| pdf_text::document_range(layer)) else {
      return;
    };
    self.selection = Some(range);
    cx.notify();
  }

  fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(text) = self.selected_text().filter(|text| !text.is_empty()) else {
      return;
    };
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
  }

  /// The text layer, for tests.
  #[cfg(test)]
  pub(crate) fn text_layer(&self) -> Option<Arc<TextLayer>> {
    self.layer().map(Arc::clone)
  }

  /// Select the word under a point in the item frame of `page`.
  #[cfg(test)]
  pub(crate) fn select_word_at(&mut self, page: usize, x: f32, y: f32, cx: &mut Context<Self>) {
    let Some(layer) = self.layer().map(Arc::clone) else {
      return;
    };
    let Some(position) = pdf_text::position_at(&layer, page, x, y) else {
      return;
    };
    let (start, end) = pdf_text::word_at(&layer, position);
    self.selection = Some((start, end));
    cx.notify();
  }

  /// Whether a point belongs to the scrollbar's gutter rather than the pages,
  /// so a drag on the thumb never starts a text selection.
  fn in_scrollbar(&self, point: Point<Pixels>) -> bool {
    self
      .viewport
      .get()
      .is_some_and(|viewport| point.x >= viewport.origin.x + viewport.size.width - Scrollbar::width())
  }

  /// The document position under a window point, when it lands on a page.
  fn position_at_point(&self, point: Point<Pixels>) -> Option<TextPos> {
    let (layout, viewport) = self.layout()?;
    let document = self.document()?;
    let layer = self.layer()?;
    let scale = self.scale();
    let x = point.x - viewport.origin.x;
    let y = point.y - viewport.origin.y + self.scroll_y;
    for (page, bounds) in layout.pages.iter().enumerate() {
      if y < bounds.origin.y || y > bounds.origin.y + bounds.size.height {
        continue;
      }
      let geometry = document.pages().get(page).copied()?;
      let display_x = f32::from(x - bounds.origin.x) / scale;
      let display_y = f32::from(y - bounds.origin.y) / scale;
      let (item_x, item_y) = geometry.to_page(display_x, display_y);
      return pdf_text::position_at(layer, page, item_x, item_y);
    }
    None
  }

  fn on_mouse_down(&mut self, event: &gpui_kit::MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if self.in_scrollbar(event.position) {
      return;
    }
    let Some(position) = self.position_at_point(event.position) else {
      self.selection = None;
      self.drag_anchor = None;
      cx.notify();
      return;
    };
    if event.click_count >= 2 {
      let Some(layer) = self.layer().map(Arc::clone) else {
        return;
      };
      let (start, end) = pdf_text::word_at(&layer, position);
      self.selection = Some((start, end));
      self.drag_anchor = None;
      cx.notify();
      return;
    }
    if event.modifiers.shift
      && let Some(anchor) = self.drag_anchor.or_else(|| self.selection.map(|(start, _)| start))
    {
      self.selection = Some(pdf_text::ordered(anchor, position));
      self.drag_anchor = Some(anchor);
      cx.notify();
      return;
    }
    self.drag_anchor = Some(position);
    self.selection = None;
    cx.notify();
  }

  fn on_mouse_move(&mut self, event: &gpui_kit::MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if self.in_scrollbar(event.position) {
      return;
    }
    let Some(anchor) = self.drag_anchor else {
      return;
    };
    if !event.dragging() {
      return;
    }
    if let Some(position) = self.position_at_point(event.position) {
      self.selection = Some(pdf_text::ordered(anchor, position));
      cx.notify();
    }
  }

  fn on_mouse_up(&mut self, _: &gpui_kit::MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
    self.drag_anchor = None;
    cx.notify();
  }

  /// Change the scale, keeping the document point under `anchor` (or the
  /// middle of the window) where it is.
  fn rescale(&mut self, zoom: Zoom, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
    let previous = self.scale();
    let scale = match zoom {
      Zoom::Scale(scale) => scale.clamp(MIN_SCALE, MAX_SCALE),
      Zoom::FitWidth => self
        .document()
        .zip(self.viewport.get())
        .map_or(previous, |(document, viewport)| {
          fit_width_scale(document.pages(), viewport.size.width)
        }),
    };
    let zoom = match zoom {
      Zoom::FitWidth => Zoom::FitWidth,
      Zoom::Scale(_) => Zoom::Scale(scale),
    };
    if zoom == self.zoom && (scale - previous).abs() < f32::EPSILON {
      return;
    }
    let viewport = self.viewport.get();
    let anchor_y = anchor
      .map(|point| point.y)
      .or_else(|| viewport.map(|viewport| viewport.origin.y + viewport.size.height / 2.));
    let offset = anchor_y
      .zip(viewport)
      .map(|(y, viewport)| y - viewport.origin.y)
      .unwrap_or_default();
    let document_y = self.scroll_y + offset;
    self.zoom = zoom;
    self.generation = self.generation.saturating_add(1);
    self.set_scroll(document_y * (scale / previous) - offset, cx);
    cx.notify();
  }

  /// Set an explicit scale, keeping the document point under `anchor` in place.
  fn set_scale(&mut self, scale: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
    self.rescale(Zoom::Scale(scale), anchor, cx);
  }

  fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
    self.set_scale(self.scale() * ZOOM_STEP, None, cx);
  }

  fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
    self.set_scale(self.scale() / ZOOM_STEP, None, cx);
  }

  fn zoom_to_fit(&mut self, _: &ZoomToFit, _window: &mut Window, cx: &mut Context<Self>) {
    self.rescale(Zoom::FitWidth, None, cx);
  }

  fn actual_size(&mut self, _: &ActualSize, _window: &mut Window, cx: &mut Context<Self>) {
    self.set_scale(1.0, None, cx);
  }

  fn page_down(&mut self, _: &PageDown, _window: &mut Window, cx: &mut Context<Self>) {
    let next = self.current_page().unwrap_or(0).saturating_add(1);
    self.scroll_to_page(next.min(self.page_count().unwrap_or(1).saturating_sub(1)), cx);
  }

  fn page_up(&mut self, _: &PageUp, _window: &mut Window, cx: &mut Context<Self>) {
    let previous = self.current_page().unwrap_or(0).saturating_sub(1);
    self.scroll_to_page(previous, cx);
  }

  fn first_page(&mut self, _: &FirstPage, _window: &mut Window, cx: &mut Context<Self>) {
    self.scroll_to_page(0, cx);
  }

  fn last_page(&mut self, _: &LastPage, _window: &mut Window, cx: &mut Context<Self>) {
    self.scroll_to_page(self.page_count().unwrap_or(1).saturating_sub(1), cx);
  }

  fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
    let delta = match event.delta {
      ScrollDelta::Pixels(point) => point,
      ScrollDelta::Lines(point) => Point {
        x: px(point.x * 20.),
        y: px(point.y * 20.),
      },
    };
    if event.modifiers.secondary() {
      let step = 1. + f32::from(delta.y) / 200.;
      self.set_scale(self.scale() * step.clamp(0.5, 2.0), Some(event.position), cx);
      return;
    }
    self.set_scroll(self.scroll_y - delta.y, cx);
  }

  fn on_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
    self.set_scale(self.scale() * (1. + event.delta), Some(event.position), cx);
  }

  /// Render the visible pages, and one page before and after them.
  fn schedule_renders(&mut self, window: &Window, cx: &Context<Self>) {
    let Some(document) = self.document().map(Arc::clone) else {
      return;
    };
    let Some((layout, viewport)) = self.layout() else {
      return;
    };
    let visible = visible_pages(&layout, self.scroll_y, viewport.size.height);
    let first = visible.start.saturating_sub(PREFETCH);
    let last = visible.end.saturating_add(PREFETCH).min(document.page_count());
    let device_scale = window.scale_factor();
    let scale = self.scale() * device_scale;
    let key = scale_key(scale);
    if self.render_scale_key != Some(key) {
      self.cache.retain_scales(key, self.render_scale_key);
      self.render_tasks.retain(|(_, task_key), _| *task_key == key);
      self.render_scale_key = Some(key);
    }
    let generation = self.generation;
    // Visible pages are queued before the prefetch ones, so they land first.
    let order = visible.clone().chain((first..last).filter(|page| !visible.contains(page)));
    for page in order {
      if self.cache.has(page, key) || self.render_tasks.contains_key(&(page, key)) {
        continue;
      }
      if self.render_tasks.len() >= MAX_RENDER_JOBS {
        break;
      }
      let document = Arc::clone(&document);
      let handle = self.window_handle;
      let task = cx.spawn(async move |view, cx| {
        let rendered = cx.background_spawn(async move { render_page(&document, page, scale) }).await;
        let applied = view.update(cx, |view, _| {
          view.render_tasks.remove(&(page, key));
          if view.generation != generation {
            return false;
          }
          match rendered {
            Ok(bitmap) => {
              let bytes = bitmap.rgba.len();
              if let Some(image) = to_render_image(bitmap) {
                view.cache.insert(page, key, image, bytes);
                true
              } else {
                tracing::warn!(page, "a rendered page could not be packed for painting");
                false
              }
            },
            Err(error) => {
              tracing::warn!(%error, page, "rendering a page failed");
              false
            },
          }
        });
        // A background result does not schedule a frame by itself.
        if applied.unwrap_or(false)
          && let Err(error) = cx.update_window(handle, |_, window, _| window.refresh())
        {
          tracing::debug!(%error, "the window closed before a page could be painted");
        }
      });
      self.render_tasks.insert((page, key), task);
    }
  }

  fn render_title_row(&self, cx: &Context<Self>) -> impl IntoElement {
    let theme = cx.theme();
    let title = file_name(
      self.window_title(cx),
      self
        .markdown
        .as_ref()
        .filter(|_| self.shows_markdown())
        .is_some_and(|document| document.read(cx).is_dirty()),
      cx,
      cx.listener(|view, _, window, cx| view.open_nearby_picker(&GoToFile, window, cx)),
    );
    let actions = div()
      .flex()
      .items_center()
      .gap_2()
      // Clicks on these belong to the buttons: without this the title bar sees
      // them and macOS zooms the window on the second one.
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .when(self.shows_markdown(), |actions| {
        actions.child(toolbar_button(
          "pdf-pages",
          Icon::empty().path("icons/presentation.svg"),
          "PDF pages (Cmd+Shift+P)",
          cx,
          cx.listener(|view, _, window, cx| view.show_pdf(&PdfPages, window, cx)),
        ))
      })
      .when(self.document().is_some(), |actions| {
        let (icon, tip) = self.markdown_button(cx);
        actions.child(toolbar_button(
          "markdown-views",
          icon,
          tip,
          cx,
          cx.listener(|view, _, window, cx| view.show_markdown(&ConvertToMarkdown, window, cx)),
        ))
      });
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

  fn render_status_bar(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
    if self.shows_markdown() {
      // The Markdown document draws its own status bar.
      return None;
    }
    let theme = cx.theme();
    Some(
      div()
      // Its own row under the pages, so the last line of a page is never
      // covered.
      .flex_shrink_0()
      .flex()
      .items_center()
      .gap_4()
      .h_6()
      .px_3()
      .bg(theme.background)
      .text_xs()
      .text_color(theme.muted_foreground)
      .when_some(self.status_text(), |bar, text| {
        bar.child(
          div()
            .id("pdf-status-page")
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme.muted))
            .child(text)
            .on_click(cx.listener(|view, _, window, cx| view.open_go_to_page(&GoToPage, window, cx))),
        )
      })
      .when(matches!(self.load, Load::Opening), |bar| bar.child("Opening…"))
      .when(self.is_indexing(), |bar| bar.child("Indexing…"))
      .when_some(self.convert.as_ref().map(|job| job.progress), |bar, progress| {
        bar.child(match progress {
          None => "Converting…".to_owned(),
          Some((done, total)) => format!("Converting… page {done} of {total}"),
        })
      })
      .when_some(self.notice().map(str::to_owned), gpui_kit::ParentElement::child)
      .when_some(self.text_failure().map(str::to_owned), |bar, reason| {
        bar.child(div().text_color(theme.warning).child(format!("No text layer: {reason}")))
      })
      .when_some(self.failure().map(str::to_owned), |bar, reason| {
        bar.child(div().text_color(theme.danger).child(reason))
      })
      .child(div().flex_1())
      .children(self.zoom_percent().map(|percent| div().child(format!("{percent}%"))))
      .child(div().child("PDF")),
    )
  }

  /// What stands in for the pages when there are none to show.
  fn blocking_message(&self) -> Option<String> {
    self
      .failure()
      .map(|reason| format!("{}: {reason}", self.title()))
      .or_else(|| self.is_locked().then(|| format!("{} is password protected", self.title())))
  }

  fn render_body(&mut self, window: &Window, cx: &Context<Self>) -> AnyElement {
    if let Some(document) = self.markdown.clone().filter(|_| self.shows_markdown()) {
      return div().flex().flex_col().flex_1().min_h_0().child(document).into_any_element();
    }
    if let Some(message) = self.blocking_message() {
      let theme = cx.theme();
      return div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element();
    }
    self.schedule_renders(window, cx);
    let theme = cx.theme();
    // The scrollbar reads the reader's position, and a drag on its thumb writes
    // back into the same cell for the next frame.
    let scroll = self.scroll.clone();
    let laid_out = self.layout();
    if let Some((layout, viewport)) = &laid_out {
      scroll.publish(*viewport, size(viewport.size.width, layout.total_height));
    }
    let visible = laid_out.as_ref().map_or(0..0, |(layout, viewport)| {
      visible_pages(layout, self.scroll_y, viewport.size.height)
    });
    let (match_overlays, current_overlays, selection_overlays) = self.overlays_for(visible);
    let (background, paper, shadow) = (theme.background, gpui_kit::white(), theme.foreground.opacity(0.18));
    let viewport = Rc::clone(&self.viewport);
    let plan = PlanSource {
      pages: self.document().map(|document| document.pages().to_vec()).unwrap_or_default(),
      layout: laid_out.map(|(layout, _)| layout),
      scale: self.scale(),
      resolves_fit: matches!(self.zoom, Zoom::FitWidth),
      scroll_y: self.scroll_y,
      images: self.cache.snapshot(scale_key(self.scale() * window.scale_factor())),
      matches: match_overlays,
      current_match: current_overlays,
      selection: selection_overlays,
    };
    let overlay_colors = OverlayColors {
      selection: theme.accent.opacity(0.4),
      match_found: theme.warning.opacity(0.35),
      current_match: theme.warning.opacity(0.6),
    };
    div()
      .id("pdf-surface")
      .relative()
      .flex_1()
      .min_h_0()
      .overflow_hidden()
      .on_scroll_wheel(cx.listener(Self::on_scroll))
      .on_pinch(cx.listener(Self::on_pinch))
      .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
      .on_mouse_move(cx.listener(Self::on_mouse_move))
      .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .child(
        canvas(
          move |bounds, _window, _cx| {
            let changed = viewport.replace(Some(bounds)) != Some(bounds);
            (plan.paint_plan(bounds), changed)
          },
          move |bounds, (plan, viewport_changed): (Vec<PagePaint>, bool), window, _cx| {
            if viewport_changed {
              window.refresh();
            }
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
              window.paint_quad(gpui_kit::fill(bounds, background));
              for page in plan {
                paint_page(window, &page, paper, shadow, &overlay_colors);
              }
            });
          },
        )
        .absolute()
        .inset_0(),
      )
      .child(
        div()
          .absolute()
          .inset_0()
          .child(Scrollbar::vertical(&scroll).id("pdf-scrollbar").viewport_from_layout()),
      )
      .into_any_element()
  }

  /// A window close from the platform runs through the same path.
  fn install_close_guard(window: &Window, cx: &Context<Self>) {
    // Weak: the platform window outlives the close in gpui-pre, and a strong entity here would keep the document alive with it.
    let entity = cx.entity().downgrade();
    window.on_window_should_close(cx, move |window, cx| {
      entity
        .update(cx, |view, cx| {
          let quitting = cx.try_global::<crate::QuitCommitted>().is_some_and(|quit| quit.0);
          if view.closing && (quitting || view.close_decided) {
            true
          } else {
            view.request_close(window, cx);
            false
          }
        })
        .unwrap_or(true)
    });
  }

  /// A PDF holds nothing unsaved, so closing never asks.
  fn request_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
    self.closing = true;
    self.close_decided = true;
    self.render_tasks.clear();
    self.cache.clear();
    let handle = self.window_handle;
    let cleanup = cx.spawn(async move |_, cx| {
      let _ = cx.update_window(handle, |_, window, _| window.remove_window());
    });
    cx.update_default_global::<PendingCleanups, _>(|pending, _| pending.0.push(cleanup));
  }

  fn close(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
    self.request_close(window, cx);
  }

  /// Watch the source file so the window follows edits made elsewhere.
  fn start_watch(&mut self, window: &Window, cx: &Context<Self>) {
    let (sender, receiver) = async_channel::bounded::<()>(1);
    #[cfg(test)]
    {
      self.watch_sender = Some(sender);
    }
    #[cfg(not(test))]
    {
      match FileWatch::new(&self.path, move || {
        let _ = sender.try_send(());
      }) {
        Ok(watch) => self.watch = Some(watch),
        Err(error) => {
          tracing::warn!(%error, "file watch unavailable");
          return;
        },
      }
    }
    self.watch_task = Some(cx.spawn_in(window, async move |view, cx| {
      while cx
        .background_spawn({
          let receiver = receiver.clone();
          async move { receiver.recv().await }
        })
        .await
        .is_ok()
      {
        cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
        while receiver.try_recv().is_ok() {}
        if view.update_in(cx, Self::on_disk_change).is_err() {
          break;
        }
      }
    }));
  }

  /// Reload after the file changed underneath the window.
  fn on_disk_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.closing {
      return;
    }
    let path = self.path.clone();
    self.reload_task = Some(cx.spawn_in(window, async move |view, cx| {
      let result = cx.background_spawn(async move { load_pdf(&path) }).await;
      let _ = view.update_in(cx, |view, window, cx| {
        match result {
          Ok(loaded) => {
            if view.closing {
              return;
            }
            view.bytes = loaded.bytes;
            view.disk = loaded.disk;
            view.generation = view.generation.saturating_add(1);
            view.cache.clear();
            view.render_tasks.clear();
            view.render_scale_key = None;
            view.layout_cache.replace(None);
            view.overlay_cache = None;
            view.load = Load::Opening;
            view.start_open(window, cx);
          },
          Err(error) => tracing::warn!(%error, "reload after an external change failed"),
        }
        cx.notify();
      });
    }));
  }

  /// Drive the watch pump from a test without a platform watcher.
  #[cfg(test)]
  pub(crate) fn notify_disk_change_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    Self::on_disk_change(self, window, cx);
  }

  // Quit gate. A PDF window has nothing to flush and never refuses to close.

  /// Start closing; returns whether the window was already closing.
  pub const fn begin_quit(&mut self) -> bool {
    let already = self.closing;
    self.closing = true;
    already
  }

  /// Undo `begin_quit` when the gate refuses to quit.
  pub const fn abort_quit(&mut self) {
    self.closing = false;
    self.close_decided = false;
  }

  /// Whether the close decision is already durable.
  pub const fn close_decided(&self) -> bool {
    self.close_decided
  }

  /// A PDF window never waits on a save to close.
  pub const fn close_after_save() -> bool {
    false
  }

  /// Nothing to write: a PDF window keeps no unsaved work.
  pub fn flush_checkpoint() -> Task<Result<(), String>> {
    Task::ready(Ok(()))
  }

  /// Rendering is discardable, so the window is always quiet enough to close.
  pub fn chain_drained(&mut self) -> Task<()> {
    self.render_tasks.clear();
    Task::ready(())
  }
}

impl Render for PdfView {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.follow_scrollbar(cx);
    let body = self.render_body(window, cx);
    let theme = cx.theme();
    div()
      .key_context("PdfView")
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::zoom_in))
      .on_action(cx.listener(Self::zoom_out))
      .on_action(cx.listener(Self::zoom_to_fit))
      .on_action(cx.listener(Self::actual_size))
      .on_action(cx.listener(Self::page_down))
      .on_action(cx.listener(Self::page_up))
      .on_action(cx.listener(Self::first_page))
      .on_action(cx.listener(Self::last_page))
      .on_action(cx.listener(Self::open_go_to_page))
      .on_action(cx.listener(Self::open_find))
      .on_action(cx.listener(Self::next_match))
      .on_action(cx.listener(Self::previous_match))
      .on_action(cx.listener(Self::select_all))
      .on_action(cx.listener(Self::copy))
      .on_action(cx.listener(Self::show_markdown))
      .on_action(cx.listener(Self::show_pdf))
      .on_action(cx.listener(Self::open_theme_picker))
      .on_action(cx.listener(Self::open_ui_font_picker))
      .on_action(cx.listener(Self::open_code_font_picker))
      .on_action(cx.listener(Self::open_nearby_picker))
      .on_action(cx.listener(Self::close))
      .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| apply_external_paths(paths, cx)))
      .drag_over::<ExternalPaths>(|style, _, _, cx| external_paths_ring(style, cx))
      .relative()
      .flex()
      .flex_col()
      .size_full()
      .bg(theme.background)
      .font_family(theme.font_family.clone())
      .text_color(theme.foreground)
      .child(self.render_title_row(cx))
      .child(
        div()
          .relative()
          .flex()
          .flex_col()
          .flex_1()
          .min_h_0()
          .child(body)
          .children(self.find.clone().map(IntoElement::into_any_element)),
      )
      .children(self.render_status_bar(cx))
      .children(self.prompt.as_ref().map(Prompt::element))
  }
}

/// Rendered pages, evicted least-recently used under a byte budget.
#[derive(Default)]
struct PageCache {
  entries: HashMap<(usize, u32), CachedPage>,
  order: VecDeque<(usize, u32)>,
  bytes: usize,
}

struct CachedPage {
  image: Arc<gpui_kit::RenderImage>,
  bytes: usize,
}

impl PageCache {
  fn has(&self, page: usize, key: u32) -> bool {
    self.entries.contains_key(&(page, key))
  }

  fn touch(&mut self, page: usize, key: u32) {
    let entry = (page, key);
    if let Some(index) = self.order.iter().position(|item| *item == entry) {
      self.order.remove(index);
      self.order.push_back(entry);
    }
  }

  fn insert(&mut self, page: usize, key: u32, image: Arc<gpui_kit::RenderImage>, bytes: usize) {
    if let Some(previous) = self.entries.insert((page, key), CachedPage { image, bytes }) {
      self.bytes = self.bytes.saturating_sub(previous.bytes);
      self.order.retain(|entry| *entry != (page, key));
    }
    self.order.push_back((page, key));
    self.bytes = self.bytes.saturating_add(bytes);
    while self.bytes > MAX_CACHE_BYTES && self.order.len() > 1 {
      let Some(oldest) = self.order.pop_front() else {
        break;
      };
      if let Some(entry) = self.entries.remove(&oldest) {
        self.bytes = self.bytes.saturating_sub(entry.bytes);
      }
    }
  }

  /// Keep only the current scale and the previous one used as a placeholder.
  fn retain_scales(&mut self, current: u32, previous: Option<u32>) {
    self.order.retain(|entry| {
      let keep = entry.1 == current || previous == Some(entry.1);
      if !keep && let Some(cached) = self.entries.remove(entry) {
        self.bytes = self.bytes.saturating_sub(cached.bytes);
      }
      keep
    });
  }

  /// The image for every cached page: the one rendered at `key` when it exists,
  /// otherwise any other scale, which the painter stretches until the right one
  /// arrives.
  fn snapshot(&mut self, key: u32) -> HashMap<usize, Arc<gpui_kit::RenderImage>> {
    let mut best: HashMap<usize, (bool, u32, Arc<gpui_kit::RenderImage>)> = HashMap::new();
    for ((page, entry_key), entry) in &self.entries {
      let exact = *entry_key == key;
      let replace = best.get(page).is_none_or(|(was_exact, _, _)| exact && !was_exact);
      if replace {
        best.insert(*page, (exact, *entry_key, Arc::clone(&entry.image)));
      }
    }
    let out: HashMap<usize, Arc<gpui_kit::RenderImage>> =
      best.iter().map(|(page, (_, _, image))| (*page, Arc::clone(image))).collect();
    for (page, (_, entry_key, _)) in best {
      self.touch(page, entry_key);
    }
    out
  }

  fn clear(&mut self) {
    self.entries.clear();
    self.order.clear();
    self.bytes = 0;
  }
}

/// A scale rounded to a cache key, so float noise cannot miss the cache.
fn scale_key(scale: f32) -> u32 {
  round_to_u32(scale * 1000.)
}

/// Where every page sits, in window pixels relative to the top of the document.
#[derive(Clone)]
pub(crate) struct Layout {
  pub(crate) pages: Vec<Bounds<Pixels>>,
  pub(crate) total_height: Pixels,
}

/// A stacked layout kept until page count, scale, or viewport width change.
struct CachedLayout {
  page_count: usize,
  scale_key: u32,
  viewport_width: u32,
  layout: Layout,
}

/// Overlay boxes kept until matches, scale, or the visible page range change.
struct OverlayCache {
  matches: Arc<[Match]>,
  current: Option<usize>,
  selection: Option<(TextPos, TextPos)>,
  scale_key: u32,
  visible: Range<usize>,
  match_overlays: Overlays,
  current_overlays: Overlays,
  selection_overlays: Overlays,
}

/// Stack the pages with a gap above, below, and between, each centered.
pub(crate) fn layout(pages: &[PageGeometry], scale: f32, viewport_width: Pixels) -> Layout {
  let gap = px(PAGE_GAP);
  let mut widest = px(0.);
  let mut sizes = Vec::with_capacity(pages.len());
  for page in pages {
    let (width, height) = page.display_size();
    let drawn = size(px(width * scale), px(height * scale));
    widest = widest.max(drawn.width);
    sizes.push(drawn);
  }
  let content_width = viewport_width.max(widest + gap + gap);
  let mut y = gap;
  let mut bounds = Vec::with_capacity(sizes.len());
  for drawn in sizes {
    bounds.push(Bounds {
      origin: Point { x: (content_width - drawn.width) / 2., y },
      size: drawn,
    });
    y += drawn.height + gap;
  }
  Layout { pages: bounds, total_height: y }
}

/// The scale at which the widest page fills the window.
pub(crate) fn fit_width_scale(pages: &[PageGeometry], viewport_width: Pixels) -> f32 {
  let widest = pages.iter().map(|page| page.display_size().0).fold(0.0_f32, f32::max).max(1.0);
  let available = 2.0_f32.mul_add(-PAGE_GAP, f32::from(viewport_width));
  (available / widest).clamp(MIN_SCALE, MAX_SCALE)
}

/// The pages touching the window.
pub(crate) fn visible_pages(layout: &Layout, scroll_y: Pixels, viewport_height: Pixels) -> Range<usize> {
  let top = scroll_y;
  let bottom = scroll_y + viewport_height;
  let mut first = None;
  let mut last = 0;
  for (index, bounds) in layout.pages.iter().enumerate() {
    let page_top = bounds.origin.y;
    let page_bottom = bounds.origin.y + bounds.size.height;
    if page_bottom < top || page_top > bottom {
      continue;
    }
    if first.is_none() {
      first = Some(index);
    }
    last = index;
  }
  let first = first.unwrap_or(0);
  first..last.saturating_add(1)
}

/// The page covering the middle of the window.
pub(crate) fn current_page(layout: &Layout, scroll_y: Pixels, viewport_height: Pixels) -> usize {
  let middle = scroll_y + viewport_height / 2.;
  let mut best = 0;
  let mut best_distance = Pixels::MAX;
  for (index, bounds) in layout.pages.iter().enumerate() {
    let top = bounds.origin.y;
    let bottom = top + bounds.size.height;
    let distance = if middle < top {
      top - middle
    } else if middle > bottom {
      middle - bottom
    } else {
      px(0.)
    };
    if distance < best_distance {
      best_distance = distance;
      best = index;
    }
  }
  best
}

/// Boxes to paint over one page, in displayed points.
type Overlays = HashMap<usize, Vec<DisplayRect>>;

/// The view state one painted frame reads, captured before layout.
struct PlanSource {
  pages: Vec<PageGeometry>,
  layout: Option<Layout>,
  scale: f32,
  resolves_fit: bool,
  scroll_y: Pixels,
  images: HashMap<usize, Arc<gpui_kit::RenderImage>>,
  matches: Overlays,
  current_match: Overlays,
  selection: Overlays,
}

impl PlanSource {
  fn paint_plan(&self, viewport: Bounds<Pixels>) -> Vec<PagePaint> {
    if self.pages.is_empty() {
      return Vec::new();
    }
    let scale = if self.resolves_fit {
      fit_width_scale(&self.pages, viewport.size.width)
    } else {
      self.scale
    };
    let laid = if let Some(cached) = &self.layout
      && (scale - self.scale).abs() < f32::EPSILON
    {
      cached.clone()
    } else {
      layout(&self.pages, scale, viewport.size.width)
    };
    let visible = visible_pages(&laid, self.scroll_y, viewport.size.height);
    let mut plan = Vec::new();
    for page in visible {
      let Some(bounds) = laid.pages.get(page) else {
        continue;
      };
      let origin = Point {
        x: viewport.origin.x + bounds.origin.x,
        y: viewport.origin.y + bounds.origin.y - self.scroll_y,
      };
      let overlay = |source: &Overlays| {
        source
          .get(&page)
          .map(|rects| rects.iter().map(|rect| place(*rect, origin, scale)).collect())
          .unwrap_or_default()
      };
      plan.push(PagePaint {
        bounds: Bounds { origin, size: bounds.size },
        image: self.images.get(&page).map(Arc::clone),
        matches: overlay(&self.matches),
        current_match: overlay(&self.current_match),
        selection: overlay(&self.selection),
      });
    }
    plan
  }
}

/// What the overlay boxes are painted in.
#[derive(Clone, Copy)]
struct OverlayColors {
  selection: gpui_kit::Hsla,
  match_found: gpui_kit::Hsla,
  current_match: gpui_kit::Hsla,
}

/// Paint one page: its shadow, its paper, its pixels, and the boxes over them.
fn paint_page(
  window: &mut Window,
  page: &PagePaint,
  paper: gpui_kit::Hsla,
  shadow: gpui_kit::Hsla,
  colors: &OverlayColors,
) {
  let shadow_bounds = Bounds {
    origin: Point {
      x: page.bounds.origin.x + px(2.),
      y: page.bounds.origin.y + px(2.),
    },
    size: page.bounds.size,
  };
  window.paint_quad(gpui_kit::fill(shadow_bounds, shadow));
  window.paint_quad(gpui_kit::fill(page.bounds, paper));
  if let Some(image) = page.image.clone()
    && let Err(error) = window.paint_image(page.bounds, page.bounds, Corners::default(), image, 0, false)
  {
    tracing::error!(%error, "painting a page failed");
  }
  for bounds in &page.selection {
    window.paint_quad(gpui_kit::fill(*bounds, colors.selection));
  }
  for bounds in &page.matches {
    window.paint_quad(gpui_kit::fill(*bounds, colors.match_found));
  }
  for bounds in &page.current_match {
    window.paint_quad(gpui_kit::fill(*bounds, colors.current_match));
  }
}

/// One page's place on screen, its bitmap, and the boxes over it.
struct PagePaint {
  bounds: Bounds<Pixels>,
  image: Option<Arc<gpui_kit::RenderImage>>,
  matches: Vec<Bounds<Pixels>>,
  current_match: Vec<Bounds<Pixels>>,
  selection: Vec<Bounds<Pixels>>,
}

/// A displayed rectangle as window bounds on a page drawn at `origin`.
fn place(rect: DisplayRect, origin: Point<Pixels>, scale: f32) -> Bounds<Pixels> {
  Bounds {
    origin: Point {
      x: origin.x + px(rect.x * scale),
      y: origin.y + px(rect.y * scale),
    },
    size: size(px(rect.width * scale), px(rect.height * scale)),
  }
}

/// Pack a rendered page for painting: BGRA, edge-capped, one frame.
fn to_render_image(bitmap: openit_core::pdf::PageBitmap) -> Option<Arc<gpui_kit::RenderImage>> {
  let mut rgba = bitmap.rgba;
  for pixel in rgba.as_chunks_mut::<4>().0 {
    pixel.swap(0, 2);
  }
  let buffer = image::RgbaImage::from_raw(bitmap.width, bitmap.height, rgba)?;
  let frame = image::Frame::new(buffer);
  Some(image_decode::to_render_image(vec![frame]))
}

/// Where a generated document landed.
struct Written {
  /// The Markdown file.
  path: PathBuf,
  /// The file as the document reader sees it.
  loaded: Loaded,
  /// Whether the folder beside the PDF refused the write.
  in_temp_folder: bool,
  /// How many pages produced no text.
  skipped_pages: usize,
}

/// Write the Markdown and its figures beside the PDF, falling back to the
/// temporary folder when that directory refuses.
fn write_markdown(folder: &Path, stem: &str, conversion: &Conversion) -> Result<Written, openit_core::Error> {
  match write_into(folder, stem, conversion) {
    Ok(written) => Ok(written),
    Err(error) => {
      tracing::warn!(%error, folder = %folder.display(), "writing the Markdown beside the document failed");
      let temp = std::env::temp_dir().join("openit");
      let mut written = write_into(&temp, stem, conversion)?;
      written.in_temp_folder = true;
      Ok(written)
    },
  }
}

/// Write one generation into `folder`, choosing a free name.
fn write_into(folder: &Path, stem: &str, conversion: &Conversion) -> Result<Written, openit_core::Error> {
  std::fs::create_dir_all(folder).map_err(|source| openit_core::Error::Write { path: folder.to_path_buf(), source })?;
  let (path, stem) = free_name(folder, stem);
  let images = folder.join(openit_core::pdf_markdown::images_dir_name(&stem));
  if !conversion.figures.is_empty() {
    std::fs::create_dir_all(&images).map_err(|source| openit_core::Error::Write { path: images.clone(), source })?;
  }
  for figure in &conversion.figures {
    // The figure links are relative to the stem the conversion used, which the
    // free name may have changed.
    let name = figure.relative_path.rsplit('/').next().unwrap_or(figure.relative_path.as_str());
    save_bytes(&images.join(name), Revision::INITIAL.next(), &figure.png)?;
  }
  let markdown = retarget_figures(&conversion.markdown, &stem);
  save_bytes(&path, Revision::INITIAL.next(), markdown.as_bytes())?;
  let loaded = load_text(&path)?;
  Ok(Written {
    path,
    loaded,
    in_temp_folder: false,
    skipped_pages: conversion.skipped_pages.len(),
  })
}

/// `<stem>.md` in `folder`, numbered when a file OpenIt did not just write is
/// already there.
fn free_name(folder: &Path, stem: &str) -> (PathBuf, String) {
  let first = folder.join(format!("{stem}.md"));
  if !first.exists() {
    return (first, stem.to_owned());
  }
  for attempt in 2..100_u32 {
    let stem = format!("{stem}-{attempt}");
    let path = folder.join(format!("{stem}.md"));
    if !path.exists() {
      return (path, stem);
    }
  }
  (first, stem.to_owned())
}

/// Point the figure links at the folder the file actually got.
fn retarget_figures(markdown: &str, stem: &str) -> String {
  let folder = openit_core::pdf_markdown::images_dir_name(stem);
  let mut out = String::with_capacity(markdown.len());
  for (index, piece) in markdown.split("](").enumerate() {
    if index > 0 {
      out.push_str("](");
      if let Some((link, rest)) = piece.split_once(')')
        && let Some((_, file)) = link.rsplit_once('/')
      {
        out.push_str(&folder);
        out.push('/');
        out.push_str(file);
        out.push(')');
        out.push_str(rest);
        continue;
      }
    }
    out.push_str(piece);
  }
  out
}

/// Keep the status bar's conversion progress current while a job runs.
fn report_progress(
  view: gpui_kit::WeakEntity<PdfView>,
  receiver: async_channel::Receiver<(u32, u32)>,
  cx: &gpui_kit::AsyncWindowContext,
) -> Task<()> {
  cx.spawn(async move |cx| {
    while let Ok(progress) = receiver.recv().await {
      let updated = view.update(cx, |view, cx| {
        if let Some(job) = view.convert.as_mut() {
          job.progress = Some(progress);
          cx.notify();
        }
      });
      if updated.is_err() {
        break;
      }
    }
  })
}

/// A scale as a whole percentage.
fn percent(scale: f32) -> u32 {
  round_to_u32(scale * 100.)
}

/// Round a finite, non-negative `f32` to the nearest `u32`, saturating.
#[expect(
  clippy::as_conversions,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "no checked float-to-integer conversion exists; the value is clamped into range first"
)]
fn round_to_u32(value: f32) -> u32 {
  if !value.is_finite() || value <= 0. {
    return 0;
  }
  value.round().min(f32::from(u16::MAX)) as u32
}

#[cfg(test)]
mod tests {
  use gpui_kit::{KeyBinding, TestAppContext, VisualTestContext, px};
  use openit_core::document::load_pdf;
  use openit_core::pdf::PageGeometry;
  use openit_core::pdf::test_support::tiny_pdf_pages;

  use super::{MAX_CACHE_BYTES, PAGE_GAP, PageCache, PdfView, current_page, fit_width_scale, layout, visible_pages};
  use crate::document_view::tests::install_globals;

  fn write_pdf(dir: &std::path::Path, name: &str, pages: usize) -> std::path::PathBuf {
    let texts: Vec<String> = (1..=pages).map(|page| format!("Page {page} text")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let path = dir.join(name);
    std::fs::write(&path, tiny_pdf_pages(&refs)).unwrap();
    path
  }

  fn open(cx: &mut TestAppContext, path: std::path::PathBuf) -> (gpui_kit::Entity<PdfView>, &mut VisualTestContext) {
    let loaded = load_pdf(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| PdfView::open(path, loaded, window, cx));
    cx.run_until_parked();
    (view, cx)
  }

  #[gpui_kit::test]
  fn a_pdf_opens_and_reports_its_pages(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "a.pdf", 3);

    let (view, cx) = open(cx, path);

    assert_eq!(view.read_with(cx, |view, _| view.page_count()), Some(3));
    assert_eq!(view.read_with(cx, |view, _| view.title()), "a.pdf");
    assert_eq!(view.read_with(cx, |view, _| view.status_text()), Some("Page 1 of 3".to_owned()));
    assert_eq!(view.read_with(cx, |view, _| view.failure().map(str::to_owned)), None);
    assert!(view.read_with(cx, |view, _| view.zoom_percent().is_some()));
  }

  #[gpui_kit::test]
  fn scrolling_to_a_page_moves_the_status_readout(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "a.pdf", 4);

    let (view, cx) = open(cx, path);
    view.update(cx, |view, cx| view.scroll_to_page(2, cx));
    cx.run_until_parked();

    assert_eq!(view.read_with(cx, |view, _| view.status_text()), Some("Page 3 of 4".to_owned()));

    view.update(cx, |view, cx| view.scroll_to_page(0, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.status_text()), Some("Page 1 of 4".to_owned()));
  }

  #[gpui_kit::test]
  fn zooming_keeps_the_page_in_view(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "a.pdf", 12);

    let (view, cx) = open(cx, path);
    view.update(cx, |view, cx| view.scroll_to_page(8, cx));
    cx.run_until_parked();
    let before = view.read_with(cx, |view, _| view.status_text());

    view.update_in(cx, |view, window, cx| view.actual_size(&crate::actions::ActualSize, window, cx));
    cx.run_until_parked();
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()),
      before,
      "actual size keeps the page"
    );

    view.update_in(cx, |view, window, cx| view.zoom_to_fit(&crate::actions::ZoomToFit, window, cx));
    cx.run_until_parked();
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()),
      before,
      "fit width keeps the page"
    );

    view.update_in(cx, |view, window, cx| view.zoom_in(&crate::actions::ZoomIn, window, cx));
    cx.run_until_parked();
    assert_eq!(
      view.read_with(cx, |view, _| view.status_text()),
      before,
      "zooming in keeps the page"
    );
  }

  #[gpui_kit::test]
  fn a_protected_document_asks_for_its_password_then_opens(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.pdf");
    let source = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/fixtures/protected.pdf"));
    std::fs::copy(&source, &path).unwrap();

    let (view, cx) = open(cx, path);

    assert!(
      view.read_with(cx, |view, _| view.is_locked()),
      "a protected document waits for a password"
    );
    assert_eq!(view.read_with(cx, |view, _| view.page_count()), None);

    view.update_in(cx, |view, window, cx| view.submit_password("nope".to_owned(), window, cx));
    cx.run_until_parked();
    assert!(
      view.read_with(cx, |view, _| view.is_locked()),
      "a wrong password keeps the prompt"
    );

    view.update_in(cx, |view, window, cx| view.submit_password("openit".to_owned(), window, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.page_count()), Some(1));
    assert!(!view.read_with(cx, |view, _| view.is_locked()));
  }

  #[gpui_kit::test]
  fn a_replaced_file_reloads_the_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "a.pdf", 1);

    let (view, cx) = open(cx, path.clone());
    assert_eq!(view.read_with(cx, |view, _| view.page_count()), Some(1));

    std::fs::write(&path, tiny_pdf_pages(&["one", "two", "three"])).unwrap();
    view.update_in(cx, super::PdfView::notify_disk_change_for_test);
    cx.run_until_parked();

    assert_eq!(view.read_with(cx, |view, _| view.page_count()), Some(3));
  }

  #[gpui_kit::test]
  fn a_broken_document_explains_itself(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.pdf");
    std::fs::write(&path, b"%PDF-1.7 not really").unwrap();

    let (view, cx) = open(cx, path);

    assert_eq!(
      view.read_with(cx, |view, _| view.failure().map(str::to_owned)),
      Some("This PDF is malformed".to_owned())
    );
    assert_eq!(view.read_with(cx, |view, _| view.page_count()), None);
  }

  #[gpui_kit::test]
  fn find_matches_across_pages_and_steps_through_them(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("words.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["alpha beta", "gamma alpha"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| view.open_find(&crate::actions::Find, window, cx));
    view.update_in(cx, |view, window, cx| view.run_search("alpha".to_owned(), window, cx));
    cx.run_until_parked();

    assert_eq!(view.read_with(cx, |view, _| view.matches().len()), 2);
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), Some(0));
    assert_eq!(view.read_with(cx, |view, _| view.matches()[0].start.page), 0);
    assert_eq!(view.read_with(cx, |view, _| view.matches()[1].start.page), 1);

    view.update_in(cx, |view, window, cx| view.next_match(&crate::actions::NextMatch, window, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), Some(1));
    assert_eq!(view.read_with(cx, |view, _| view.status_text()), Some("Page 2 of 2".to_owned()));

    // Stepping past the end wraps to the first match.
    view.update_in(cx, |view, window, cx| view.next_match(&crate::actions::NextMatch, window, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), Some(0));

    view.update_in(cx, |view, window, cx| {
      view.previous_match(&crate::actions::PreviousMatch, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), Some(1));
  }

  #[gpui_kit::test]
  fn a_query_with_no_match_clears_the_current_match(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("words.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["alpha beta"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| view.run_search("zeta".to_owned(), window, cx));
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.matches().is_empty()));
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), None);
  }

  #[gpui_kit::test]
  fn closing_the_find_bar_clears_the_matches(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("words.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["alpha beta"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| view.open_find(&crate::actions::Find, window, cx));
    view.update_in(cx, |view, window, cx| view.run_search("alpha".to_owned(), window, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.matches().len()), 1);

    view.update_in(cx, super::PdfView::close_find);
    cx.run_until_parked();

    assert!(view.read_with(cx, |view, _| view.matches().is_empty()));
    assert_eq!(view.read_with(cx, |view, _| view.current_match()), None);
  }

  #[gpui_kit::test]
  fn selecting_everything_copies_the_document_text(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("words.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["alpha beta", "gamma alpha"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| view.select_all(&crate::actions::SelectAll, window, cx));
    cx.run_until_parked();

    assert_eq!(
      view.read_with(cx, |view, _| view.selected_text()),
      Some("alpha beta\n\ngamma alpha".to_owned())
    );

    view.update_in(cx, |view, window, cx| view.copy(&crate::actions::Copy, window, cx));
    cx.run_until_parked();
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("alpha beta\n\ngamma alpha"));
  }

  #[gpui_kit::test]
  fn a_double_click_selects_one_word(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("words.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["alpha beta"])).unwrap();

    let (view, cx) = open(cx, path);
    let layer = view.read_with(cx, |view, _| view.text_layer()).expect("the text layer is read");
    let (start, _) = openit_core::pdf_text::document_range(&layer).unwrap();
    let rect = openit_core::pdf_text::char_rect(&layer, start).unwrap();

    view.update_in(cx, |view, _window, cx| {
      view.select_word_at(0, rect.x + rect.width / 2., rect.y + rect.height / 2., cx);
    });
    cx.run_until_parked();

    assert_eq!(view.read_with(cx, |view, _| view.selected_text()), Some("alpha".to_owned()));
  }

  #[gpui_kit::test]
  fn generating_writes_the_markdown_beside_the_pdf_and_shows_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["Hello World", "Second page"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();

    let written = dir.path().join("paper.md");
    let text = std::fs::read_to_string(&written).unwrap();
    assert!(text.contains("Hello World"), "{text}");
    assert!(text.contains("Second page"), "{text}");
    assert_eq!(cx.windows().len(), 1, "the Markdown lives in the PDF's own window");
    assert!(view.read_with(cx, |view, _| view.shows_markdown()));
    assert_eq!(
      view.read_with(cx, |view, _| view.notice().map(str::to_owned)),
      Some("Saved paper.md".to_owned())
    );
    let document = view.read_with(cx, |view, _| view.markdown().cloned()).expect("a document");
    assert_eq!(document.read_with(cx, |document, _| document.title()), "paper.md");
    assert!(
      !document.read_with(cx, |document, _| document.is_dirty()),
      "a written file is clean"
    );
    assert!(
      !document.read_with(cx, |document, _| document.is_editing()),
      "it opens in preview"
    );
  }

  #[gpui_kit::test]
  fn the_markdown_button_moves_between_preview_editor_and_the_pages(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["Hello World"])).unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();
    let document = view.read_with(cx, |view, _| view.markdown().cloned()).expect("a document");

    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();
    assert!(
      document.read_with(cx, |document, _| document.is_editing()),
      "the second press edits"
    );

    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();
    assert!(
      !document.read_with(cx, |document, _| document.is_editing()),
      "the third press previews"
    );

    view.update_in(cx, |view, window, cx| view.show_pdf(&crate::actions::PdfPages, window, cx));
    cx.run_until_parked();
    assert!(
      !view.read_with(cx, |view, _| view.shows_markdown()),
      "the PDF button returns to the pages"
    );
    assert_eq!(view.read_with(cx, |view, _| view.status_text()), Some("Page 1 of 1".to_owned()));

    // The document is not regenerated: the button goes straight back to it.
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.shows_markdown()));
    assert_eq!(
      std::fs::read_dir(dir.path()).unwrap().count(),
      2,
      "one PDF and one Markdown file"
    );
  }

  #[gpui_kit::test]
  fn a_name_already_taken_gets_a_number(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.pdf");
    std::fs::write(&path, tiny_pdf_pages(&["Hello World"])).unwrap();
    std::fs::write(dir.path().join("paper.md"), "# Mine\n").unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();

    assert_eq!(std::fs::read_to_string(dir.path().join("paper.md")).unwrap(), "# Mine\n");
    assert!(
      dir.path().join("paper-2.md").is_file(),
      "the generation takes the next free name"
    );
    let document = view.read_with(cx, |view, _| view.markdown().cloned()).expect("a document");
    assert_eq!(document.read_with(cx, |document, _| document.title()), "paper-2.md");
  }

  #[gpui_kit::test]
  fn figures_land_in_a_folder_beside_the_markdown(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("figured.pdf");
    std::fs::write(
      &path,
      openit_core::pdf::test_support::tiny_pdf_with_image("Caption", 0, (612.0, 792.0)),
    )
    .unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("figured.md")).unwrap();
    assert!(text.contains("![Figure](figured-images/p001-01.png)"), "{text}");
    assert!(dir.path().join("figured-images/p001-01.png").is_file());
    assert!(view.read_with(cx, |view, _| view.shows_markdown()));
  }

  #[gpui_kit::test]
  fn a_document_without_text_reports_it_in_place(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.pdf");
    std::fs::write(
      &path,
      openit_core::pdf::test_support::tiny_pdf_with_image("", 0, (612.0, 792.0)),
    )
    .unwrap();

    let (view, cx) = open(cx, path);
    view.update_in(cx, |view, window, cx| {
      view.show_markdown(&crate::actions::ConvertToMarkdown, window, cx);
    });
    cx.run_until_parked();

    assert!(!dir.path().join("scan.md").exists(), "nothing is written");
    assert!(!view.read_with(cx, |view, _| view.shows_markdown()));
    assert_eq!(
      view.read_with(cx, |view, _| view.notice().map(str::to_owned)),
      Some("This PDF has no text to convert.".to_owned())
    );
  }

  #[core::prelude::v1::test]
  fn layout_stacks_pages_with_a_gap_and_centers_them() {
    let pages = [
      PageGeometry { width: 100.0, height: 200.0, rotation: 0 },
      PageGeometry { width: 50.0, height: 100.0, rotation: 90 },
    ];

    let laid_out = layout(&pages, 2.0, px(400.));

    assert_eq!(laid_out.pages[0].origin.y, px(PAGE_GAP));
    assert_eq!(laid_out.pages[0].size, gpui_kit::size(px(200.), px(400.)));
    assert_eq!(laid_out.pages[0].origin.x, px(100.));
    assert_eq!(laid_out.pages[1].origin.y, px(PAGE_GAP + 400. + PAGE_GAP));
    assert_eq!(laid_out.pages[1].size, gpui_kit::size(px(200.), px(100.)));
    assert_eq!(laid_out.total_height, px(PAGE_GAP + 400. + PAGE_GAP + 100. + PAGE_GAP));
    assert_eq!(visible_pages(&laid_out, px(0.), px(300.)), 0..1);
    assert_eq!(visible_pages(&laid_out, px(350.), px(300.)), 0..2);
    assert_eq!(current_page(&laid_out, px(430.), px(300.)), 1);
  }

  #[core::prelude::v1::test]
  fn fit_width_uses_the_widest_displayed_page() {
    let pages = [
      PageGeometry { width: 100.0, height: 200.0, rotation: 0 },
      PageGeometry { width: 50.0, height: 300.0, rotation: 90 },
    ];

    // 632 - 2 * 16 = 600 window pixels for the widest displayed page (300 pt).
    let scale = fit_width_scale(&pages, px(632.));

    assert!((scale - 2.0).abs() < 1e-3, "{scale}");
  }

  #[core::prelude::v1::test]
  fn the_page_cache_evicts_the_oldest_entry_over_budget() {
    let mut cache = PageCache::default();
    let image = || {
      crate::image_decode::to_render_image(vec![image::Frame::new(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 255]),
      ))])
    };
    let big = MAX_CACHE_BYTES / 2 + 1;

    cache.insert(0, 1000, image(), big);
    cache.insert(1, 1000, image(), big);
    cache.insert(2, 1000, image(), big);

    assert!(!cache.has(0, 1000), "the oldest entry is evicted");
    assert!(cache.has(2, 1000));
    assert!(cache.bytes <= MAX_CACHE_BYTES + big);
  }

  #[core::prelude::v1::test]
  fn the_page_cache_evicts_in_lru_order() {
    let mut cache = PageCache::default();
    let image = || {
      crate::image_decode::to_render_image(vec![image::Frame::new(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 255]),
      ))])
    };
    let chunk = MAX_CACHE_BYTES / 3 + 1;

    cache.insert(0, 1000, image(), chunk);
    cache.insert(1, 1000, image(), chunk);
    cache.touch(0, 1000);
    cache.insert(2, 1000, image(), chunk);

    assert!(cache.has(0, 1000), "a recently touched entry is kept");
    assert!(!cache.has(1, 1000), "the least recently used entry is evicted");
    assert!(cache.has(2, 1000));
  }

  #[core::prelude::v1::test]
  fn the_cache_snapshot_prefers_the_requested_scale() {
    let mut cache = PageCache::default();
    let image = || {
      crate::image_decode::to_render_image(vec![image::Frame::new(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0, 0, 0, 255]),
      ))])
    };
    let stale = image();
    let fresh = image();
    cache.insert(3, 500, std::sync::Arc::clone(&stale), 4);
    cache.insert(3, 1000, std::sync::Arc::clone(&fresh), 4);

    let snapshot = cache.snapshot(1000);

    assert!(std::sync::Arc::ptr_eq(snapshot.get(&3).unwrap(), &fresh));
    let older = cache.snapshot(500);
    assert!(std::sync::Arc::ptr_eq(older.get(&3).unwrap(), &stale));
  }

  #[gpui_kit::test]
  fn go_to_file_opens_the_nearby_picker(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = install_globals(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-p", crate::actions::GoToFile, None)]));
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "a.pdf", 1);
    let (view, cx) = open(cx, path);

    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| matches!(view.prompt, Some(super::Prompt::Nearby(_)))));
  }
}
