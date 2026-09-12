//! OpenIt desktop application binary.

mod actions;
mod assets;
mod cache;
mod cli;
mod cli_install;
mod document_view;
mod drop;
mod empty_view;
mod export_dialog;
mod fetch;
mod font_picker;
mod handoff;
mod image_cache;
mod image_decode;
mod image_view;
mod menus;
mod nearby_picker;
mod open_urls;
mod pdf_find;
mod pdf_prompts;
mod pdf_view;
mod schema_cache;
mod schema_complete;
mod schema_validate;
mod session;
mod settings;
mod status_pickers;
mod svg;
mod theme;
mod theme_picker;
mod title_bar;
mod window;

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{
  App, AppContext, BorrowAppContext, Context, Global, KeyBinding, Subscription, Task, Window, WindowHandle,
};
use openit_core::ipc;
use openit_core::recovery::RecoveryStore;
use openit_core::settings::{Settings, ThemeSettings};

use crate::cache::ResourceCacheHandle;
use crate::document_view::DocumentView;
use crate::fetch::{Fetcher, HttpFetcher};
use crate::image_view::ImageView;
use crate::pdf_view::PdfView;
use crate::session::{PendingCleanups, Recovery};
use crate::settings::{AppSettings, SettingsStore, watch_settings};
use crate::window::{
  apply_open_request, on_reopen, open_document_window, open_empty_window, open_startup_windows, open_untitled_text,
};

struct AppQuitSubscription {
  _subscription: Subscription,
}

impl Global for AppQuitSubscription {}
struct AppSettingsSubscription {
  _subscription: Subscription,
  previous_theme: ThemeSettings,
  previous_file_generation: u64,
}

impl Global for AppSettingsSubscription {}
#[derive(Default)]
pub(crate) struct QuitInProgress(pub(crate) bool);

impl Global for QuitInProgress {}
#[derive(Default)]
pub(crate) struct QuitCommitted(pub(crate) bool);

impl Global for QuitCommitted {}

struct LastWindowQuitSubscription {
  _subscription: Subscription,
}

impl Global for LastWindowQuitSubscription {}

/// App-level last-window observer. Captures no entities: gpui-pre keeps a closed
/// window's platform state, and a strong view here would leak the document with it.
pub(crate) fn install_last_window_quit(cx: &mut App) {
  cx.set_quit_mode(gpui_kit::QuitMode::Explicit);
  cx.default_global::<QuitInProgress>();
  cx.default_global::<QuitCommitted>();
  cx.default_global::<PendingCleanups>();
  if cx.has_global::<LastWindowQuitSubscription>() {
    return;
  }
  let subscription = cx.on_window_closed(on_last_window_closed);
  cx.set_global(LastWindowQuitSubscription { _subscription: subscription });
}

fn on_last_window_closed(cx: &mut App, _: gpui_kit::WindowId) {
  if cx.try_global::<QuitInProgress>().is_some_and(|quit| quit.0) {
    return;
  }
  if !crate::handoff::should_quit_after_last_window(cx) {
    return;
  }
  request_quit(cx).detach();
}

/// One window that takes part in the quit gate.
trait QuitParticipant: 'static + gpui_kit::Render + Sized {
  /// Start closing; returns whether the window was already closing.
  fn begin_quit(&mut self) -> bool;
  /// Whether the close decision is already durable.
  fn close_decided(&self) -> bool;
  /// Whether a durable close is waiting on a save.
  fn close_after_save(&self) -> bool;
  /// Write any pending draft now.
  fn flush_checkpoint(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>>;
  /// Wait for the window's own background work.
  fn chain_drained(&mut self, cx: &mut Context<Self>) -> Task<()>;
  /// Undo `begin_quit` when the gate refuses to quit.
  fn abort_quit(&mut self, cx: &Context<Self>);
}

impl QuitParticipant for DocumentView {
  fn begin_quit(&mut self) -> bool {
    Self::begin_quit(self)
  }
  fn close_decided(&self) -> bool {
    Self::close_decided(self)
  }
  fn close_after_save(&self) -> bool {
    Self::close_after_save(self)
  }
  fn flush_checkpoint(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
    Self::flush_checkpoint(self, cx)
  }
  fn chain_drained(&mut self, cx: &mut Context<Self>) -> Task<()> {
    Self::chain_drained(self, cx)
  }
  fn abort_quit(&mut self, cx: &Context<Self>) {
    Self::abort_quit(self, cx);
  }
}

impl QuitParticipant for ImageView {
  fn begin_quit(&mut self) -> bool {
    Self::begin_quit(self)
  }
  fn close_decided(&self) -> bool {
    Self::close_decided(self)
  }
  fn close_after_save(&self) -> bool {
    Self::close_after_save()
  }
  fn flush_checkpoint(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
    Self::flush_checkpoint(self, cx)
  }
  fn chain_drained(&mut self, cx: &mut Context<Self>) -> Task<()> {
    Self::chain_drained(self, cx)
  }
  fn abort_quit(&mut self, cx: &Context<Self>) {
    let _ = cx;
    Self::abort_quit(self);
  }
}

