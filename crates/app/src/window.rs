//! Opening a document in its own window, and replacing the document in one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::component::TitleBar;
use gpui_kit::{
  AnyWindowHandle, App, AppContext, Bounds, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, px,
  size,
};
use openit_core::document::{load_image, load_pdf, load_text};
use openit_core::kind::{DocumentKind, detect};
use openit_core::recovery::{Draft, RecoveryStore};
use openit_core::session::SessionId;

use crate::document_view::DocumentView;
use crate::empty_view::EmptyView;
use crate::image_view::ImageView;
use crate::pdf_view::PdfView;
use crate::session::Recovery;

fn path_identity(path: PathBuf) -> PathBuf {
  let absolute = std::path::absolute(&path).unwrap_or(path);
  if absolute.exists() {
    fs::canonicalize(&absolute).unwrap_or(absolute)
  } else {
    absolute
  }
}

fn document_title(path: Option<&Path>) -> SharedString {
  path
    .and_then(Path::file_name)
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default()
    .into()
}

/// The document view draws the title bar (title, mode toggle, theme) beside the traffic lights.
fn document_window_options(title: SharedString, cx: &App) -> WindowOptions {
  WindowOptions {
    titlebar: Some(TitlebarOptions {
      title: Some(title),
      ..TitleBar::title_bar_options()
    }),
    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(960.), px(720.)), cx))),
    window_min_size: Some(size(px(480.), px(320.))),
    ..TitleBar::window_options()
  }
}

/// Read `path` and open it in a new window. A file no reader accepts goes to
/// the system's default application instead.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_document_window(path: PathBuf, cx: &mut App) {
  let path = path_identity(path);
  crate::handoff::started(cx);
  if detect(&path).is_image() {
    open_image_window(path, cx);
    return;
  }
  if detect(&path).is_pdf() {
    open_pdf_window(path, cx);
    return;
  }
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { load_text(&load_path) }).await;
    let update_result = cx.update(|cx| match result {
      Ok(loaded) => {
        let title = document_title(Some(&path));
        let options = document_window_options(title, cx);
        let opened = cx
          .open_window(options, move |window, cx| {
            cx.new(|cx| DocumentView::open(path, loaded, SessionId::new(), window, cx))
          })
          .map(|_| ());
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "window open failed");
    }
  })
  .detach();
}

/// Replace the document in `window` with `path`.
///
/// The current path is left alone. A path already open elsewhere focuses that
/// window. A dirty document is prompted first; Cancel keeps the buffer.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn replace_document(path: PathBuf, window: &mut Window, cx: &mut App) {
  let path = path_identity(path);
  let handle = window.window_handle();
  cx.spawn(async move |cx| {
    cx.update(|cx| replace_document_inner(path, handle, cx));
  })
  .detach();
}
fn replace_document_inner(path: PathBuf, handle: AnyWindowHandle, cx: &mut App) {
  let current_path = handle.update(cx, |_, window, cx| document_path(window, cx)).ok().flatten();
  if current_path.as_deref() == Some(path.as_path()) {
    return;
  }
  if let Some(other) = window_holding_path(cx, &path, Some(handle)) {
    let _ = other.update(cx, |_, window, _| window.activate_window());
    return;
  }
  let _ = handle.update(cx, |_, window, cx| confirm_leave_root(path, window, cx));
}
fn document_path(window: &Window, cx: &App) -> Option<PathBuf> {
  if let Some(view) = window.root::<DocumentView>().flatten() {
    return nonempty_path(view.read(cx).path().to_path_buf()).map(path_identity);
  }
  if let Some(view) = window.root::<ImageView>().flatten() {
    return nonempty_path(view.read(cx).path().to_path_buf()).map(path_identity);
  }
  if let Some(view) = window.root::<PdfView>().flatten() {
    return nonempty_path(view.read(cx).path().to_path_buf()).map(path_identity);
  }
  None
}

fn nonempty_path(path: PathBuf) -> Option<PathBuf> {
  (!path.as_os_str().is_empty()).then_some(path)
}

