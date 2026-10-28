//! The find bar: a query field, a match count, for PDFs the results list, and
//! for the editor a case toggle and a replace row.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
  AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
  Subscription, Task, Window, div, px,
};
use openit_core::pdf_text::Match;

use crate::actions::{NextMatch, PreviousMatch, ReplaceAllMatches, ReplaceMatch};
use crate::theme::{ActivePalette, hsla, overlay_border};
use crate::title_bar::toolbar_button;
use gpui_kit::component::{Icon, IconName};

/// Rows of the results list shown at once before it scrolls.
const MAX_RESULT_ROWS: usize = 8;
/// Fixed height of one result row, so the list always ends on a whole row.
const ROW_HEIGHT: f32 = 28.;
/// Fixed height of the query row.
const HEADER_HEIGHT: f32 = 48.;
/// Fixed height of the editor's replace row.
const REPLACE_ROW_HEIGHT: f32 = 40.;
/// Space kept between the card and the document edges around it.
const CARD_MARGIN: f32 = 8.;
/// gpui-kit's Replace shortcut, shown on the replace toggle.
#[cfg(target_os = "macos")]
const REPLACE_SHORTCUT: &str = "cmd-shift-f";
#[cfg(not(target_os = "macos"))]
const REPLACE_SHORTCUT: &str = "ctrl-h";
/// The card's widest size, so it reads as a panel rather than a band across the window.
const CARD_MAX_WIDTH: f32 = 640.;
/// The corner card's width over text, where it should cover as little as it can.
const CORNER_WIDTH: f32 = 440.;
/// Room kept at the right of the corner card for the document's scrollbar.
const SCROLLBAR_GUTTER: f32 = 12.;
/// How long after the last keystroke the query is searched.
const DEBOUNCE_MS: u64 = 100;

/// What the find bar decided.
pub enum FindBarEvent {
  /// The query changed.
  QueryChanged(String),
  /// Make this match the current one.
  Pick(usize),
  /// Dismissed.
  Close,
  /// The editor's case toggle changed; read it with [`FindBar::case_sensitive`].
  CaseToggled,
  /// The replace row opened or closed, so the bar's height changed.
  ReplaceToggled,
  /// Replace the current match with this text.
  Replace(String),
  /// Replace every match with this text.
  ReplaceAll(String),
}

/// The editor's extra controls.
struct Editing {
  replacement: Entity<InputState>,
  replace_open: bool,
  case_sensitive: bool,
}

