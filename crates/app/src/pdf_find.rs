//! The PDF find bar: a query field, a match count, and the results list.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
  AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
  Subscription, Task, Window, div, px,
};
use openit_core::pdf_text::Match;

use crate::theme::{ActivePalette, hsla};

/// Rows of the results list shown at once before it scrolls.
const MAX_RESULT_ROWS: usize = 8;
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
}

/// The bar over the pages: query, count, and one row per match.
pub struct FindBar {
  input: Entity<InputState>,
  matches: Arc<[Match]>,
  current: Option<usize>,
  query_task: Option<Task<()>>,
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
      current: None,
      query_task: None,
      _subscription: subscription,
    }
  }

  /// Move focus to the field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.input.update(cx, |state, cx| state.focus(window, cx));
  }

  /// The query as typed.
  pub fn query(&self, cx: &gpui_kit::App) -> String {
    self.input.read(cx).value().to_string()
  }

  /// Replace the results and the current match.
  pub fn set_results(&mut self, matches: Arc<[Match]>, current: Option<usize>, cx: &mut Context<Self>) {
    self.matches = matches;
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

impl Render for FindBar {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let query = self.input.read(cx).value().to_string();
    let label: SharedString = count_label(self.matches.len(), self.current, query.trim().is_empty()).into();
    let input = Input::new(&self.input).bordered(false);
    let border = hsla(palette.border);
    let muted = hsla(palette.muted_foreground);
    let current = self.current;
    let range = visible_row_range(self.matches.len(), current);
    let rows: Vec<_> = range
      .filter_map(|index| self.matches.get(index).map(|hit| (index, hit)))
      .map(|(index, hit)| {
        let selected = current == Some(index);
        div()
          .id(("pdf-find-row", index))
          .flex()
          .items_baseline()
          .gap_2()
          .px_3()
          .py_1()
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
      .collect();
    div()
      .key_context("PdfFindBar")
      .occlude()
      .absolute()
      .top_0()
      .left_0()
      .right_0()
      .flex()
      .flex_col()
      .bg(hsla(palette.sidebar))
      .border_b_1()
      .border_color(border)
      .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .child(
        div()
          .flex()
          .items_center()
          .gap_3()
          .px_3()
          .py_2()
          .child(div().flex_1().min_w_0().text_size(px(14.)).child(input))
          .child(div().flex_shrink_0().text_size(px(13.)).text_color(muted).child(label))
          .child(
            div()
              .id("pdf-find-close")
              .flex_shrink_0()
              .px_2()
              .rounded_sm()
              .cursor_pointer()
              .text_size(px(13.))
              .text_color(muted)
              .hover(|button| button.bg(hsla(palette.list_hover)))
              .child("Done")
              .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Close))),
          ),
      )
      .when(!rows.is_empty(), |bar| {
        bar.child(
          div()
            .id("pdf-find-results")
            .flex()
            .flex_col()
            .max_h(px(28. * row_count(rows.len())))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(border)
            .text_size(px(13.))
            .children(rows),
        )
      })
      .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| cx.emit(FindBarEvent::Close)))
  }
}

/// How many rows the list shows before it scrolls.
fn row_count(rows: usize) -> f32 {
  f32::from(u16::try_from(rows.min(MAX_RESULT_ROWS)).unwrap_or(u16::MAX))
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
