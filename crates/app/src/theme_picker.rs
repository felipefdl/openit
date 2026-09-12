//! Color theme picker: a command palette that previews the highlighted theme and writes the pick to
//! settings on confirm.

use gpui_kit::base::actions::{SelectDown, SelectUp};
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::prelude::*;
use gpui_kit::{
  App, AppContext, Context, Entity, EventEmitter, IntoElement, MouseButton, Render, Window, WindowAppearance, div, px,
};
use openit_core::settings::ThemeMode;
use openit_core::theme::ThemeKind;

use crate::settings::{AppSettings, SettingsStore};
use crate::theme::{ActivePalette, ThemeCatalog, ThemeEntry, apply_for_appearance, apply_theme, hsla};

const PLACEHOLDER: &str = "Select Color Theme";
const DARK_THEMES: &str = "dark themes";
const LIGHT_THEMES: &str = "light themes";

/// Filter by label substring and put the OS-preferred kind first.
fn grouped(entries: &[ThemeEntry], query: &str, os_dark: bool) -> (Vec<ThemeEntry>, Vec<ThemeEntry>) {
  let needle = query.trim().to_lowercase();
  let matches = |entry: &ThemeEntry| needle.is_empty() || entry.label.to_lowercase().contains(&needle);
  let of = |kind: ThemeKind| -> Vec<ThemeEntry> {
    entries
      .iter()
      .filter(|entry| entry.kind == kind && matches(entry))
      .cloned()
      .collect()
  };
  let (dark, light) = (of(ThemeKind::Dark), of(ThemeKind::Light));
  if os_dark {
    (dark, light)
  } else {
    (light, dark)
  }
}

fn index_path_of(first: &[ThemeEntry], second: &[ThemeEntry], id: &str) -> Option<IndexPath> {
  let mut section = 0;
  for group in [first, second] {
    if group.is_empty() {
      continue;
    }
    if let Some(row) = group.iter().position(|entry| entry.id == id) {
      return Some(IndexPath::new(row).section(section));
    }
    section += 1;
  }
  None
}

/// The id in effect for the current mode and appearance.
fn current_id(cx: &App) -> String {
  let settings = &cx.global::<AppSettings>().0.theme;
  let os_dark = matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
  let dark = match settings.mode {
    ThemeMode::System => os_dark,
    ThemeMode::Dark => true,
    ThemeMode::Light => false,
  };
  if dark {
    settings.dark.clone()
  } else {
    settings.light.clone()
  }
}

/// Close the overlay.
pub enum ThemePickerEvent {
  Close,
}

/// Whether the picker has settled and how.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
  Open,
  Committed,
  Cancelled,
}

/// Color theme command palette.
pub struct ThemePicker {
  state: Entity<CommandState>,
  entries: Vec<ThemeEntry>,
  query: String,
  os_dark: bool,
  outcome: Outcome,
  needs_initial_selection: bool,
}

impl EventEmitter<ThemePickerEvent> for ThemePicker {}

impl ThemePicker {
  /// Open the picker on the current theme, with search focused.
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let os_dark = matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let entries = ThemeCatalog::get(cx).entries.clone();
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| state.focus(window, cx));
    Self {
      state,
      entries,
      query: String::new(),
      os_dark,
      outcome: Outcome::Open,
      needs_initial_selection: true,
    }
  }

  /// Move focus to the search field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.focus(window, cx));
  }

  /// Restore the configured theme unless the user confirmed a pick.
  pub fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    match self.outcome {
      Outcome::Open => {
        self.outcome = Outcome::Cancelled;
        apply_for_appearance(cx.window_appearance(), Some(window), cx);
      },
      Outcome::Committed | Outcome::Cancelled => {},
    }
  }

  fn groups(&self) -> (Vec<ThemeEntry>, Vec<ThemeEntry>) {
    grouped(&self.entries, &self.query, self.os_dark)
  }

  fn entry_at(&self, path: IndexPath) -> Option<ThemeEntry> {
    let (first, second) = self.groups();
    match (first.is_empty(), second.is_empty(), path.section) {
      (false, _, 0) => first.get(path.row).cloned(),
      (false, false, 1) | (true, false, 0) => second.get(path.row).cloned(),
      _ => None,
    }
  }

  fn preview(entry: &ThemeEntry, window: &mut Window, cx: &mut App) {
    apply_theme(&entry.id, entry.kind, Some(window), cx);
  }

  fn preview_at(&self, path: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(entry) = self.entry_at(path) {
      Self::preview(&entry, window, cx);
    }
  }

  fn schedule_preview(&self, window: &Window, cx: &mut Context<Self>) {
    let state = self.state.clone();
    let this = cx.entity().downgrade();
    window.defer(cx, move |window, cx| {
      let Some(path) = state.read(cx).selected_index() else {
        return;
      };
      let _ = this.update(cx, |this, cx| this.preview_at(path, window, cx));
    });
  }

  fn on_query_change(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
    self.query = query.to_string();
    let (first, second) = self.groups();
    if let Some(entry) = first.first().or_else(|| second.first()) {
      Self::preview(entry, window, cx);
    }
    cx.notify();
  }

  /// Persist `entry` as the preferred theme of its kind. Picking the other kind under `system`
  /// pins the mode to that kind so the choice is visible right away.
  fn confirm(&mut self, path: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
    let Some(entry) = self.entry_at(path) else {
      return;
    };
    self.outcome = Outcome::Committed;
    Self::preview(&entry, window, cx);
    let os_dark = self.os_dark;
    SettingsStore::update(cx, |settings| {
      let theme = &mut settings.theme;
      match entry.kind {
        ThemeKind::Dark => theme.dark.clone_from(&entry.id),
        ThemeKind::Light => theme.light.clone_from(&entry.id),
      }
      let kind_shown = match theme.mode {
        ThemeMode::System => {
          if os_dark {
            ThemeKind::Dark
          } else {
            ThemeKind::Light
          }
        },
        ThemeMode::Dark => ThemeKind::Dark,
        ThemeMode::Light => ThemeKind::Light,
      };
      if kind_shown != entry.kind {
        theme.mode = match entry.kind {
          ThemeKind::Dark => ThemeMode::Dark,
          ThemeKind::Light => ThemeMode::Light,
        };
      }
    });
    self.close(window, cx);
  }

  fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.finish(window, cx);
    cx.emit(ThemePickerEvent::Close);
  }
}

