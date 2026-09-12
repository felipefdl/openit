//! PDF overlays: the password prompt and the go-to-page prompt.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::InteractiveElement;
use gpui_kit::{
  AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
  Subscription, Window, div, px,
};

use crate::status_pickers::overlay_frame;
use crate::theme::{ActivePalette, hsla};

/// What the password prompt decided.
pub enum PasswordPromptEvent {
  /// Try this password.
  Submit(String),
  /// Dismissed without a password.
  Cancel,
}

/// A masked field for a protected document's password.
pub struct PasswordPrompt {
  input: Entity<InputState>,
  wrong: bool,
  _subscription: Subscription,
}

impl EventEmitter<PasswordPromptEvent> for PasswordPrompt {}

impl PasswordPrompt {
  /// Open the prompt. `wrong` marks a rejected attempt.
  pub fn new(wrong: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
    input.update(cx, |state, cx| state.focus(window, cx));
    let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| match event {
      InputEvent::PressEnter { .. } => this.confirm(cx),
      InputEvent::Change => cx.notify(),
      InputEvent::Focus | InputEvent::Blur => {},
    });
    Self {
      input,
      wrong,
      _subscription: subscription,
    }
  }

  /// Move focus to the field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.input.update(cx, |state, cx| state.focus(window, cx));
  }

  fn confirm(&self, cx: &mut Context<Self>) {
    let password = self.input.read(cx).value().to_string();
    if password.is_empty() {
      cx.notify();
      return;
    }
    cx.emit(PasswordPromptEvent::Submit(password));
  }
}

impl Render for PasswordPrompt {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let hint: SharedString = if self.wrong {
      "That password did not unlock the document. Try again.".into()
    } else {
      "This PDF is password protected.".into()
    };
    let input = Input::new(&self.input).bordered(false);
    let border = hsla(palette.border);
    let muted = if self.wrong {
      cx.theme().danger
    } else {
      hsla(palette.muted_foreground)
    };
    overlay_frame(
      "pdf-password-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(PasswordPromptEvent::Cancel)),
      move |panel| {
        panel.child(div().px_3().py_2().text_size(px(15.)).child(input)).child(
          div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .text_size(px(13.))
            .text_color(muted)
            .child(hint),
        )
      },
    )
    .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| {
      cx.emit(PasswordPromptEvent::Cancel);
    }))
  }
}

/// What the go-to-page prompt decided.
pub enum GoToPageEvent {
  /// Scroll to this zero-based page.
  Jump(usize),
  /// Dismissed.
  Close,
}

/// A `page` prompt showing the current page and the page count.
pub struct GoToPage {
  input: Entity<InputState>,
  current: usize,
  page_count: usize,
  _subscription: Subscription,
}

impl EventEmitter<GoToPageEvent> for GoToPage {}

impl GoToPage {
  /// Open with the current (zero-based) page prefilled and selected.
  pub fn new(current: usize, page_count: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("page"));
    input.update(cx, |state, cx| {
      state.set_value(format!("{}", current.saturating_add(1)), window, cx);
      state.select_all(window, cx);
      state.focus(window, cx);
    });
    let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| match event {
      InputEvent::PressEnter { .. } => this.confirm(cx),
      InputEvent::Change => cx.notify(),
      InputEvent::Focus | InputEvent::Blur => {},
    });
    Self {
      input,
      current,
      page_count,
      _subscription: subscription,
    }
  }

  /// Move focus to the field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.input.update(cx, |state, cx| state.focus(window, cx));
  }

  /// Parse a one-based page number into a zero-based index inside the document.
  pub fn parse(text: &str, page_count: usize) -> Option<usize> {
    let page: usize = text.trim().parse().ok()?;
    if page == 0 || page_count == 0 {
      return None;
    }
    Some(page.min(page_count).saturating_sub(1))
  }

  fn confirm(&self, cx: &mut Context<Self>) {
    let text = self.input.read(cx).value();
    match Self::parse(&text, self.page_count) {
      Some(page) => cx.emit(GoToPageEvent::Jump(page)),
      None => cx.notify(),
    }
  }
}

impl Render for GoToPage {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let text = self.input.read(cx).value();
    let hint: SharedString = if text.trim().is_empty() || Self::parse(&text, self.page_count).is_some() {
      format!("Page {} of {}", self.current.saturating_add(1), self.page_count).into()
    } else {
      format!("Type a page between 1 and {}", self.page_count).into()
    };
    let input = Input::new(&self.input).bordered(false);
    let border = hsla(palette.border);
    let muted = hsla(palette.muted_foreground);
    overlay_frame(
      "pdf-goto-page-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(GoToPageEvent::Close)),
      move |panel| {
        panel.child(div().px_3().py_2().text_size(px(15.)).child(input)).child(
          div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .text_size(px(13.))
            .text_color(muted)
            .child(hint),
        )
      },
    )
    .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| cx.emit(GoToPageEvent::Close)))
  }
}

#[cfg(test)]
mod tests {
  use super::GoToPage;

  #[core::prelude::v1::test]
  fn parse_clamps_to_the_document() {
    assert_eq!(GoToPage::parse("1", 10), Some(0));
    assert_eq!(GoToPage::parse(" 7 ", 10), Some(6));
    assert_eq!(GoToPage::parse("99", 10), Some(9));
    assert_eq!(GoToPage::parse("0", 10), None);
    assert_eq!(GoToPage::parse("x", 10), None);
    assert_eq!(GoToPage::parse("1", 0), None);
  }
}