/// The bar over the document: query, count, and one row per PDF match.
pub struct FindBar {
  input: Entity<InputState>,
  /// Rows of the results list. Only the PDF reader fills it.
  matches: Arc<[Match]>,
  total: usize,
  current: Option<usize>,
  query_task: Option<Task<()>>,
  /// Present only in the editor, which can replace and match case.
  editing: Option<Editing>,
  /// Sits compact in the top-right corner instead of centered and wide.
  corner: bool,
  _subscription: Subscription,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
  /// Open the bar with the field focused.
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
    input.update(cx, |state, cx| state.focus(window, cx));
    let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
      InputEvent::Change => {
        let query = input.read(cx).value().to_string();
        this.query_task = Some(cx.spawn(async move |this, cx| {
          cx.background_executor().timer(Duration::from_millis(DEBOUNCE_MS)).await;
          let _ = this.update(cx, |_, cx| cx.emit(FindBarEvent::QueryChanged(query)));
        }));
      },
      // Enter is bound to NextMatch on this key context, so the field ignores it.
      InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {},
    });
    Self {
      input,
      matches: Arc::from([]),
      total: 0,
      current: None,
      query_task: None,
      editing: None,
      corner: false,
      _subscription: subscription,
    }
  }

  /// Put the bar in the top-right corner at a compact width, the place it covers
  /// least of a text document. The PDF reader keeps the wide centered card for
  /// its results list.
  #[must_use]
  pub const fn in_corner(mut self) -> Self {
    self.corner = true;
    self
  }

  /// Open the bar for the editor: `query` prefilled and selected, the case
  /// toggle off, and the replace row open when `replace` is set. The caller
  /// searches for `query` itself; prefilling emits no [`FindBarEvent::QueryChanged`].
  pub fn for_editor(query: &str, replace: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let mut bar = Self::new(window, cx).in_corner();
    bar.input.update(cx, |state, cx| {
      state.set_value(query.to_owned(), window, cx);
      state.select_all(window, cx);
    });
    let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
    bar.editing = Some(Editing {
      replacement,
      replace_open: replace,
      case_sensitive: false,
    });
    bar
  }

  /// Move focus to the field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.input.update(cx, |state, cx| state.focus(window, cx));
  }

  /// The query as typed.
  pub fn query(&self, cx: &gpui_kit::App) -> String {
    self.input.read(cx).value().to_string()
  }

  /// Whether the editor's case toggle asks for exact case.
  pub fn case_sensitive(&self) -> bool {
    self.editing.as_ref().is_some_and(|editing| editing.case_sensitive)
  }

  /// Open the replace row and focus its field. Only the editor has one.
  pub fn open_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(editing) = self.editing.as_mut() else {
      return;
    };
    let opened = !editing.replace_open;
    editing.replace_open = true;
    editing.replacement.update(cx, |state, cx| state.focus(window, cx));
    if opened {
      cx.emit(FindBarEvent::ReplaceToggled);
    }
    cx.notify();
  }

  fn toggle_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(editing) = self.editing.as_mut() else {
      return;
    };
    editing.replace_open = !editing.replace_open;
    if editing.replace_open {
      editing.replacement.update(cx, |state, cx| state.focus(window, cx));
    } else {
      self.input.update(cx, |state, cx| state.focus(window, cx));
    }
    cx.emit(FindBarEvent::ReplaceToggled);
    cx.notify();
  }

  pub(crate) fn toggle_case(&mut self, cx: &mut Context<Self>) {
    let Some(editing) = self.editing.as_mut() else {
      return;
    };
    editing.case_sensitive = !editing.case_sensitive;
    cx.emit(FindBarEvent::CaseToggled);
    cx.notify();
  }

  fn replacement(&self, cx: &gpui_kit::App) -> Option<String> {
    let editing = self.editing.as_ref().filter(|editing| editing.replace_open)?;
    Some(editing.replacement.read(cx).value().to_string())
  }

  /// The space the bar takes now: [`panel_height`] for its results, plus the
  /// replace row while it is open.
  pub fn height(&self) -> f32 {
    let replace = self.editing.as_ref().is_some_and(|editing| editing.replace_open);
    panel_height(self.matches.len()) + if replace { REPLACE_ROW_HEIGHT + 1. } else { 0. }
  }

  /// Replace the results list and the current match.
  pub fn set_results(&mut self, matches: Arc<[Match]>, current: Option<usize>, cx: &mut Context<Self>) {
    self.total = matches.len();
    self.matches = matches;
    self.current = current;
    cx.notify();
  }

  /// Replace the match count and the current match, with no results list.
  pub fn set_count(&mut self, total: usize, current: Option<usize>, cx: &mut Context<Self>) {
    if self.total == total && self.current == current {
      return;
    }
    self.total = total;
    self.current = current;
    cx.notify();
  }
}

/// `"3 of 41"`, `"No matches"`, or nothing while the query is empty.
#[must_use]
pub fn count_label(total: usize, current: Option<usize>, query_empty: bool) -> String {
  if query_empty {
    return String::new();
  }
  if total == 0 {
    return "No matches".to_owned();
  }
  let position = current.unwrap_or(0).saturating_add(1);
  format!("{position} of {total}")
}

impl FindBar {
  /// One fixed-height row per visible match.
  fn result_rows(
    &self,
    palette: &openit_core::theme::UiPalette,
    cx: &Context<Self>,
  ) -> Vec<gpui_kit::Stateful<gpui_kit::Div>> {
    let muted = hsla(palette.muted_foreground);
    let current = self.current;
    visible_row_range(self.matches.len(), current)
      .filter_map(|index| self.matches.get(index).map(|hit| (index, hit)))
      .map(|(index, hit)| {
        let selected = current == Some(index);
        div()
          .id(("find-row", index))
          .flex()
          .items_center()
          .gap_2()
          .px_3()
          .h(px(ROW_HEIGHT))
          .flex_shrink_0()
          .cursor_pointer()
          .when(selected, |row| row.bg(hsla(palette.list_active)))
          .hover(|row| row.bg(hsla(palette.list_hover)))
          .child(
            div()
              .flex_shrink_0()
              .w(px(52.))
              .text_color(muted)
              .child(format!("p. {}", hit.start.page.saturating_add(1))),
          )
          .child(
            div()
              .flex_1()
              .min_w_0()
              .overflow_hidden()
              .text_ellipsis()
              .whitespace_nowrap()
              .child(SharedString::from(hit.context.as_str())),
          )
          .on_click(cx.listener(move |_, _, _, cx| cx.emit(FindBarEvent::Pick(index))))
      })
      .collect()
  }
}

