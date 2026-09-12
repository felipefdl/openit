//! Font picker: a command palette that previews the highlighted family and writes one `[font]` key
//! on confirm.

use std::time::Duration;

use gpui_kit::base::actions::{SelectDown, SelectUp};
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::{
  App, AppContext, Context, Entity, EventEmitter, IntoElement, MouseButton, Render, SharedString, Task, Window, div, px,
};

use crate::settings::SettingsStore;
use crate::theme::{ActivePalette, hsla};

const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(80);

/// Which settings key and Theme field the picker writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSlot {
  /// `font.ui` / `Theme.font_family`.
  Ui,
  /// `font.code` / `Theme.mono_font_family`.
  Code,
}

impl FontSlot {
  const fn placeholder(self) -> &'static str {
    match self {
      Self::Ui => "Select UI Font",
      Self::Code => "Select Code Font",
    }
  }
}

/// Close the overlay.
pub enum FontPickerEvent {
  Close,
}

/// Whether the picker has settled and how.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
  Open,
  Committed,
  Cancelled,
}

/// Font family command palette. One type, two slots.
pub struct FontPicker {
  slot: FontSlot,
  state: Entity<CommandState>,
  names: Vec<String>,
  query: String,
  outcome: Outcome,
  needs_initial_selection: bool,
  previewed: Option<String>,
  preview_task: Option<Task<()>>,
  saved_ui: SharedString,
  saved_code: SharedString,
}

impl EventEmitter<FontPickerEvent> for FontPicker {}

impl FontPicker {
  /// Open the picker on the current family for `slot`, with search focused.
  pub fn new(slot: FontSlot, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let names = cx.text_system().all_font_names();
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| state.focus(window, cx));
    let previewed = Some(current_name(slot, cx).to_string());
    let theme = Theme::global(cx);
    Self {
      slot,
      state,
      names,
      query: String::new(),
      outcome: Outcome::Open,
      needs_initial_selection: true,
      previewed,
      preview_task: None,
      saved_ui: theme.font_family.clone(),
      saved_code: theme.mono_font_family.clone(),
    }
  }

  /// Which slot this opener writes.
  pub const fn slot(&self) -> FontSlot {
    self.slot
  }

  /// Move focus to the search field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.focus(window, cx));
  }

  /// Restore the saved pair unless the user confirmed a pick.
  pub fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.preview_task = None;
    match self.outcome {
      Outcome::Open => {
        self.outcome = Outcome::Cancelled;
        let theme = Theme::global_mut(cx);
        theme.font_family = self.saved_ui.clone();
        theme.mono_font_family = self.saved_code.clone();
        Theme::sync_base(cx);
        window.refresh();
      },
      Outcome::Committed | Outcome::Cancelled => {},
    }
  }

  fn filtered(&self) -> Vec<&str> {
    let needle = self.query.trim().to_lowercase();
    self
      .names
      .iter()
      .map(String::as_str)
      .filter(|name| needle.is_empty() || name.to_lowercase().contains(&needle))
      .collect()
  }

  fn name_at(&self, path: IndexPath) -> Option<String> {
    self.filtered().get(path.row).map(|name| (*name).to_owned())
  }

  fn preview(slot: FontSlot, name: &str, window: &mut Window, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    match slot {
      FontSlot::Ui => theme.font_family = SharedString::from(name),
      FontSlot::Code => theme.mono_font_family = SharedString::from(name),
    }
    Theme::sync_base(cx);
    window.refresh();
  }

  fn preview_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(path) = self.state.read(cx).selected_index() else {
      return;
    };
    let Some(name) = self.name_at(path) else {
      return;
    };
    if self.previewed.as_deref() == Some(name.as_str()) {
      return;
    }
    self.previewed = Some(name.clone());
    Self::preview(self.slot, &name, window, cx);
  }

  fn schedule_preview(&mut self, cx: &Context<Self>) {
    self.preview_task = Some(cx.spawn(async move |this, cx| {
      cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
      let _ = this.update_in(cx, Self::preview_selected);
    }));
  }

  fn on_query_change(&mut self, query: &str, _: &mut Window, cx: &mut Context<Self>) {
    self.query = query.to_string();
    cx.notify();
  }

  fn confirm(&mut self, path: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
    let Some(name) = self.name_at(path) else {
      return;
    };
    self.outcome = Outcome::Committed;
    self.preview_task = None;
    self.previewed = Some(name.clone());
    Self::preview(self.slot, &name, window, cx);
    let slot = self.slot;
    SettingsStore::update(cx, |settings| match slot {
      FontSlot::Ui => settings.font.ui = Some(name),
      FontSlot::Code => settings.font.code = Some(name),
    });
    self.close(window, cx);
  }

  fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.finish(window, cx);
    cx.emit(FontPickerEvent::Close);
  }

  #[cfg(test)]
  fn names(&self) -> &[String] {
    &self.names
  }

  #[cfg(test)]
  fn preview_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
    self.previewed = Some(name.to_owned());
    Self::preview(self.slot, name, window, cx);
  }
}