fn window_holding_path(cx: &mut App, path: &Path, except: Option<AnyWindowHandle>) -> Option<AnyWindowHandle> {
  for handle in cx.windows() {
    if let Some(skip) = except
      && handle.window_id() == skip.window_id()
    {
      continue;
    }
    let matches = handle
      .update(cx, |_, window, cx| document_path(window, cx).as_deref() == Some(path))
      .unwrap_or(false);
    if matches {
      return Some(handle);
    }
  }
  None
}

/// Activate the application and open each path, or focus the window that already holds it.
///
/// An empty list opens or focuses one empty window. The first path fills an
/// empty window in place when one exists; remaining paths open new windows.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn apply_open_request(paths: Vec<PathBuf>, cx: &mut App) {
  cx.activate(true);
  if paths.is_empty() {
    focus_or_open_empty(cx);
    return;
  }
  let mut paths = paths.into_iter();
  if let Some(first) = paths.next() {
    open_or_fill(first, cx);
  }
  for path in paths {
    let path = path_identity(path);
    if let Some(handle) = window_holding_path(cx, &path, None) {
      let _ = handle.update(cx, |_, window, _| window.activate_window());
      continue;
    }
    open_document_window(path, cx);
  }
}

fn open_or_fill(path: PathBuf, cx: &mut App) {
  let path = path_identity(path);
  if let Some(handle) = window_holding_path(cx, &path, None) {
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    return;
  }
  if let Some(empty) = empty_window(cx) {
    let _ = empty.update(cx, |_, window, cx| confirm_leave_root(path, window, cx));
    return;
  }
  open_document_window(path, cx);
}

fn empty_window(cx: &mut App) -> Option<AnyWindowHandle> {
  for handle in cx.windows() {
    let is_empty = handle
      .update(cx, |_, window, _| window.root::<EmptyView>().flatten().is_some())
      .unwrap_or(false);
    if is_empty {
      return Some(handle);
    }
  }
  None
}

fn focus_or_open_empty(cx: &mut App) {
  if let Some(handle) = empty_window(cx) {
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    return;
  }
  open_empty_window(cx);
}

/// Open one empty window titled OpenIt.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_empty_window(cx: &mut App) {
  let options = document_window_options("OpenIt".into(), cx);
  if let Err(error) = cx.open_window(options, |window, cx| cx.new(|cx| EmptyView::new(window, cx))) {
    tracing::error!(%error, "window open failed");
  }
}

/// Dock or launcher reopen: open an empty window only when none are open.
///
/// Tests call this seam. The test platform panics on `App::on_reopen`.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn on_reopen(cx: &mut App) {
  if cx.windows().is_empty() {
    open_empty_window(cx);
  }
}

fn confirm_leave_root(path: PathBuf, window: &mut Window, cx: &mut App) {
  let replace_path = path.clone();
  let on_ready = move |window: &mut Window, cx: &mut App| {
    load_and_replace(replace_path, window.window_handle(), cx);
  };
  if let Some(view) = window.root::<DocumentView>().flatten() {
    view.update(cx, |view, cx| view.confirm_leave(window, cx, on_ready));
    return;
  }
  if let Some(view) = window.root::<ImageView>().flatten() {
    view.update(cx, |view, cx| view.confirm_leave(window, cx, on_ready));
    return;
  }
  if let Some(view) = window.root::<PdfView>().flatten() {
    view.update(cx, |view, cx| view.confirm_leave(window, cx, on_ready));
    return;
  }
  load_and_replace(path, window.window_handle(), cx);
}

fn end_outgoing_session(window: &Window, cx: &mut App) {
  if let Some(view) = window.root::<DocumentView>().flatten() {
    view.update(cx, |view, cx| view.end_session_without_draft(cx));
    return;
  }
  if let Some(view) = window.root::<ImageView>().flatten() {
    view.update(cx, |view, cx| view.end_session_without_draft(cx));
    return;
  }
  if let Some(view) = window.root::<PdfView>().flatten() {
    view.update(cx, |view, cx| view.end_session_without_draft(cx));
  }
}