impl FindBar {
  /// The query row: the editor's replace toggle, the field, the editor's case
  /// toggle, previous/next, the count, and Done.
  fn header(&self, palette: &openit_core::theme::UiPalette, cx: &Context<Self>) -> gpui_kit::Div {
    let query = self.input.read(cx).value().to_string();
    let label: SharedString = count_label(self.total, self.current, query.trim().is_empty()).into();
    let toggled = hsla(palette.list_active);
    let editing = self
      .editing
      .as_ref()
      .map(|editing| (editing.replace_open, editing.case_sensitive));
    div()
      .flex()
      .items_center()
      .gap_2()
      .px_3()
      .h(px(HEADER_HEIGHT))
      .when_some(editing, |row, (replace_open, _)| {
        row.child(toggle(
          replace_open,
          toggled,
          toolbar_button(
            "find-toggle-replace",
            Icon::new(IconName::Replace),
            "Toggle Replace",
            Some(REPLACE_SHORTCUT),
            cx,
            cx.listener(|this, _, window, cx| this.toggle_replace(window, cx)),
          ),
        ))
      })
      .child(
        div()
          .flex_1()
          .min_w_0()
          .text_size(px(14.))
          .child(Input::new(&self.input).bordered(false)),
      )
      .when_some(editing, |row, (_, case_sensitive)| {
        row.child(toggle(
          case_sensitive,
          toggled,
          toolbar_button(
            "find-match-case",
            Icon::new(IconName::CaseSensitive),
            "Match Case",
            None,
            cx,
            cx.listener(|this, _, _, cx| this.toggle_case(cx)),
          ),
        ))
      })
      .child(toolbar_button(
        "find-previous",
        Icon::new(IconName::ChevronUp),
        "Previous Match",
        Some("shift-enter"),
        cx,
        |_, window, cx| window.dispatch_action(Box::new(PreviousMatch), cx),
      ))
      .child(toolbar_button(
        "find-next",
        Icon::new(IconName::ChevronDown),
        "Next Match",
        Some("enter"),
        cx,
        |_, window, cx| window.dispatch_action(Box::new(NextMatch), cx),
      ))
      .child(
        div()
          .flex_shrink_0()
          .text_size(px(13.))
          .text_color(hsla(palette.muted_foreground))
          .child(label),
      )
      .child(text_button(
        "find-close",
        "Done",
        palette,
        cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Close)),
      ))
  }

  /// The editor's replace row, while it is open.
  fn replace_row(&self, palette: &openit_core::theme::UiPalette) -> Option<gpui_kit::Div> {
    let editing = self.editing.as_ref().filter(|editing| editing.replace_open)?;
    Some(
      div()
        .key_context("FindReplace")
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .h(px(REPLACE_ROW_HEIGHT))
        .border_t_1()
        .border_color(overlay_border(palette))
        .child(
          div()
            .flex_1()
            .min_w_0()
            .text_size(px(14.))
            .child(Input::new(&editing.replacement).bordered(false)),
        )
        .child(text_button("find-replace", "Replace", palette, |_, window, cx| {
          window.dispatch_action(Box::new(ReplaceMatch), cx);
        }))
        .child(text_button("find-replace-all", "Replace All", palette, |_, window, cx| {
          window.dispatch_action(Box::new(ReplaceAllMatches), cx);
        })),
    )
  }
}

