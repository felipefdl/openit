//! The title bar's file name and bare icon buttons.
//!
//! gpui-component's own `Button::tooltip` reaches its tooltip overlay through
//! `Root`, and OpenIt windows do not use `Root`, so that call is silently
//! inert. The tooltip rides on a wrapper element here instead, which GPUI
//! itself shows.

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::{InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
  App, ClickEvent, FontWeight, IntoElement, MouseButton, ParentElement as _, SharedString, Styled as _, Window, div,
};

/// One bare icon in the title bar: no frame at rest, a rounded fill on hover
/// that deepens while pressed, and a tooltip naming what a click does.
pub(crate) fn toolbar_button(
  id: &'static str,
  icon: Icon,
  tip: impl Into<SharedString>,
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
    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
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
/// Hover uses the same fill as [`toolbar_button`].
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