fn load_and_replace(path: PathBuf, handle: AnyWindowHandle, cx: &mut App) {
  crate::handoff::started(cx);
  if detect(&path).is_image() {
    replace_with_image(path, handle, cx);
    return;
  }
  if detect(&path).is_pdf() {
    replace_with_pdf(path, handle, cx);
    return;
  }
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { load_text(&load_path) }).await;
    let update_result = cx.update(|cx| match result {
      Ok(loaded) => {
        let title = document_title(Some(&path));
        let opened = cx.update_window(handle, |_, window, cx| {
          end_outgoing_session(window, cx);
          window.replace_root(cx, |window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
          window.set_window_title(&title);
        });
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "document replace failed");
    }
  })
  .detach();
}

fn replace_with_image(path: PathBuf, handle: AnyWindowHandle, cx: &App) {
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx
      .background_spawn(async move {
        let loaded = load_image(&load_path)?;
        let decoded = Some(decode_for(&loaded, load_path.parent()));
        Ok::<_, openit_core::Error>((loaded, decoded))
      })
      .await;
    let update_result = cx.update(|cx| match result {
      Ok((loaded, decoded)) => {
        let title = document_title(Some(&path));
        let opened = cx.update_window(handle, |_, window, cx| {
          end_outgoing_session(window, cx);
          window.replace_root(cx, |window, cx| {
            ImageView::open(path, loaded, decoded, SessionId::new(), window, cx)
          });
          window.set_window_title(&title);
        });
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "document replace failed");
    }
  })
  .detach();
}

fn replace_with_pdf(path: PathBuf, handle: AnyWindowHandle, cx: &App) {
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { load_pdf(&load_path) }).await;
    let update_result = cx.update(|cx| match result {
      Ok(loaded) => {
        let title = document_title(Some(&path));
        let opened = cx.update_window(handle, |_, window, cx| {
          end_outgoing_session(window, cx);
          window.replace_root(cx, |window, cx| PdfView::open(path, loaded, window, cx));
          window.set_window_title(&title);
        });
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "document replace failed");
    }
  })
  .detach();
}

/// Read an image and open it in the image viewer.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
fn open_image_window(path: PathBuf, cx: &mut App) {
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx
      .background_spawn(async move {
        let loaded = load_image(&load_path)?;
        // Decode before the window opens so its first frame shows the image.
        let decoded = Some(decode_for(&loaded, load_path.parent()));
        Ok::<_, openit_core::Error>((loaded, decoded))
      })
      .await;
    let update_result = cx.update(|cx| match result {
      Ok((loaded, decoded)) => {
        let options = document_window_options(document_title(Some(&path)), cx);
        let opened = cx
          .open_window(options, move |window, cx| {
            cx.new(|cx| ImageView::open(path, loaded, decoded, SessionId::new(), window, cx))
          })
          .map(|_| ());
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "window open failed");
    }
  })
  .detach();
}

/// Read a PDF and open it in the reader. Parsing happens inside the view, so a
/// long document shows its window at once.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
fn open_pdf_window(path: PathBuf, cx: &mut App) {
  let load_path = path.clone();
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { load_pdf(&load_path) }).await;
    let update_result = cx.update(|cx| match result {
      Ok(loaded) => {
        let options = document_window_options(document_title(Some(&path)), cx);
        let opened = cx
          .open_window(options, move |window, cx| cx.new(|cx| PdfView::open(path, loaded, window, cx)))
          .map(|_| ());
        crate::handoff::settled(cx);
        opened
      },
      Err(error) => {
        crate::handoff::failed(&path, &error, cx);
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "window open failed");
    }
  })
  .detach();
}

/// Decode an image document so the window's first frame shows the image, or
/// the reason it cannot.
fn decode_for(
  loaded: &openit_core::document::LoadedImage,
  base_dir: Option<&Path>,
) -> Result<crate::image_decode::DocumentImage, String> {
  if loaded.format == openit_core::document::ImageFormat::Svg {
    crate::svg::decode_document(&loaded.bytes, openit_core::raster::Transform::IDENTITY, base_dir)
  } else {
    crate::image_decode::decode_document(&loaded.bytes, loaded.format, openit_core::raster::Transform::IDENTITY)
  }
}