impl QuitParticipant for PdfView {
  fn begin_quit(&mut self) -> bool {
    Self::begin_quit(self)
  }
  fn close_decided(&self) -> bool {
    Self::close_decided(self)
  }
  fn close_after_save(&self) -> bool {
    Self::close_after_save()
  }
  fn flush_checkpoint(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
    let _ = cx;
    Self::flush_checkpoint()
  }
  fn chain_drained(&mut self, cx: &mut Context<Self>) -> Task<()> {
    let _ = cx;
    Self::chain_drained(self)
  }
  fn abort_quit(&mut self, cx: &Context<Self>) {
    let _ = cx;
    Self::abort_quit(self);
  }
}

/// One window that may hold work the user has not decided to save or drop.
trait UnsavedParticipant: 'static + gpui_kit::Render + Sized {
  /// Whether quitting has to ask this window first.
  fn needs_close_decision(&self) -> bool;
  /// Ask, when needed, and resolve to whether the window is on its way out.
  fn close_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool>;
}

impl UnsavedParticipant for DocumentView {
  fn needs_close_decision(&self) -> bool {
    Self::needs_close_decision(self)
  }
  fn close_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
    Self::request_close_decision(self, window, cx)
  }
}

impl UnsavedParticipant for ImageView {
  fn needs_close_decision(&self) -> bool {
    Self::needs_close_decision(self)
  }
  fn close_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
    Self::request_close_decision(self, window, cx)
  }
}

/// One window's save-or-discard answer, asked once the earlier windows have answered.
type Decision = Box<dyn FnOnce(&mut gpui_kit::AsyncApp) -> Task<bool>>;

/// Queue a prompt for every window of type `V` with undecided work.
fn collect_decisions<V: UnsavedParticipant>(decisions: &mut Vec<Decision>, cx: &App) {
  for handle in cx.windows().into_iter().filter_map(|window| window.downcast::<V>()) {
    if !matches!(handle.read_with(cx, |view, _| view.needs_close_decision()), Ok(true)) {
      continue;
    }
    decisions.push(Box::new(move |cx| {
      handle
        .update(cx, |view, window, cx| {
          window.activate_window();
          view.close_decision(window, cx)
        })
        .unwrap_or_else(|_| Task::ready(true))
    }));
  }
}

/// A step the quit gate runs later, once the window type is no longer visible
/// in the shared task.
type QuitStep = Box<dyn FnOnce(&mut gpui_kit::AsyncApp)>;

/// What draining one window produced.
enum Drain {
  /// Wait for this task before quitting.
  Pending(Task<()>),
  /// The window is gone and had already settled its draft.
  Settled,
  /// The window is gone with work unaccounted for; quit is refused.
  Lost,
}

/// A drain step, deferred the same way.
type DrainStep = Box<dyn FnOnce(&mut gpui_kit::AsyncApp) -> Drain>;

/// One pending draft flush and whether its window already settled its close.
type FlushStep = (Task<Result<(), String>>, bool);

/// Everything the quit gate collected from the open windows.
#[derive(Default)]
struct QuitWork {
  flushes: Vec<FlushStep>,
  drains: Vec<DrainStep>,
  aborts: Vec<QuitStep>,
}

impl QuitWork {
  /// Begin quitting every window of type `V` and record its pending work.
  fn collect<V: QuitParticipant>(&mut self, cx: &mut App) {
    let handles: Vec<WindowHandle<V>> = cx.windows().into_iter().filter_map(|window| window.downcast::<V>()).collect();
    for handle in handles {
      match handle.update(cx, |view, _, cx| {
        let already_closing = view.begin_quit();
        let durable_close = view.close_decided() || view.close_after_save();
        let flush = if already_closing {
          None
        } else {
          Some(view.flush_checkpoint(cx))
        };
        (flush, durable_close)
      }) {
        Ok((flush, durable_close)) => {
          if let Some(flush) = flush {
            self.flushes.push((flush, durable_close));
            self.aborts.push(Box::new(move |cx| {
              let _ = handle.update(cx, |view, _, cx| view.abort_quit(cx));
            }));
          }
          self.drains.push(Box::new(move |cx| {
            match handle.update(cx, |view, _, cx| view.chain_drained(cx)) {
              Ok(drain) => Drain::Pending(drain),
              Err(error) if durable_close => {
                tracing::debug!(%error, "durably closed view vanished before the drain");
                Drain::Settled
              },
              Err(error) => {
                tracing::error!(%error, "could not start the operation drain; quit refused");
                Drain::Lost
              },
            }
          }));
        },
        Err(error) => {
          // A handle whose window already closed has nothing left to flush; its close path
          // persisted or discarded the draft. Refusing to quit here would strand the user.
          tracing::debug!(%error, "window vanished before the quit flush; skipping it");
        },
      }
    }
  }
}