impl Render for FindBar {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let border = overlay_border(&palette);
    let rows = self.result_rows(&palette, cx);
    let rows_len = rows.len();
    let corner = self.corner;
    // A transparent frame that only positions the card: it handles no mouse
    // input, so the margins around the card stay part of the document.
    div()
      .flex()
      .p(px(CARD_MARGIN))
      .map(|frame| {
        if corner {
          frame.justify_end().pr(px(CARD_MARGIN + SCROLLBAR_GUTTER))
        } else {
          frame.justify_center()
        }
      })
      .child(
        div()
        .key_context("FindBar")
        .occlude()
        .flex()
        .flex_col()
        .w_full()
        .max_w(px(if corner { CORNER_WIDTH } else { CARD_MAX_WIDTH }))
        .overflow_hidden()
        .rounded_lg()
        .bg(hsla(palette.sidebar))
        .border_1()
        .border_color(border)
        .shadow_lg()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        // Up and Down in the field walk the matches, like the results list beneath it.
        .capture_action(cx.listener(|_, _: &MoveUp, window, cx| {
          cx.stop_propagation();
          window.dispatch_action(Box::new(PreviousMatch), cx);
        }))
        .capture_action(cx.listener(|_, _: &MoveDown, window, cx| {
          cx.stop_propagation();
          window.dispatch_action(Box::new(NextMatch), cx);
        }))
        .on_action(cx.listener(|this, _: &ReplaceMatch, _, cx| {
          if let Some(replacement) = this.replacement(cx) {
            cx.emit(FindBarEvent::Replace(replacement));
          }
        }))
        .on_action(cx.listener(|this, _: &ReplaceAllMatches, _, cx| {
          if let Some(replacement) = this.replacement(cx) {
            cx.emit(FindBarEvent::ReplaceAll(replacement));
          }
        }))
        .child(self.header(&palette, cx))
        .children(self.replace_row(&palette))
        .when(rows_len > 0, |bar| {
          bar.child(
            div()
              .id("find-results")
              .flex()
              .flex_col()
              .max_h(px(list_height(rows_len)))
              .overflow_y_scroll()
              .border_t_1()
              .border_color(border)
              .text_size(px(13.))
              .children(rows),
          )
        })
        .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| cx.emit(FindBarEvent::Close))),
      )
  }
}

/// A toolbar button that shows a resting fill while `on`.
fn toggle(on: bool, fill: gpui_kit::Hsla, button: impl IntoElement) -> impl IntoElement {
  div().flex_shrink_0().rounded_md().when(on, |wrap| wrap.bg(fill)).child(button)
}

/// A quiet text button in the bar: muted label, fill on hover.
fn text_button(
  id: &'static str,
  label: &'static str,
  palette: &openit_core::theme::UiPalette,
  on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
  div()
    .id(id)
    .flex_shrink_0()
    .px_2()
    .rounded_sm()
    .cursor_pointer()
    .text_size(px(13.))
    .text_color(hsla(palette.muted_foreground))
    .hover(|button| button.bg(hsla(palette.list_hover)))
    .child(label)
    .on_click(on_click)
}

/// The space the bar takes for `matches` results: the card's margins and
/// border, its header, and the list when there are rows. Rows and header are
/// fixed-height, so this is exact.
#[must_use]
pub fn panel_height(matches: usize) -> f32 {
  let list = if matches == 0 {
    0.
  } else {
    list_height(matches) + 1.
  };
  CARD_MARGIN.mul_add(2., HEADER_HEIGHT + 2. + list)
}

/// Height of the results list: whole rows, at most [`MAX_RESULT_ROWS`].
fn list_height(matches: usize) -> f32 {
  let rows = matches.min(MAX_RESULT_ROWS);
  f32::from(u16::try_from(rows).unwrap_or(u16::MAX)) * ROW_HEIGHT
}

/// The slice of matches drawn in the results list, clustered on the current hit.
fn visible_row_range(total: usize, current: Option<usize>) -> Range<usize> {
  if total <= MAX_RESULT_ROWS {
    return 0..total;
  }
  let current = current.unwrap_or(0).min(total.saturating_sub(1));
  let before = MAX_RESULT_ROWS.saturating_sub(1) / 2;
  let start = current.saturating_sub(before).min(total.saturating_sub(MAX_RESULT_ROWS));
  start..start.saturating_add(MAX_RESULT_ROWS).min(total)
}

#[cfg(test)]
mod tests {
  use super::count_label;

  #[core::prelude::v1::test]
  fn count_label_reads_like_a_finder() {
    assert_eq!(count_label(41, Some(2), false), "3 of 41");
    assert_eq!(count_label(1, Some(0), false), "1 of 1");
    assert_eq!(count_label(0, None, false), "No matches");
    assert_eq!(count_label(0, None, true), "");
  }
}