fn current_name(slot: FontSlot, cx: &App) -> SharedString {
  let theme = Theme::global(cx);
  match slot {
    FontSlot::Ui => theme.font_family.clone(),
    FontSlot::Code => theme.mono_font_family.clone(),
  }
}

impl Render for FontPicker {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    if self.needs_initial_selection {
      self.needs_initial_selection = false;
      let current = current_name(self.slot, cx);
      if let Some(row) = self.filtered().iter().position(|name| *name == current.as_ref()) {
        let state = self.state.clone();
        window.on_next_frame(move |window, cx| {
          state.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(row)), window, cx);
          });
        });
      }
    }
    let palette = cx.global::<ActivePalette>().0;
    let this = cx.entity().downgrade();
    let on_query = this.clone();
    let on_confirm = this.clone();
    let on_cancel = this;
    let items: Vec<CommandItem> = self
      .filtered()
      .iter()
      .map(|name| CommandItem::new().label((*name).to_owned()))
      .collect();
    let command = Command::new(&self.state)
      .filterable(false)
      .placeholder(self.slot.placeholder())
      .max_h(px(440.))
      .bordered(false)
      .w_full()
      .bg(hsla(palette.sidebar))
      .text_size(px(13.))
      .on_query(move |query, window, cx| {
        let query = query.to_string();
        let _ = on_query.update(cx, |this, cx| this.on_query_change(&query, window, cx));
      })
      .on_confirm(move |index, window, cx| {
        let _ = on_confirm.update(cx, |this, cx| this.confirm(index, window, cx));
      })
      .on_cancel(move |window, cx| {
        let _ = on_cancel.update(cx, |this, cx| this.close(window, cx));
      })
      .group(CommandGroup::new().items(items));
    div()
      .id("font-picker-backdrop")
      .absolute()
      .inset_0()
      .flex()
      .justify_center()
      .items_start()
      .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
      .child(
        div()
          .key_context("Dialog")
          .occlude()
          .mt(px(60.))
          .w(px(500.))
          .overflow_hidden()
          .bg(hsla(palette.sidebar))
          .border_1()
          .border_color(hsla(palette.border))
          .rounded_lg()
          .shadow_lg()
          .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
          .capture_action(cx.listener(|this, _: &SelectUp, _, cx| this.schedule_preview(cx)))
          .capture_action(cx.listener(|this, _: &SelectDown, _, cx| this.schedule_preview(cx)))
          .child(command),
      )
  }
}

#[cfg(test)]
mod tests {
  use gpui_kit::TestAppContext;
  use gpui_kit::component::IndexPath;
  use gpui_kit::component::theme::Theme;

  use openit_core::settings::Settings;

  use crate::settings::{AppSettings, SettingsStore};
  use crate::theme::ThemeDirs;

  use super::{FontPicker, FontSlot};