/// Open a clipboard image as an untitled document. Non-PNG clipboard formats
/// are converted so an untitled image always saves as PNG.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_clipboard_image_window(image: gpui_kit::Image, cx: &mut App) {
  cx.spawn(async move |cx| {
    let converted = cx
      .background_spawn(async move { openit_core::raster::to_png(&image.bytes) })
      .await;
    let update_result = cx.update(|cx| match converted {
      Ok((bytes, width, height)) => {
        if let Some(empty) = empty_window(cx) {
          return cx.update_window(empty, |_, window, cx| {
            window.replace_root(cx, |window, cx| ImageView::from_clipboard(bytes, (width, height), window, cx));
            window.set_window_title("Untitled");
          });
        }
        let options = document_window_options("Untitled".into(), cx);
        cx.open_window(options, move |window, cx| {
          cx.new(|cx| ImageView::from_clipboard(bytes, (width, height), window, cx))
        })
        .map(|_| ())
      },
      Err(error) => {
        tracing::error!(%error, "the clipboard image could not be read");
        Ok(())
      },
    });
    if let Err(error) = update_result {
      tracing::error!(%error, "window open failed");
    }
  })
  .detach();
}

/// Open a recovered draft in its own window. The file is not read.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_draft_window(mut draft: Draft, cx: &mut App) {
  if let Some(path) = draft.path.as_mut() {
    *path = path_identity(path.clone());
  }
  let title = document_title(draft.path.as_deref());
  let options = document_window_options(title, cx);
  let result = if draft.image.is_some() {
    let Some(store) = cx.global::<Recovery>().0.clone() else {
      tracing::error!("no recovery store; the image draft cannot be reopened");
      return;
    };
    match store.read_blob(draft.session) {
      Ok(bytes) => cx
        .open_window(options, move |window, cx| {
          cx.new(|cx| ImageView::restore(draft, bytes, window, cx))
        })
        .map(|_| ()),
      Err(error) => {
        tracing::error!(%error, "image draft pixels could not be read");
        return;
      },
    }
  } else {
    let kind = draft.path.as_deref().map_or(DocumentKind::Text { language: None }, detect);
    cx.open_window(options, move |window, cx| {
      cx.new(|cx| DocumentView::restore(draft, kind, window, cx))
    })
    .map(|_| ())
  };
  if let Err(error) = result {
    tracing::error!(%error, "window open failed");
  }
}

/// Open untitled text, filling an empty window when one exists.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_untitled_text(text: String, cx: &mut App) {
  let draft = Draft {
    session: SessionId::new(),
    path: None,
    disk: None,
    text,
    cursor: 0,
    image: None,
    schema: None,
  };
  if let Some(empty) = empty_window(cx) {
    let _ = empty.update(cx, |_, window, cx| {
      window.replace_root(cx, |window, cx| {
        DocumentView::restore(draft, DocumentKind::Text { language: None }, window, cx)
      });
      window.set_window_title("Untitled");
    });
    return;
  }
  open_draft_window(draft, cx);
}

/// Restore persisted drafts before opening command-line paths, skipping paths
/// that already have a recovered draft.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI schedules work from a mutable application callback"
)]
pub fn open_startup_windows(store: &RecoveryStore, paths: Vec<PathBuf>, cx: &mut App) {
  let store = store.clone();
  let paths = paths.into_iter().map(path_identity).collect::<Vec<_>>();
  // Held until every startup window is dispatched, so a handoff cannot decide
  // the launch is empty while drafts are still being listed.
  crate::handoff::started(cx);
  cx.spawn(async move |cx| {
    let result = cx.background_spawn(async move { store.list() }).await;
    cx.update(|cx| {
      let mut identities = HashSet::new();
      let mut opened = false;
      match result {
        Ok(drafts) => {
          for draft in drafts {
            let identity = draft.path.as_ref().map(|path| path_identity(path.clone()));
            if let Some(identity) = identity.as_ref()
              && !identities.insert(identity.clone())
            {
              continue;
            }
            open_draft_window(draft, cx);
            opened = true;
          }
        },
        Err(error) => tracing::error!(%error, "could not list drafts"),
      }
      for path in paths {
        if identities.insert(path.clone()) {
          open_document_window(path, cx);
          opened = true;
        }
      }
      if !opened {
        open_empty_window(cx);
      }
      crate::handoff::settled(cx);
      crate::open_urls::startup_ready(cx);
    });
  })
  .detach();
}

