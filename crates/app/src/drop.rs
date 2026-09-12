//! Drop of external paths onto a root view.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, ExternalPaths, StyleRefinement, Styled as _};

use crate::window::apply_open_request;

/// Open every dropped path through the shared open-request rule.
#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "GPUI drop listeners receive a mutable App"
)]
pub fn apply_external_paths(paths: &ExternalPaths, cx: &mut App) {
  let paths = paths.paths().to_vec();
  // The drop listener holds the window. Defer so `empty_window` can update it.
  cx.spawn(async move |cx| {
    cx.update(|cx| apply_open_request(paths, cx));
  })
  .detach();
}

/// Thin accent ring inside the window edge while external paths hover.
pub fn external_paths_ring(style: StyleRefinement, cx: &App) -> StyleRefinement {
  style.border_1().border_color(cx.theme().accent)
}

#[allow(
  clippy::needless_pass_by_ref_mut,
  reason = "gpui_kit::test supplies a mutable test context"
)]
#[cfg(test)]
mod tests {
  use std::fs;
  use std::path::PathBuf;

  use gpui_kit::test::TestWindowExt as _;
  use gpui_kit::{AnyWindowHandle, ExternalPaths, FileDropEvent, InputEvent as _, TestAppContext, point, px};
  use openit_core::settings::Settings;

  use crate::document_view::DocumentView;
  use crate::empty_view::EmptyView;
  use crate::window::{open_document_window, open_empty_window};

  fn init_app(cx: &TestAppContext) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(crate::settings::AppSettings(Settings::default()));
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
      crate::handoff::record_openings(cx);
    });
  }

  fn write_text(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, name).unwrap();
    path
  }

  fn drop_external_paths(cx: &mut TestAppContext, handle: AnyWindowHandle, paths: Vec<PathBuf>) {
    let payload = ExternalPaths(paths.into_iter().collect());
    handle
      .update(cx, |_, window, cx| {
        window.render_frame(cx);
        let position = point(px(200.), px(200.));
        window.dispatch_event(
          FileDropEvent::Entered { position, paths: payload.clone() }.to_platform_input(),
          cx,
        );
        window.render_frame(cx);
        window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
      })
      .unwrap();
  }

  fn root_is<V: gpui_kit::Render>(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    handle
      .update(cx, |_, window, _| window.root::<V>().flatten().is_some())
      .unwrap()
  }

  fn document_path(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Option<PathBuf> {
    handle
      .update(cx, |_, window, cx| {
        window
          .root::<DocumentView>()
          .flatten()
          .map(|view| view.read(cx).path().to_path_buf())
      })
      .unwrap()
  }

  #[gpui_kit::test]
  fn dropping_two_paths_on_a_document_window_opens_two_new_windows(cx: &mut TestAppContext) {
    init_app(cx);
    let dir = tempfile::tempdir().unwrap();
    let original_path = write_text(dir.path(), "original.txt");
    let first = write_text(dir.path(), "a.txt");
    let second = write_text(dir.path(), "b.txt");

    cx.update(|cx| open_document_window(original_path.clone(), cx));
    cx.run_until_parked();
    let original = cx.windows().into_iter().next().expect("one document window");
    assert!(root_is::<DocumentView>(cx, original));

    drop_external_paths(cx, original, vec![first, second]);
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 3, "the drop opens two new windows");
    assert!(cx.windows().contains(&original), "the original window stays open");
    assert!(root_is::<DocumentView>(cx, original), "the original root stays a document");
    let opened = document_path(cx, original).and_then(|path| fs::canonicalize(path).ok());
    let expected = fs::canonicalize(&original_path).ok();
    assert_eq!(opened, expected, "the original document is not replaced");
  }

  #[gpui_kit::test]
  fn dropping_two_paths_on_an_empty_window_fills_it_and_opens_one_more(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(open_empty_window);
    let empty = cx.windows().into_iter().next().expect("one empty window");
    assert!(root_is::<EmptyView>(cx, empty));

    let dir = tempfile::tempdir().unwrap();
    let first = write_text(dir.path(), "a.txt");
    let second = write_text(dir.path(), "b.txt");

    drop_external_paths(cx, empty, vec![first, second]);
    cx.run_until_parked();

    assert_eq!(cx.windows().len(), 2, "the first path fills the empty window");
    assert_eq!(cx.windows()[0], empty, "filling keeps the same window handle");
    assert!(root_is::<DocumentView>(cx, empty), "the empty root becomes a DocumentView");
  }
}
