//! The title bar's file name and bare icon buttons.
//!
//! gpui-component's own `Button::tooltip` reaches its tooltip overlay through
//! `Root`, and OpenIt windows do not use `Root`, so that call is silently
//! inert. The tooltip rides on a wrapper element here instead, which GPUI
//! itself shows.

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::{InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
  AnyView, App, ClickEvent, FontWeight, IntoElement, Keystroke, MouseButton, ParentElement as _, SharedString,
  Styled as _, Window, div,
};

/// A tooltip naming what a click does, with its shortcut drawn the platform's
/// way (`⇧⌘E` on macOS, `Ctrl+Shift+E` elsewhere). `shortcut` uses GPUI
/// keystroke syntax, where `secondary` is Cmd on macOS and Ctrl elsewhere.
pub(crate) fn tooltip(
  text: SharedString,
  shortcut: Option<&'static str>,
  window: &mut Window,
  cx: &mut App,
) -> AnyView {
  let key = shortcut.and_then(|keys| Keystroke::parse(keys).ok()).map(Kbd::new);
  Tooltip::new(text).key_binding(key).build(window, cx)
}

/// One bare icon in the title bar: no frame at rest, a rounded fill on hover
/// that deepens while pressed, and a tooltip naming what a click does.
pub(crate) fn toolbar_button(
  id: &'static str,
  icon: Icon,
  tip: impl Into<SharedString>,
  shortcut: Option<&'static str>,
  cx: &App,
  on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
  let theme = cx.theme();
  let variant = ButtonCustomVariant::new(cx)
    .color(theme.transparent)
    .hover(theme.accent)
    .active(theme.accent.opacity(0.7))
    .foreground(theme.foreground);
  let tip: SharedString = tip.into();
  div()
    .id(id)
    .flex()
    .tooltip(move |window, cx| tooltip(tip.clone(), shortcut, window, cx))
    .on_click(on_click)
    .child(
      Button::new(SharedString::new_static(id))
        .custom(variant)
        .small()
        .icon(icon)
        .rounded_md(),
    )
}

/// The document name in the title bar, with a dirty marker and a chevron.
/// Hover uses the same fill as [`toolbar_button`]; a click lists nearby files.
pub(crate) fn file_name(
  title: impl Into<SharedString>,
  dirty: bool,
  cx: &App,
  on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
  let theme = cx.theme();
  let accent = theme.accent;
  let pressed = accent.opacity(0.7);
  let marker = if dirty { " \u{2022}" } else { "" };
  let title: SharedString = title.into();
  let label = format!("{title}{marker}");
  div().flex_1().min_w_0().flex().items_center().child(
    div()
      .id("file-name")
      .flex()
      .min_w_0()
      .items_center()
      .gap_1()
      .px_1()
      .rounded_md()
      .cursor_pointer()
      .hover(move |style| style.bg(accent))
      .active(move |style| style.bg(pressed))
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .tooltip(|window, cx| tooltip("Go to File".into(), Some("secondary-p"), window, cx))
      .on_click(on_click)
      .child(
        div()
          .min_w_0()
          .overflow_hidden()
          .text_ellipsis()
          .whitespace_nowrap()
          .text_sm()
          .font_weight(FontWeight::SEMIBOLD)
          .child(label),
      )
      .child(
        Icon::new(IconName::ChevronDown)
          .small()
          .text_color(theme.muted_foreground)
          .flex_shrink_0(),
      ),
  )
}