/// Write every image edit still waiting on its debounce.
pub(crate) fn flush_pending_image_writes(cx: &mut App) -> Vec<Task<Result<(), String>>> {
  let handles: Vec<WindowHandle<ImageView>> = cx
    .windows()
    .into_iter()
    .filter_map(|window| window.downcast::<ImageView>())
    .collect();
  let mut writes = Vec::new();
  for handle in handles {
    if let Ok(Some(write)) = handle.update(cx, |view, _, cx| view.flush_pending_write(cx)) {
      writes.push(write);
    }
  }
  writes
}

/// Ask each window with unsaved work what to do, then quit. Cancelling any prompt keeps
/// the application open, and Discard drops that window's recovery draft.
pub fn request_quit(cx: &mut App) -> Task<bool> {
  let quit = cx.default_global::<QuitInProgress>();
  if quit.0 {
    return Task::ready(false);
  }
  quit.0 = true;
  cx.default_global::<QuitCommitted>().0 = false;
  // Collected inside the task: a Cmd+Q dispatched by a window arrives with that
  // window on the stack, where it can be neither read nor updated.
  cx.spawn(async move |cx| {
    let decisions = cx.update(|cx| {
      let mut decisions = Vec::new();
      collect_decisions::<DocumentView>(&mut decisions, cx);
      collect_decisions::<ImageView>(&mut decisions, cx);
      decisions
    });
    for decision in decisions {
      if !decision(cx).await {
        cx.update_global::<QuitInProgress, _>(|quit, _| quit.0 = false);
        return false;
      }
    }
    cx.update(drain_and_quit).await
  })
}

/// Flush every open document, refusing to quit on a failed draft or save.
fn drain_and_quit(cx: &mut App) -> Task<bool> {
  let mut work = QuitWork::default();
  work.collect::<DocumentView>(cx);
  work.collect::<ImageView>(cx);
  work.collect::<PdfView>(cx);
  let QuitWork { flushes, drains, aborts } = work;
  if cx.has_global::<SettingsStore>()
    && let Some(settings_write) = cx.update_global::<SettingsStore, _>(|store, _| store.take_write())
  {
    cx.update_default_global::<PendingCleanups, _>(|pending, _| pending.0.push(settings_write));
  }
  cx.spawn(async move |cx| {
    let mut failed = false;
    for (flush, durable_close) in flushes {
      if let Err(error) = flush.await
        && (error != "window closed" || !durable_close)
      {
        tracing::error!(%error, "draft flush failed; quit refused");
        failed = true;
      }
    }
    for drain in drains {
      match drain(cx) {
        Drain::Pending(task) => task.await,
        Drain::Settled => {},
        Drain::Lost => failed = true,
      }
    }
    loop {
      let cleanups = cx.update_global::<PendingCleanups, _>(|pending, _| std::mem::take(&mut pending.0));
      if cleanups.is_empty() {
        break;
      }
      for cleanup in cleanups {
        cleanup.await;
      }
    }
    if failed {
      for abort in aborts {
        abort(cx);
      }
      cx.update_global::<QuitInProgress, _>(|quit, _| quit.0 = false);
      cx.update_global::<QuitCommitted, _>(|quit, _| quit.0 = false);
    } else {
      cx.update_global::<QuitCommitted, _>(|quit, _| quit.0 = true);
      cx.update(|cx| cx.quit());
    }
    !failed
  })
}

fn recovery_dirs() -> [Option<PathBuf>; 2] {
  [RecoveryStore::default_dir(), Some(std::env::temp_dir().join("openit-drafts"))]
}

/// Flip the setting that keeps the status bar visible in Markdown preview.
fn handle_toggle_always_show_status_bar(cx: &mut App) {
  SettingsStore::update(cx, |settings| {
    settings.always_show_status_bar = !settings.always_show_status_bar;
  });
}

fn handle_set_theme_mode(action: &actions::SetThemeMode, cx: &mut App) {
  let mode = action.0;
  SettingsStore::update(cx, |settings| settings.theme.mode = mode);
  let appearance = cx.window_appearance();
  theme::apply_for_appearance(appearance, None, cx);
}

/// Select the preset width used by the Markdown preview column.
fn handle_set_markdown_preview_width(action: &actions::SetMarkdownPreviewWidth, cx: &mut App) {
  let width = action.width;
  SettingsStore::update(cx, |settings| settings.markdown_preview_width = width);
}

/// Show the file picker and open every chosen file in its own window.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI action handlers receive a mutable App"
)]
fn handle_open_file(cx: &mut App) {
  let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
    files: true,
    directories: false,
    multiple: true,
    prompt: Some("Open".into()),
  });
  cx.spawn(async move |cx| {
    let paths = match receiver.await {
      Ok(Ok(Some(paths))) => paths,
      Ok(Ok(None)) | Err(_) => return,
      Ok(Err(error)) => {
        tracing::error!(%error, "file picker failed");
        return;
      },
    };
    cx.update(|cx| {
      apply_open_request(paths, cx);
    });
  })
  .detach();
}