  fn init_app(cx: &TestAppContext) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(SettingsStore::new(None));
      cx.set_global(ThemeDirs::default());
      crate::theme::init(cx);
    });
  }

  fn set_saved_fonts(ui: &str, code: &str, cx: &TestAppContext) {
    cx.update(|cx| {
      SettingsStore::update(cx, |settings| {
        settings.font.ui = Some(ui.to_owned());
        settings.font.code = Some(code.to_owned());
      });
    });
  }

  #[gpui_kit::test]
  fn ui_font_opener_lists_all_font_names_and_enter_writes_ui_only(cx: &mut TestAppContext) {
    init_app(cx);
    set_saved_fonts("KeepUi", "KeepCode", cx);
    let (picker, cx) = cx.add_window_view(|window, cx| FontPicker::new(FontSlot::Ui, window, cx));
    let names = cx.update(|_, cx| cx.text_system().all_font_names());
    picker.read_with(cx, |picker, _| assert_eq!(picker.names(), names.as_slice()));
    assert!(!names.is_empty(), "the text system must list at least one family");

    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.confirm(IndexPath::new(0), window, cx)));
    cx.run_until_parked();
    let family = names[0].clone();
    cx.update(|_, cx| {
      let font = &cx.global::<AppSettings>().0.font;
      assert_eq!(font.ui.as_deref(), Some(family.as_str()));
      assert_eq!(font.code.as_deref(), Some("KeepCode"));
    });
  }

  #[gpui_kit::test]
  fn code_font_opener_lists_all_font_names_and_enter_writes_code_only(cx: &mut TestAppContext) {
    init_app(cx);
    set_saved_fonts("KeepUi", "KeepCode", cx);
    let (picker, cx) = cx.add_window_view(|window, cx| FontPicker::new(FontSlot::Code, window, cx));
    let names = cx.update(|_, cx| cx.text_system().all_font_names());
    picker.read_with(cx, |picker, _| assert_eq!(picker.names(), names.as_slice()));
    assert!(!names.is_empty(), "the text system must list at least one family");

    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.confirm(IndexPath::new(0), window, cx)));
    cx.run_until_parked();
    let family = names[0].clone();
    cx.update(|_, cx| {
      let font = &cx.global::<AppSettings>().0.font;
      assert_eq!(font.ui.as_deref(), Some("KeepUi"));
      assert_eq!(font.code.as_deref(), Some(family.as_str()));
    });
  }

  #[gpui_kit::test]
  fn moving_the_highlight_previews_the_matching_field_and_escape_restores_the_pair(cx: &mut TestAppContext) {
    init_app(cx);
    let (picker, cx) = cx.add_window_view(|window, cx| FontPicker::new(FontSlot::Ui, window, cx));
    let names = picker.read_with(cx, |picker, _| picker.names().to_vec());
    let (saved_ui, saved_code) = cx.update(|_, cx| {
      let theme = Theme::global(cx);
      (theme.font_family.clone(), theme.mono_font_family.clone())
    });
    let preview = names
      .iter()
      .find(|name| *name != saved_ui.as_ref())
      .cloned()
      .or_else(|| names.first().cloned())
      .expect("the text system must list at least one family");

    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.preview_name(&preview, window, cx)));
    cx.update(|_, cx| {
      let theme = Theme::global(cx);
      assert_eq!(theme.font_family.as_ref(), preview.as_str());
      assert_eq!(theme.mono_font_family, saved_code);
      assert_eq!(
        gpui_kit::base::Theme::global(cx).tokens.typography.sans.as_ref(),
        preview.as_str(),
      );
    });

    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.close(window, cx)));
    cx.run_until_parked();
    cx.update(|_, cx| {
      let theme = Theme::global(cx);
      assert_eq!(theme.font_family, saved_ui);
      assert_eq!(theme.mono_font_family, saved_code);
      assert_eq!(
        gpui_kit::base::Theme::global(cx).tokens.typography.sans.as_ref(),
        saved_ui.as_ref(),
      );
    });
  }

  #[gpui_kit::test]
  fn code_highlight_previews_mono_only(cx: &mut TestAppContext) {
    init_app(cx);
    let (picker, cx) = cx.add_window_view(|window, cx| FontPicker::new(FontSlot::Code, window, cx));
    let names = picker.read_with(cx, |picker, _| picker.names().to_vec());
    let (saved_ui, saved_code) = cx.update(|_, cx| {
      let theme = Theme::global(cx);
      (theme.font_family.clone(), theme.mono_font_family.clone())
    });
    let preview = names
      .iter()
      .find(|name| *name != saved_code.as_ref())
      .cloned()
      .or_else(|| names.first().cloned())
      .expect("the text system must list at least one family");

    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.preview_name(&preview, window, cx)));
    cx.update(|_, cx| {
      let theme = Theme::global(cx);
      assert_eq!(theme.font_family, saved_ui);
      assert_eq!(theme.mono_font_family.as_ref(), preview.as_str());
    });
  }
}
