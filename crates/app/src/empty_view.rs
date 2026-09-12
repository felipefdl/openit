//! The empty window: a waiting surface that an open request fills in place.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, TitleBar};
use gpui_kit::prelude::{InteractiveElement as _, Styled as _};
use gpui_kit::{
  Context, DragMoveEvent, ExternalPaths, FocusHandle, FontWeight, IntoElement, ParentElement as _, Render, Window, div,
  px, svg,
};

use crate::actions::{CloseWindow, OpenFile};
use crate::drop::{apply_external_paths, external_paths_ring};
use crate::theme::ActivePalette;

/// Fourth root view: no document, no draft, no status bar.
pub struct EmptyView {
  focus: FocusHandle,
  drop_hover: bool,
}

impl EmptyView {
  /// Install the empty surface as this window's root.
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let focus = cx.focus_handle();
    window.focus(&focus, cx);
    Self { focus, drop_hover: false }
  }

  #[allow(clippy::unused_self, reason = "CloseWindow listener signature")]
  fn close(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
    window.remove_window();
  }

  fn on_external_drag(&mut self, event: &DragMoveEvent<ExternalPaths>, _: &mut Window, cx: &mut Context<Self>) {
    let over = event.bounds.contains(&event.event.position);
    if self.drop_hover != over {
      self.drop_hover = over;
      cx.notify();
    }
  }

  fn on_external_drop(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
    self.drop_hover = false;
    apply_external_paths(paths, cx);
  }
}

impl Render for EmptyView {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    if !cx.has_active_drag() {
      self.drop_hover = false;
    }
    let theme = cx.theme();
    let drop_hover = self.drop_hover;
    let mark = cx.global::<ActivePalette>().mark().opacity(if drop_hover { 1.0 } else { 0.6 });
    let hint = if drop_hover {
      theme.foreground
    } else {
      theme.muted_foreground
    };
    div()
      .key_context("EmptyView")
      .track_focus(&self.focus)
      .on_action(cx.listener(Self::close))
      .on_drag_move(cx.listener(Self::on_external_drag))
      .on_drop(cx.listener(Self::on_external_drop))
      .drag_over::<ExternalPaths>(|style, _, _, cx| external_paths_ring(style, cx))
      .flex()
      .flex_col()
      .size_full()
      .bg(theme.background)
      .text_color(theme.foreground)
      .child(
        TitleBar::new().border_0().bg(theme.background).child(
          div()
            .flex()
            .items_center()
            .w_full()
            .h_full()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child("OpenIt"),
        ),
      )
      .child(
        div()
          .flex_1()
          .flex()
          .flex_col()
          .items_center()
          .justify_center()
          .gap_3()
          .child(
            svg()
              .path("brand/openit-glyph.svg")
              .size(px(80.))
              .text_color(mark)
              .flex_shrink_0(),
          )
          .child(
            Button::new("select-file")
              .ghost()
              .label("Select a file")
              .on_click(cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(OpenFile), cx))),
          )
          .child(div().text_size(px(12.)).text_color(hint).child("or drag and drop a file")),
      )
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::rc::Rc;

  use gpui_kit::TestAppContext;
  use gpui_kit::test::TestWindowExt as _;
  use openit_core::settings::Settings;

  use crate::actions::OpenFile;

  use super::EmptyView;

  fn init_app(cx: &TestAppContext) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(crate::settings::AppSettings(Settings::default()));
      cx.set_global(crate::theme::ThemeDirs::default());
      crate::theme::init(cx);
    });
  }

  #[gpui_kit::test]
  fn the_select_file_button_dispatches_open_file(cx: &mut TestAppContext) {
    init_app(cx);
    let dispatched = Rc::new(Cell::new(false));
    cx.update({
      let dispatched = Rc::clone(&dispatched);
      move |cx| {
        cx.on_action(move |_: &OpenFile, _| dispatched.set(true));
      }
    });
    let (_view, cx) = cx.add_window_view(EmptyView::new);
    cx.update(|window, cx| {
      window.render_frame(cx);
      window.click("select-file", cx);
    });
    assert!(dispatched.get(), "the ghost button dispatches OpenFile");
  }
}