/// Open what the clipboard holds: copied files open their originals, an image
/// becomes an untitled PNG document, and text becomes an untitled text
/// document. Nothing happens for an empty clipboard.
fn handle_new_from_clipboard(cx: &mut App) {
  let Some(item) = cx.read_from_clipboard() else {
    tracing::info!("clipboard is empty; nothing to open");
    return;
  };
  let paths: Vec<PathBuf> = item
    .entries
    .iter()
    .filter_map(|entry| match entry {
      gpui_kit::ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
      _ => None,
    })
    .flatten()
    .collect();
  if !paths.is_empty() {
    apply_open_request(paths, cx);
    return;
  }
  let image = item.entries.iter().find_map(|entry| match entry {
    gpui_kit::ClipboardEntry::Image(image) => Some(image.clone()),
    _ => None,
  });
  if let Some(image) = image {
    window::open_clipboard_image_window(image, cx);
    return;
  }
  let Some(text) = item.text() else {
    tracing::info!("clipboard holds nothing OpenIt can open");
    return;
  };
  open_untitled_text(text, cx);
}

/// Reload user themes once per application-level settings change: a different
/// theme configuration, or a settings snapshot loaded from the settings file
/// (whose theme files may have changed with it).
fn observe_app_settings(cx: &mut App) {
  let reload = cx.update_global::<AppSettingsSubscription, _>(|subscription, cx| {
    let theme = cx.global::<AppSettings>().0.theme.clone();
    let file_generation = cx.global::<SettingsStore>().file_generation();
    let changed = subscription.previous_theme != theme || subscription.previous_file_generation != file_generation;
    subscription.previous_theme = theme;
    subscription.previous_file_generation = file_generation;
    changed
  });
  menus::install(cx);
  if reload {
    theme::reload_user_themes(None, cx);
  }
}

/// Install the one application-level `AppSettings` observer.
pub(crate) fn install_app_settings_observer(cx: &mut App) {
  let previous_theme = cx.global::<AppSettings>().0.theme.clone();
  let previous_file_generation = cx.global::<SettingsStore>().file_generation();
  let subscription = cx.observe_global::<AppSettings>(observe_app_settings);
  cx.set_global(AppSettingsSubscription {
    _subscription: subscription,
    previous_theme,
    previous_file_generation,
  });
}

