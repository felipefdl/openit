//! The empty window: a waiting surface that an open request fills in place.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, TitleBar};
use gpui_kit::prelude::{InteractiveElement as _, Styled as _};
use gpui_kit::{
  AppContext as _, Context, DragMoveEvent, Entity, ExternalPaths, FocusHandle, FontWeight, IntoElement,
  ParentElement as _, Render, Subscription, Window, div, px, svg,
};

use crate::actions::{CloseWindow, CodeFont, ColorTheme, OpenFile, UiFont};
use crate::drop::{apply_external_paths, external_paths_ring};
use crate::font_picker::{FontPicker, FontPickerEvent, FontSlot};
use crate::theme::{ActivePalette, observe_appearance};
use crate::theme_picker::{ThemePicker, ThemePickerEvent};

/// Fourth root view: no document, no draft, no status bar.
pub struct EmptyView {
  focus: FocusHandle,
  drop_hover: bool,
  theme_picker: Option<Entity<ThemePicker>>,
  theme_picker_subscription: Option<Subscription>,
  font_picker: Option<Entity<FontPicker>>,
  font_picker_subscription: Option<Subscription>,
  #[allow(dead_code, reason = "the subscription keeps the appearance observer alive")]
  appearance_observation: Option<Subscription>,
}

impl EmptyView {
  /// Install the empty surface as this window's root.
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let focus = cx.focus_handle();
    window.focus(&focus, cx);
    Self {
      focus,
      drop_hover: false,
      theme_picker: None,
      theme_picker_subscription: None,
      font_picker: None,
      font_picker_subscription: None,
      appearance_observation: Some(observe_appearance(window)),
    }
  }

  fn open_theme_picker(&mut self, _: &ColorTheme, window: &mut Window, cx: &mut Context<Self>) {
    self.dismiss_font_picker(window, cx);
    if let Some(picker) = &self.theme_picker {
      picker.update(cx, |picker, cx| picker.focus(window, cx));
      return;
    }
    let picker = cx.new(|cx| ThemePicker::new(window, cx));
    self.theme_picker_subscription =
      Some(cx.subscribe_in(&picker, window, |this, _, _: &ThemePickerEvent, window, cx| {
        this.close_theme_picker(window, cx);
      }));
    self.theme_picker = Some(picker);
    cx.notify();
  }

  fn close_theme_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.theme_picker = None;
    self.theme_picker_subscription = None;
    window.focus(&self.focus, cx);
    cx.notify();
  }

  fn open_ui_font_picker(&mut self, _: &UiFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Ui, window, cx);
  }

  fn open_code_font_picker(&mut self, _: &CodeFont, window: &mut Window, cx: &mut Context<Self>) {
    self.open_font_picker(FontSlot::Code, window, cx);
  }

  fn open_font_picker(&mut self, slot: FontSlot, window: &mut Window, cx: &mut Context<Self>) {
    self.close_theme_picker(window, cx);
    if let Some(picker) = &self.font_picker {
      if picker.read(cx).slot() == slot {
        picker.update(cx, |picker, cx| picker.focus(window, cx));
        return;
      }
      picker.update(cx, |picker, cx| picker.finish(window, cx));
    }
    let picker = cx.new(|cx| FontPicker::new(slot, window, cx));
    self.font_picker_subscription =
      Some(cx.subscribe_in(&picker, window, |this, _, _: &FontPickerEvent, window, cx| {
        this.close_font_picker(window, cx);
      }));
    self.font_picker = Some(picker);
    cx.notify();
  }

  fn close_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.font_picker = None;
    self.font_picker_subscription = None;
    window.focus(&self.focus, cx);
    cx.notify();
  }

  fn dismiss_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(picker) = self.font_picker.take() {
      picker.update(cx, |picker, cx| picker.finish(window, cx));
    }
    self.font_picker_subscription = None;
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
      .on_action(cx.listener(Self::open_theme_picker))
      .on_action(cx.listener(Self::open_ui_font_picker))
      .on_action(cx.listener(Self::open_code_font_picker))
      .on_drag_move(cx.listener(Self::on_external_drag))
      .on_drop(cx.listener(Self::on_external_drop))
      .drag_over::<ExternalPaths>(|style, _, _, cx| external_paths_ring(style, cx))
      .relative()
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
      .children(self.theme_picker.clone().map(IntoElement::into_any_element))
      .children(self.font_picker.clone().map(IntoElement::into_any_element))
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::rc::Rc;

  use gpui_kit::component::theme::Theme;
  use gpui_kit::test::TestWindowExt as _;
  use gpui_kit::{KeyBinding, TestAppContext, WindowAppearance};

  use openit_core::settings::Settings;

  use crate::actions::{CodeFont, ColorTheme, OpenFile, UiFont};

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

  #[gpui_kit::test]
  fn color_theme_opens_and_escape_closes_it(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-t", ColorTheme, None)]));
    let (view, cx) = cx.add_window_view(EmptyView::new);

    cx.simulate_keystrokes("cmd-k cmd-t");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.theme_picker.is_some()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.theme_picker.is_none()));
  }

  #[gpui_kit::test]
  fn ui_font_chord_opens_and_escape_closes_it(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-u", UiFont, None)]));
    let (view, cx) = cx.add_window_view(EmptyView::new);

    cx.simulate_keystrokes("cmd-k cmd-u");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, cx| {
      view
        .font_picker
        .as_ref()
        .is_some_and(|picker| picker.read(cx).slot() == crate::font_picker::FontSlot::Ui)
    }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.font_picker.is_none()));
  }

  #[gpui_kit::test]
  fn code_font_chord_opens_and_escape_closes_it(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-k cmd-c", CodeFont, None)]));
    let (view, cx) = cx.add_window_view(EmptyView::new);

    cx.simulate_keystrokes("cmd-k cmd-c");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, cx| {
      view
        .font_picker
        .as_ref()
        .is_some_and(|picker| picker.read(cx).slot() == crate::font_picker::FontSlot::Code)
    }));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.font_picker.is_none()));
  }

  #[gpui_kit::test]
  fn ui_font_opener_opens_and_escape_closes_it(cx: &mut TestAppContext) {
    init_app(cx);
    let (view, cx) = cx.add_window_view(EmptyView::new);
    cx.update(|window, cx| {
      view.update(cx, |view, cx| {
        view.open_font_picker(crate::font_picker::FontSlot::Ui, window, cx);
      });
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.font_picker.is_some()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.font_picker.is_none()));
  }

  #[gpui_kit::test]
  fn a_window_appearance_change_reapplies_the_theme(cx: &mut TestAppContext) {
    init_app(cx);
    let (_view, cx) = cx.add_window_view(EmptyView::new);

    cx.update(|window, cx| {
      crate::theme::apply_for_appearance(WindowAppearance::Dark, Some(window), cx);
    });
    assert!(cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));
    cx.update(|window, cx| {
      crate::theme::apply_for_appearance(WindowAppearance::Light, Some(window), cx);
    });
    assert!(!cx.read_global::<Theme, _>(|theme, _| theme.mode.is_dark()));
  }
}