fn command_items(entries: &[ThemeEntry]) -> Vec<CommandItem> {
  entries
    .iter()
    .map(|entry| CommandItem::new().label(entry.label.clone()))
    .collect()
}

impl Render for ThemePicker {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    if self.needs_initial_selection {
      self.needs_initial_selection = false;
      let (first, second) = self.groups();
      if let Some(path) = index_path_of(&first, &second, &current_id(cx)) {
        let state = self.state.clone();
        window.on_next_frame(move |window, cx| {
          state.update(cx, |state, cx| {
            state.set_selected_index(Some(path), window, cx);
          });
        });
      }
    }
    let palette = cx.global::<ActivePalette>().0;
    let this = cx.entity().downgrade();
    let (first, second) = self.groups();
    let (first_label, second_label) = if self.os_dark {
      (DARK_THEMES, LIGHT_THEMES)
    } else {
      (LIGHT_THEMES, DARK_THEMES)
    };
    let on_query = this.clone();
    let on_confirm = this.clone();
    let on_cancel = this;
    let mut command = Command::new(&self.state)
      .filterable(false)
      .placeholder(PLACEHOLDER)
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
      });
    if !first.is_empty() {
      command = command.group(CommandGroup::new().label(first_label).items(command_items(&first)));
    }
    if !first.is_empty() && !second.is_empty() {
      command = command.separator();
    }
    if !second.is_empty() {
      command = command.group(CommandGroup::new().label(second_label).items(command_items(&second)));
    }
    div()
      .id("theme-picker-backdrop")
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
          .capture_action(cx.listener(|this, _: &SelectUp, window, cx| this.schedule_preview(window, cx)))
          .capture_action(cx.listener(|this, _: &SelectDown, window, cx| this.schedule_preview(window, cx)))
          .child(command),
      )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn entry(id: &str, label: &str, kind: ThemeKind) -> ThemeEntry {
    ThemeEntry { id: id.into(), label: label.into(), kind }
  }

  fn ids(entries: &[ThemeEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
  }

  #[core::prelude::v1::test]
  fn grouped_puts_the_os_kind_first_and_filters_by_label() {
    let entries = vec![
      entry("a-dark", "Alpha Dark", ThemeKind::Dark),
      entry("a-light", "Alpha Light", ThemeKind::Light),
      entry("b-dark", "Beta Dark", ThemeKind::Dark),
    ];
    let (first, second) = grouped(&entries, "", true);
    assert_eq!(ids(&first), ["a-dark", "b-dark"]);
    assert_eq!(ids(&second), ["a-light"]);
    let (first, second) = grouped(&entries, "alpha", false);
    assert_eq!(ids(&first), ["a-light"]);
    assert_eq!(ids(&second), ["a-dark"]);
  }

  #[core::prelude::v1::test]
  fn index_path_skips_empty_sections() {
    let dark = vec![entry("a-dark", "A", ThemeKind::Dark)];
    let light = vec![entry("a-light", "A", ThemeKind::Light)];
    assert_eq!(index_path_of(&dark, &light, "a-light"), Some(IndexPath::new(0).section(1)));
    assert_eq!(index_path_of(&[], &light, "a-light"), Some(IndexPath::new(0).section(0)));
    assert_eq!(index_path_of(&dark, &light, "nope"), None);
  }
}