fn open_recovery_store() -> Option<Arc<RecoveryStore>> {
  let [primary, fallback] = recovery_dirs();
  if let Some(dir) = primary {
    match RecoveryStore::open(dir) {
      Ok(store) => return Some(Arc::new(store)),
      Err(error) => tracing::error!(%error, "data dir unusable"),
    }
  }
  match RecoveryStore::open(fallback?) {
    Ok(store) => Some(Arc::new(store)),
    Err(error) => {
      tracing::error!(%error, "temporary recovery directory unusable");
      None
    },
  }
}
/// Install every key binding. Image and PDF shortcuts are scoped to their own
/// window, where `cmd-r`, `cmd-l`, and `cmd-f` cannot collide with a text action.
fn bind_keys(cx: &mut App) {
  cx.bind_keys([
    KeyBinding::new("cmd-s", actions::Save, None),
    KeyBinding::new("ctrl-s", actions::Save, None),
    KeyBinding::new("cmd-o", actions::OpenFile, None),
    KeyBinding::new("ctrl-o", actions::OpenFile, None),
    KeyBinding::new("cmd-p", actions::GoToFile, None),
    KeyBinding::new("ctrl-p", actions::GoToFile, None),
    KeyBinding::new("cmd-n", actions::NewFromClipboard, None),
    KeyBinding::new("ctrl-n", actions::NewFromClipboard, None),
    KeyBinding::new("cmd-shift-e", actions::ToggleMode, None),
    KeyBinding::new("ctrl-shift-e", actions::ToggleMode, None),
    KeyBinding::new("cmd-k cmd-t", actions::ColorTheme, None),
    KeyBinding::new("ctrl-k ctrl-t", actions::ColorTheme, None),
    KeyBinding::new("cmd-k cmd-u", actions::UiFont, None),
    KeyBinding::new("ctrl-k ctrl-u", actions::UiFont, None),
    KeyBinding::new("cmd-k cmd-c", actions::CodeFont, None),
    KeyBinding::new("ctrl-k ctrl-c", actions::CodeFont, None),
    KeyBinding::new("cmd-w", actions::CloseWindow, None),
    KeyBinding::new("ctrl-w", actions::CloseWindow, None),
    KeyBinding::new("cmd-q", actions::Quit, None),
    KeyBinding::new("cmd-=", actions::ZoomIn, Some("ImageView")),
    KeyBinding::new("ctrl-=", actions::ZoomIn, Some("ImageView")),
    KeyBinding::new("cmd--", actions::ZoomOut, Some("ImageView")),
    KeyBinding::new("ctrl--", actions::ZoomOut, Some("ImageView")),
    KeyBinding::new("cmd-0", actions::ZoomToFit, Some("ImageView")),
    KeyBinding::new("ctrl-0", actions::ZoomToFit, Some("ImageView")),
    KeyBinding::new("cmd-1", actions::ActualSize, Some("ImageView")),
    KeyBinding::new("ctrl-1", actions::ActualSize, Some("ImageView")),
    KeyBinding::new("cmd-z", gpui_kit::component::input::Undo, Some("ImageView")),
    KeyBinding::new("ctrl-z", gpui_kit::component::input::Undo, Some("ImageView")),
    KeyBinding::new("cmd-shift-z", gpui_kit::component::input::Redo, Some("ImageView")),
    KeyBinding::new("ctrl-shift-z", gpui_kit::component::input::Redo, Some("ImageView")),
    KeyBinding::new("cmd-shift-s", actions::Export, Some("ImageView")),
    KeyBinding::new("ctrl-shift-s", actions::Export, Some("ImageView")),
    KeyBinding::new("cmd-l", actions::RotateLeft, Some("ImageView")),
    KeyBinding::new("ctrl-l", actions::RotateLeft, Some("ImageView")),
    KeyBinding::new("cmd-r", actions::RotateRight, Some("ImageView")),
    KeyBinding::new("ctrl-r", actions::RotateRight, Some("ImageView")),
    KeyBinding::new("cmd-=", actions::ZoomIn, Some("PdfView")),
    KeyBinding::new("ctrl-=", actions::ZoomIn, Some("PdfView")),
    KeyBinding::new("cmd--", actions::ZoomOut, Some("PdfView")),
    KeyBinding::new("ctrl--", actions::ZoomOut, Some("PdfView")),
    KeyBinding::new("cmd-0", actions::ZoomToFit, Some("PdfView")),
    KeyBinding::new("ctrl-0", actions::ZoomToFit, Some("PdfView")),
    KeyBinding::new("cmd-1", actions::ActualSize, Some("PdfView")),
    KeyBinding::new("ctrl-1", actions::ActualSize, Some("PdfView")),
    KeyBinding::new("cmd-f", actions::Find, Some("PdfView")),
    KeyBinding::new("ctrl-f", actions::Find, Some("PdfView")),
    KeyBinding::new("cmd-g", actions::GoToPage, Some("PdfView")),
    KeyBinding::new("ctrl-g", actions::GoToPage, Some("PdfView")),
    KeyBinding::new("cmd-a", actions::SelectAll, Some("PdfView")),
    KeyBinding::new("ctrl-a", actions::SelectAll, Some("PdfView")),
    KeyBinding::new("cmd-c", actions::Copy, Some("PdfView")),
    KeyBinding::new("ctrl-c", actions::Copy, Some("PdfView")),
    KeyBinding::new("cmd-shift-m", actions::ConvertToMarkdown, Some("PdfView")),
    KeyBinding::new("ctrl-shift-m", actions::ConvertToMarkdown, Some("PdfView")),
    KeyBinding::new("cmd-shift-p", actions::PdfPages, Some("PdfView")),
    KeyBinding::new("ctrl-shift-p", actions::PdfPages, Some("PdfView")),
    KeyBinding::new("pageup", actions::PageUp, Some("PdfView")),
    KeyBinding::new("pagedown", actions::PageDown, Some("PdfView")),
    KeyBinding::new("home", actions::FirstPage, Some("PdfView")),
    KeyBinding::new("end", actions::LastPage, Some("PdfView")),
    KeyBinding::new("enter", actions::NextMatch, Some("PdfFindBar")),
    KeyBinding::new("shift-enter", actions::PreviousMatch, Some("PdfFindBar")),
  ]);
}

fn start_open_request_listener(paths: &[PathBuf], cx: &mut App) -> bool {
  match ipc::listen() {
    Ok(listener) => {
      let (tx, rx) = async_channel::unbounded();
      match std::thread::Builder::new().name("openit-ipc".to_owned()).spawn(move || {
        loop {
          match listener.accept() {
            Ok(incoming) => {
              if tx.send_blocking(incoming.take()).is_err() {
                break;
              }
            },
            Err(error) => tracing::debug!(%error, "open request accept failed"),
          }
        }
      }) {
        Ok(_) => pump_open_requests(rx, cx),
        Err(error) => tracing::error!(%error, "could not start open request listener"),
      }
      true
    },
    Err(error) => {
      if ipc::send(paths).is_ok() {
        tracing::info!("another instance accepted the paths; quitting");
        false
      } else {
        tracing::error!(%error, "could not listen for open requests");
        true
      }
    },
  }
}

#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
fn pump_open_requests(rx: async_channel::Receiver<Vec<PathBuf>>, cx: &mut App) {
  cx.spawn(async move |cx| {
    loop {
      let received = cx
        .background_spawn({
          let rx = rx.clone();
          async move { rx.recv().await }
        })
        .await;
      let Ok(paths) = received else {
        break;
      };
      cx.update(|cx| apply_open_request(paths, cx));
    }
  })
  .detach();
}