#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "gpui_kit::test supplies a mutable test context"
)]
#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex, PoisonError};

  use gpui_kit::{TestAppContext, WindowHandle};
  use openit_core::document::load_text;
  use openit_core::recovery::{Draft, RecoveryStore};

  use crate::document_view::DocumentView;
  use crate::empty_view::EmptyView;
  use crate::image_view::ImageView;
  use crate::pdf_view::PdfView;
  use openit_core::session::SessionId;
  use openit_core::settings::Settings;

  use super::{
    apply_open_request, on_reopen, open_document_window, open_empty_window, open_startup_windows, replace_document,
  };

  fn init_app(cx: &TestAppContext) -> Arc<Mutex<Vec<PathBuf>>> {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(crate::settings::AppSettings(Settings::default()));
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
      crate::handoff::record_openings(cx)
    })
  }

  fn handed_off(recorded: &Arc<Mutex<Vec<PathBuf>>>) -> Vec<PathBuf> {
    recorded.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  #[gpui_kit::test]
  fn open_document_window_loads_in_the_background_and_opens_one_window(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "text").unwrap();

    cx.update(|cx| open_document_window(path, cx));
    assert_eq!(cx.windows().len(), 0);

    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
  }

  #[gpui_kit::test]
  fn a_missing_file_reaches_neither_a_window_nor_the_system_opener(cx: &mut TestAppContext) {
    let recorded = init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.txt");

    cx.update(|cx| open_document_window(path, cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 0);
    assert!(handed_off(&recorded).is_empty());
  }

  #[gpui_kit::test]
  fn a_file_with_no_reader_goes_to_the_system_opener(cx: &mut TestAppContext) {
    let recorded = init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bundle.dmg");
    fs::write(&path, "not a document").unwrap();

    cx.update(|cx| open_document_window(path.clone(), cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 0);
    assert_eq!(handed_off(&recorded), vec![fs::canonicalize(&path).unwrap()]);
  }

  #[gpui_kit::test]
  fn a_pdf_opens_in_the_reader(cx: &mut TestAppContext) {
    let recorded = init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.pdf");
    fs::write(&path, openit_core::pdf::test_support::tiny_pdf_pages(&["one", "two"])).unwrap();

    cx.update(|cx| open_document_window(path, cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
    assert!(
      cx.windows()
        .first()
        .and_then(gpui_kit::AnyWindowHandle::downcast::<PdfView>)
        .is_some(),
      "a PDF opens in the PDF reader"
    );
    assert!(handed_off(&recorded).is_empty());
  }

  #[gpui_kit::test]
  fn bytes_that_are_not_text_go_to_the_system_opener(cx: &mut TestAppContext) {
    let recorded = init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.log");
    fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();

    cx.update(|cx| open_document_window(path.clone(), cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 0);
    assert_eq!(handed_off(&recorded), vec![fs::canonicalize(&path).unwrap()]);
  }

  #[gpui_kit::test]
  fn the_system_opener_does_not_get_the_same_file_twice(cx: &mut TestAppContext) {
    let recorded = init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.zip");
    fs::write(&path, "PK").unwrap();

    cx.update(|cx| open_document_window(path.clone(), cx));
    cx.run_until_parked();
    cx.update(|cx| open_document_window(path.clone(), cx));
    cx.run_until_parked();

    assert_eq!(handed_off(&recorded).len(), 1, "the shell handing it back must not loop");
  }
  #[gpui_kit::test]
  fn drafts_open_before_cli_paths_and_deduplicate(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let draft_path = dir.path().join("draft.txt");
    let cli_path = dir.path().join("cli.txt");
    fs::write(&draft_path, "on disk").unwrap();
    fs::write(&cli_path, "cli").unwrap();
    let store = RecoveryStore::open(dir.path().join("drafts")).unwrap();
    store
      .checkpoint(&Draft {
        session: SessionId::new(),
        path: Some(draft_path.clone()),
        disk: Some(openit_core::watch::Fingerprint::of(&draft_path).unwrap()),
        text: "recovered".to_owned(),
        cursor: 0,
        image: None,
        schema: None,
      })
      .unwrap();

    cx.update(|cx| open_startup_windows(&store, vec![draft_path, cli_path], cx));
    assert_eq!(cx.windows().len(), 0, "draft listing runs off the UI thread");

    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2, "the unique command-line path opens after recovery");
  }

  #[gpui_kit::test]
  fn startup_dedups_duplicate_cli_paths(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "text").unwrap();
    let store = RecoveryStore::open(dir.path().join("drafts")).unwrap();

    cx.update(|cx| open_startup_windows(&store, vec![path.clone(), path], cx));
    assert_eq!(cx.windows().len(), 0);
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
  }

  #[gpui_kit::test]
  fn startup_lists_drafts_off_the_ui_thread(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "text").unwrap();
    let store = RecoveryStore::open(dir.path().join("drafts")).unwrap();
    store
      .checkpoint(&Draft {
        session: SessionId::new(),
        path: Some(path.clone()),
        disk: Some(openit_core::watch::Fingerprint::of(&path).unwrap()),
        text: "recovered".to_owned(),
        cursor: 0,
        image: None,
        schema: None,
      })
      .unwrap();

    cx.update(|cx| open_startup_windows(&store, Vec::new(), cx));
    assert_eq!(cx.windows().len(), 0);
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
  }

  fn write_png(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]))
      .save(&path)
      .unwrap();
    path
  }

  #[gpui_kit::test]
  fn replacing_markdown_with_a_png_swaps_the_root_view(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, store) = crate::document_view::tests::install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let markdown = doc.path().join("notes.md");
    fs::write(&markdown, "# Hi\n").unwrap();
    let png = write_png(doc.path(), "pic.png");
    let loaded = load_text(&markdown).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(markdown, loaded, SessionId::new(), window, cx));
    let outgoing = view.read_with(cx, |view, _| view.session.session);

    cx.update(|window, cx| replace_document(png, window, cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
    assert!(
      cx.update(|window, _| window.root::<ImageView>().flatten().is_some()),
      "a PNG replace installs an image view"
    );
    assert_eq!(cx.window_title().as_deref(), Some("pic.png"));
    assert!(
      store.list().unwrap().iter().all(|draft| draft.session != outgoing),
      "the outgoing session leaves no recovery entry"
    );
  }

  #[gpui_kit::test]
  fn replacing_with_the_current_path_changes_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = crate::document_view::tests::install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("notes.md");
    fs::write(&path, "hello\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) =
      cx.add_window_view(|window, cx| DocumentView::open(path.clone(), loaded, SessionId::new(), window, cx));
    let before = view.read_with(cx, |view, cx| (view.snapshot(cx).unwrap().text.to_string(), view.revision()));

    cx.update(|window, cx| replace_document(path, window, cx));
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
    assert!(
      cx.update(|window, _| window.root::<DocumentView>().flatten().is_some()),
      "the document view stays put"
    );
    view.read_with(cx, |view, cx| {
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), before.0);
      assert_eq!(view.revision(), before.1);
    });
  }

  #[gpui_kit::test]
  fn replacing_with_a_path_open_elsewhere_focuses_that_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = crate::document_view::tests::install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path_a = doc.path().join("a.md");
    let path_b = doc.path().join("b.md");
    fs::write(&path_a, "aaa\n").unwrap();
    fs::write(&path_b, "bbb\n").unwrap();
    let loaded_a = load_text(&path_a).unwrap();
    let loaded_b = load_text(&path_b).unwrap();
    let a: WindowHandle<DocumentView> =
      cx.add_window(|window, cx| DocumentView::open(path_a, loaded_a, SessionId::new(), window, cx));
    let b: WindowHandle<DocumentView> =
      cx.add_window(|window, cx| DocumentView::open(path_b.clone(), loaded_b, SessionId::new(), window, cx));
    cx.run_until_parked();

    a.update(cx, |_, window, _| window.activate_window()).unwrap();
    a.update(cx, |_, window, cx| replace_document(path_b, window, cx)).unwrap();
    cx.run_until_parked();

    assert_eq!(cx.update(|cx| cx.active_window()), Some(b.into()));
    assert_eq!(cx.windows().len(), 2);
    a.update(cx, |view, _, cx| {
      assert_eq!(view.title(), "a.md");
      assert_eq!(view.snapshot(cx).unwrap().text.to_string(), "aaa\n");
    })
    .unwrap();
  }

  #[gpui_kit::test]
  fn replacing_a_dirty_document_prompts_and_cancel_keeps_the_buffer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = crate::document_view::tests::install_globals(cx);
    let doc = tempfile::tempdir().unwrap();
    let path = doc.path().join("notes.txt");
    let other = doc.path().join("other.txt");
    fs::write(&path, "old\n").unwrap();
    fs::write(&other, "other\n").unwrap();
    let loaded = load_text(&path).unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| DocumentView::open(path, loaded, SessionId::new(), window, cx));
    cx.simulate_input("x");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.is_dirty()));

    cx.update(|window, cx| replace_document(other, window, cx));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();

    view.read_with(cx, |view, cx| {
      assert!(view.is_dirty());
      assert!(view.snapshot(cx).unwrap().text.to_string().starts_with('x'));
    });
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.update(|window, _| window.root::<DocumentView>().flatten().is_some()));
  }
  #[gpui_kit::test]
  fn an_open_request_opens_a_window_and_a_repeat_focuses_it(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "hello").unwrap();

    cx.update(|cx| apply_open_request(vec![path.clone()], cx));
    cx.run_until_parked();
    let first = cx.windows().into_iter().next();
    assert!(first.is_some(), "the first request opens a window");

    cx.update(|cx| apply_open_request(vec![path], cx));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(cx.update(|cx| cx.active_window()), first);
  }

  #[gpui_kit::test]
  fn startup_with_no_paths_and_no_drafts_opens_one_empty_window(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path().join("drafts")).unwrap();

    cx.update(|cx| open_startup_windows(&store, Vec::new(), cx));
    assert_eq!(cx.windows().len(), 0, "draft listing runs off the UI thread");
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 1);
    assert!(
      cx.windows()
        .first()
        .and_then(gpui_kit::AnyWindowHandle::downcast::<EmptyView>)
        .is_some(),
      "a launch with no paths and no drafts yields one EmptyView"
    );
  }

  #[gpui_kit::test]
  fn an_open_request_fills_the_empty_window_and_a_second_opens_another(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(open_empty_window);
    let empty = cx.windows().into_iter().next().expect("one empty window");
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    fs::write(&first, "a").unwrap();
    fs::write(&second, "b").unwrap();

    cx.update(|cx| apply_open_request(vec![first], cx));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    assert_eq!(cx.windows()[0], empty, "filling keeps the same window handle");
    assert!(
      empty
        .update(cx, |_, window, _| window.root::<DocumentView>().flatten().is_some())
        .unwrap(),
      "the empty root becomes a DocumentView"
    );

    cx.update(|cx| apply_open_request(vec![second], cx));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2, "a second request opens a new window");
  }

  #[gpui_kit::test]
  fn reopen_with_zero_windows_opens_one_empty(cx: &mut TestAppContext) {
    init_app(cx);
    assert!(cx.windows().is_empty());
    cx.update(on_reopen);
    assert_eq!(cx.windows().len(), 1);
    assert!(
      cx.windows()
        .first()
        .and_then(gpui_kit::AnyWindowHandle::downcast::<EmptyView>)
        .is_some(),
      "reopen with zero windows opens EmptyView"
    );

    cx.update(on_reopen);
    assert_eq!(cx.windows().len(), 1, "reopen with a window open does nothing");
  }
}