fn main() {
  let paths = match cli::from_env() {
    cli::Process::Exit(code) => std::process::exit(i32::from(code)),
    cli::Process::Launch(paths) => paths,
  };
  run_app(paths);
}

fn attach_app_handlers(cx: &mut App) {
  bind_keys(cx);
  menus::install(cx);
  svg::warm_fonts(cx);
  install_app_settings_observer(cx);
  cx.set_global(PendingCleanups::default());
  cx.set_global(QuitInProgress::default());
  cx.set_global(QuitCommitted::default());
  let on_quit = cx.on_app_quit(|cx| {
    // Runs for every quit, including a Cmd+Q the platform handles itself, so
    // an image turn waiting on its debounce still reaches the file.
    let writes = flush_pending_image_writes(cx);
    let cleanups = cx.update_global::<PendingCleanups, _>(|pending, _| std::mem::take(&mut pending.0));
    async move {
      for write in writes {
        if let Err(error) = write.await {
          tracing::error!(%error, "an image edit could not be written before quitting");
        }
      }
      for cleanup in cleanups {
        cleanup.await;
      }
    }
  });
  cx.set_global(AppQuitSubscription { _subscription: on_quit });
  install_last_window_quit(cx);
  cx.on_action(|action: &actions::SetThemeMode, cx| handle_set_theme_mode(action, cx));
  cx.on_action(|action: &actions::SetMarkdownPreviewWidth, cx| handle_set_markdown_preview_width(action, cx));
  cx.on_action(|_: &actions::ToggleAlwaysShowStatusBar, cx| handle_toggle_always_show_status_bar(cx));
  cx.on_action(|_: &actions::OpenFile, cx| handle_open_file(cx));
  cx.on_action(|_: &actions::NewFromClipboard, cx| handle_new_from_clipboard(cx));
  #[cfg(target_os = "macos")]
  cx.on_action(|_: &actions::InstallCommandLineTools, cx| cli_install::open_window(cx));
  cx.on_action(|_: &actions::Quit, cx| {
    request_quit(cx).detach();
  });
}

fn run_app(paths: Vec<PathBuf>) {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();
  if ipc::send(&paths).is_ok() {
    return;
  }
  #[cfg(unix)]
  cli::hand_off_under_product_name();

  let app = gpui_kit::application()
    .with_assets(assets::AppAssets)
    .with_quit_mode(gpui_kit::QuitMode::Explicit);
  let (open_url_tx, open_url_rx) = async_channel::unbounded();
  app.on_open_urls(move |urls| {
    if open_url_tx.send_blocking(urls).is_err() {
      tracing::error!("open url event dropped");
    }
  });
  app.on_reopen(on_reopen);
  app.run(move |cx| {
    gpui_kit::init(cx);
    let settings_path = Settings::default_path();
    let settings = settings_path.as_deref().map_or_else(Settings::default, |path| {
      Settings::load(path).unwrap_or_else(|error| {
        tracing::error!(%error, "settings unreadable; using defaults");
        Settings::default()
      })
    });
    cx.set_global(AppSettings(settings));
    cx.set_global(SettingsStore::new(settings_path.clone()));
    cx.set_global(theme::ThemeDirs {
      user: dirs::config_dir().map(|path| path.join("openit").join("themes")),
    });
    theme::init(cx);
    match reqwest_client::ReqwestClient::user_agent(&format!("openit/{}", env!("CARGO_PKG_VERSION"))) {
      Ok(client) => cx.set_http_client(Arc::new(client)),
      Err(error) => tracing::error!(%error, "could not initialize HTTP client; using default client"),
    }
    if let Some(cache) = ResourceCacheHandle::open() {
      let prune_cache = Arc::clone(&cache.0);
      cx.set_global(cache);
      cx.background_spawn(async move {
        if let Err(error) = prune_cache.prune() {
          tracing::warn!(%error, "could not prune resource cache");
        }
      })
      .detach();
    } else {
      tracing::error!("resource cache unavailable; continuing without a cache");
    }
    cx.set_global(Fetcher(Arc::new(HttpFetcher::new(cx.http_client()))));
    cx.set_app_identity("com.openit.app", "OpenIt");
    attach_app_handlers(cx);
    let store = open_recovery_store();
    cx.set_global(Recovery(store.clone()));

    if let Some(path) = settings_path {
      watch_settings(path, cx);
    }
    crate::open_urls::listen(open_url_rx, cx);
    if !start_open_request_listener(&paths, cx) {
      cx.quit();
      return;
    }
    if let Some(store) = store {
      open_startup_windows(&store, paths, cx);
    } else {
      tracing::error!("no writable location for drafts; opening documents without recovery");
      if paths.is_empty() {
        open_empty_window(cx);
      } else {
        for path in paths {
          open_document_window(path, cx);
        }
      }
      crate::open_urls::startup_ready(cx);
    }
    cx.activate(true);
  });
}
#[cfg(test)]
mod tests {
  use std::fs;
  use std::time::Duration;

  use gpui_kit::component::theme::Theme;
  use gpui_kit::{AppContext, MenuItem, TestAppContext, WindowHandle};
  use openit_core::document::load_text;
  use openit_core::resource::DomainFamily;
  use openit_core::session::SessionId;
  use openit_core::settings::{MarkdownPreviewWidth, Settings, ThemeMode};
  use openit_core::theme::Rgba;

  use super::recovery_dirs;
  use crate::actions::{SetMarkdownPreviewWidth, SetThemeMode, ToggleAlwaysShowStatusBar};
  use crate::document_view::DocumentView;
  use crate::settings::{AppSettings, SettingsStore, watch_settings_for_test};
  use crate::theme::{ActivePalette, ThemeCatalog, ThemeDirs};

  #[test]
  fn fallback_recovery_dir_is_stable_across_launches() {
    assert_eq!(recovery_dirs()[1], Some(std::env::temp_dir().join("openit-drafts")));
  }

  #[gpui_kit::test]
  fn set_theme_mode_action_writes_the_settings_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let document_dir = tempfile::tempdir().unwrap();
    let document_path = document_dir.path().join("notes.txt");
    fs::write(&document_path, "notes\n").unwrap();
    let loaded = load_text(&document_path).unwrap();

    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(settings_path.clone())));
      cx.set_global(ThemeDirs::default());
      crate::theme::init(cx);
      cx.on_action(|action: &SetThemeMode, cx| crate::handle_set_theme_mode(action, cx));
    });
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(document_path, loaded, SessionId::new(), window, cx));

    cx.dispatch_action(SetThemeMode(ThemeMode::Dark));
    cx.run_until_parked();

    assert_eq!(Settings::load(&settings_path).unwrap().theme.mode, ThemeMode::Dark);
    assert!(cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));

    for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
      let settings = Settings {
        theme: openit_core::settings::ThemeSettings { mode, ..Settings::default().theme },
        ..Settings::default()
      };
      let checked = crate::menus::build(&settings)
        .into_iter()
        .flat_map(|menu| menu.items)
        .find_map(|item| match item {
          MenuItem::Submenu(menu) if menu.name == "Appearance" => {
            Some(menu.items.into_iter().filter(MenuItem::is_checked).count())
          },
          _ => None,
        })
        .unwrap();
      assert_eq!(checked, 1);
    }
  }

  /// The checked state of the View menu's "Always Show Status Bar" item.
  fn status_bar_item_is_checked(settings: &Settings) -> bool {
    crate::menus::build(settings)
      .into_iter()
      .filter(|menu| menu.name == "View")
      .flat_map(|menu| menu.items)
      .find(|item| matches!(item, MenuItem::Action { name, .. } if name == "Always Show Status Bar"))
      .map(|item| item.is_checked())
      .expect("the View menu offers the status bar toggle")
  }

  /// The checked state of a Markdown preview width menu item.
  fn markdown_preview_width_item_is_checked(settings: &Settings, label: &str) -> bool {
    crate::menus::build(settings)
      .into_iter()
      .filter(|menu| menu.name == "View")
      .flat_map(|menu| menu.items)
      .find_map(|item| match item {
        MenuItem::Submenu(menu) if menu.name == "Markdown Preview Width" => menu.items.into_iter().find_map(|item| {
          if matches!(&item, MenuItem::Action { name, .. } if name == label) {
            Some(item.is_checked())
          } else {
            None
          }
        }),
        _ => None,
      })
      .expect("the View menu offers the Markdown Preview Width submenu")
  }

  #[gpui_kit::test]
  fn markdown_preview_width_action_updates_selection_and_persists(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let document_dir = tempfile::tempdir().unwrap();
    let document_path = document_dir.path().join("notes.md");
    fs::write(&document_path, "# Hi\n").unwrap();
    let loaded = load_text(&document_path).unwrap();

    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(settings_path.clone())));
      cx.set_global(ThemeDirs::default());
      crate::theme::init(cx);
      cx.on_action(|action: &SetMarkdownPreviewWidth, cx| crate::handle_set_markdown_preview_width(action, cx));
    });
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(document_path, loaded, SessionId::new(), window, cx));

    assert!(markdown_preview_width_item_is_checked(&Settings::default(), "Readable"));
    assert!(!markdown_preview_width_item_is_checked(&Settings::default(), "Wide"));

    cx.dispatch_action(SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::Wide });
    cx.run_until_parked();

    let saved = Settings::load(&settings_path).unwrap();
    assert_eq!(saved.markdown_preview_width, MarkdownPreviewWidth::Wide);
    assert!(!markdown_preview_width_item_is_checked(&saved, "Readable"));
    assert!(markdown_preview_width_item_is_checked(&saved, "Wide"));
  }

  #[gpui_kit::test]
  fn the_status_bar_menu_item_toggles_the_setting_and_writes_the_file(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let document_dir = tempfile::tempdir().unwrap();
    let document_path = document_dir.path().join("notes.md");
    fs::write(&document_path, "# Hi\n").unwrap();
    let loaded = load_text(&document_path).unwrap();

    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(Some(settings_path.clone())));
      cx.set_global(ThemeDirs::default());
      crate::theme::init(cx);
      cx.on_action(|_: &ToggleAlwaysShowStatusBar, cx| crate::handle_toggle_always_show_status_bar(cx));
    });
    let (_view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(document_path, loaded, SessionId::new(), window, cx));
    assert!(!status_bar_item_is_checked(&Settings::default()));

    cx.dispatch_action(ToggleAlwaysShowStatusBar);
    cx.run_until_parked();

    assert!(Settings::load(&settings_path).unwrap().always_show_status_bar);
    assert!(status_bar_item_is_checked(
      &cx.read_global::<AppSettings, _>(|settings, _| settings.0.clone())
    ));

    cx.dispatch_action(ToggleAlwaysShowStatusBar);
    cx.run_until_parked();

    assert!(!Settings::load(&settings_path).unwrap().always_show_status_bar);
  }

  #[gpui_kit::test]
  fn one_theme_applies_to_every_window_and_the_catalog_builds_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let themes = tempfile::tempdir().unwrap();
    let document_dir = tempfile::tempdir().unwrap();
    let first_path = document_dir.path().join("first.txt");
    fs::write(&first_path, "first\n").unwrap();
    let second_path = document_dir.path().join("second.txt");
    fs::write(&second_path, "second\n").unwrap();
    let first_loaded = load_text(&first_path).unwrap();
    let second_loaded = load_text(&second_path).unwrap();

    cx.update(|cx| {
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(None));
      cx.set_global(ThemeDirs { user: Some(themes.path().to_path_buf()) });
      crate::theme::init(cx);
      crate::install_app_settings_observer(cx);
    });
    let _first: WindowHandle<DocumentView> =
      cx.add_window(|window, cx| DocumentView::open(first_path, first_loaded, SessionId::new(), window, cx));
    let _second: WindowHandle<DocumentView> =
      cx.add_window(|window, cx| DocumentView::open(second_path, second_loaded, SessionId::new(), window, cx));
    cx.run_until_parked();

    let before = cx.update(|cx| ThemeCatalog::get(cx).builds());
    cx.update(|cx| SettingsStore::update(cx, |settings| settings.theme.mode = ThemeMode::Dark));
    cx.run_until_parked();
    cx.update(|cx| {
      assert_eq!(ThemeCatalog::get(cx).builds() - before, 1);
      let expected = ThemeCatalog::get(cx)
        .palette("warm-burnout-dark")
        .expect("bundled dark default");
      assert_eq!(cx.global::<ActivePalette>().0, expected);
      assert!(Theme::global(cx).mode.is_dark());
    });

    let before = cx.update(|cx| ThemeCatalog::get(cx).builds());
    let family = DomainFamily::of_host("example.org");
    cx.update(|cx| SettingsStore::update(cx, |settings| settings.allow_family(&family)));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| ThemeCatalog::get(cx).builds()), before);
  }

  #[gpui_kit::test]
  fn a_settings_file_change_rescans_user_themes(cx: &TestAppContext) {
    cx.update(gpui_kit::init);
    let themes = tempfile::tempdir().unwrap();
    let theme_path = themes.path().join("mine.json");
    fs::write(
      &theme_path,
      r##"{"name":"Mine","themes":[{"name":"Mine Dark","appearance":"dark","style":{"editor.background":"#010203"}}]}"##,
    )
    .unwrap();
    let settings_dir = tempfile::tempdir().unwrap();
    let settings_path = settings_dir.path().join("settings.toml");
    let pinned = "[theme]\nmode = \"dark\"\ndark = \"mine-dark\"\n";
    fs::write(&settings_path, pinned).unwrap();
    let settings = Settings::load(&settings_path).unwrap();
    cx.update(|cx| {
      cx.set_global(AppSettings(settings));
      cx.set_global(SettingsStore::new(Some(settings_path.clone())));
      cx.set_global(ThemeDirs { user: Some(themes.path().to_path_buf()) });
      crate::theme::init(cx);
      crate::install_app_settings_observer(cx);
      assert_eq!(cx.global::<ActivePalette>().0.background, Rgba::rgb(1, 2, 3));
    });
    let sender = watch_settings_for_test(settings_path.clone(), cx);

    // The theme file and the settings file both change; the theme settings do not.
    fs::write(
      &theme_path,
      r##"{"name":"Mine","themes":[{"name":"Mine Dark","appearance":"dark","style":{"editor.background":"#040506"}}]}"##,
    )
    .unwrap();
    fs::write(&settings_path, format!("autosave = true\n{pinned}")).unwrap();
    sender.try_send(()).unwrap();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();

    cx.update(|cx| {
      assert!(cx.global::<AppSettings>().0.autosave);
      assert_eq!(cx.global::<ActivePalette>().0.background, Rgba::rgb(4, 5, 6));
    });
  }
}
