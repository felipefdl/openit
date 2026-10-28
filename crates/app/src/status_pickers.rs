//! Status bar overlays: the language palette, schema picker, and the go-to-line prompt.

use std::ops::Range;

use gpui_kit::base::actions::Cancel;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::highlighter::Language;
use gpui_kit::component::input::{Input, InputEvent, InputState, Position};
use gpui_kit::prelude::*;
use gpui_kit::{
  App, AppContext, Context, Entity, EventEmitter, FontWeight, IntoElement, MouseButton, Render, SharedString,
  Subscription, Window, div, px,
};

use openit_core::catalog::{self, CatalogSchema};

use crate::theme::{ActivePalette, hsla, overlay_border};

/// Grammars that only highlight embedded ranges of another language; selecting one for a whole
/// document is meaningless, so the picker does not list them.
const INJECTION_ONLY: &[&str] = &["markdown_inline", "jsdoc"];

/// Display name for a gpui-kit grammar identifier.
pub fn language_label(name: &str) -> String {
  match name {
    "text" => "Plain Text".to_owned(),
    "csharp" => "C#".to_owned(),
    "cpp" => "C++".to_owned(),
    "cmake" => "CMake".to_owned(),
    "graphql" => "GraphQL".to_owned(),
    "javascript" => "JavaScript".to_owned(),
    "typescript" => "TypeScript".to_owned(),
    "jsdoc" => "JSDoc".to_owned(),
    "markdown_inline" => "Markdown (Inline)".to_owned(),
    "proto" => "Protocol Buffers".to_owned(),
    "tsx" => "TSX".to_owned(),
    "json" | "html" | "css" | "sql" | "yaml" | "toml" | "php" | "ejs" | "erb" => name.to_ascii_uppercase(),
    other => {
      let mut chars = other.chars();
      chars
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().collect::<String>() + chars.as_str())
    },
  }
}

/// Every compiled-in grammar a user can pick, sorted by label.
pub fn languages() -> Vec<&'static str> {
  let mut names: Vec<&'static str> = Language::all()
    .map(|language| language.name())
    .filter(|name| !INJECTION_ONLY.contains(name))
    .collect();
  names.sort_by_key(|name| language_label(name).to_lowercase());
  names.dedup();
  names
}

/// What the language picker decided.
pub enum LanguagePickerEvent {
  /// A language was chosen.
  Picked(&'static str),
  /// Dismissed without a choice.
  Close,
}

/// Command palette listing the highlighting languages.
pub struct LanguagePicker {
  state: Entity<CommandState>,
  names: Vec<&'static str>,
  current: String,
  query: String,
  needs_initial_selection: bool,
}

impl EventEmitter<LanguagePickerEvent> for LanguagePicker {}

impl LanguagePicker {
  /// Open on `current`, with search focused.
  pub fn new(current: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| state.focus(window, cx));
    Self {
      state,
      names: languages(),
      current: current.to_owned(),
      query: String::new(),
      needs_initial_selection: true,
    }
  }

  /// Move focus to the search field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.focus(window, cx));
  }

  fn filtered(&self) -> Vec<&'static str> {
    let needle = self.query.trim().to_lowercase();
    self
      .names
      .iter()
      .copied()
      .filter(|name| {
        needle.is_empty() || language_label(name).to_lowercase().contains(&needle) || name.contains(&needle)
      })
      .collect()
  }

  fn confirm(&self, path: IndexPath, cx: &mut Context<Self>) {
    if let Some(name) = self.filtered().get(path.row).copied() {
      cx.emit(LanguagePickerEvent::Picked(name));
    }
  }
}

impl Render for LanguagePicker {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let names = self.filtered();
    if self.needs_initial_selection {
      self.needs_initial_selection = false;
      if let Some(row) = names.iter().position(|name| *name == self.current) {
        select_row_in_view(&self.state, row, names.len(), window);
      }
    }
    let palette = cx.global::<ActivePalette>().0;
    let this = cx.entity().downgrade();
    let on_query = this.clone();
    let on_confirm = this.clone();
    let on_cancel = this;
    let items: Vec<CommandItem> = names
      .iter()
      .map(|name| {
        let mut label = language_label(name);
        if *name == self.current {
          label.push_str(" (current)");
        }
        CommandItem::new().label(label)
      })
      .collect();
    let command = Command::new(&self.state)
      .filterable(false)
      .placeholder("Select a language...")
      .max_h(px(440.))
      .bordered(false)
      .w_full()
      .bg(hsla(palette.sidebar))
      .text_size(px(13.))
      .on_query(move |query, _, cx| {
        let query = query.to_string();
        let _ = on_query.update(cx, |this, cx| {
          this.query = query;
          cx.notify();
        });
      })
      .on_confirm(move |index, _, cx| {
        let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
      })
      .on_cancel(move |_, cx| {
        let _ = on_cancel.update(cx, |_, cx| cx.emit(LanguagePickerEvent::Close));
      })
      .group(CommandGroup::new().items(items));
    overlay_frame(
      "language-picker-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(LanguagePickerEvent::Close)),
      move |panel| panel.child(command),
    )
    .capture_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(LanguagePickerEvent::Close)))
  }
}

/// What the schema picker decided.
pub enum SchemaPickerEvent {
  /// Clear the manual pick and use automatic selection.
  Automatic,
  /// Open a native file prompt for a local schema.
  PickFile,
  /// Use this local path or URL as the manual pick.
  Picked(String),
  /// Dismissed without a choice.
  Close,
}

/// Command palette listing catalog schemas plus local file and URL picks.
pub struct SchemaPicker {
  state: Entity<CommandState>,
  current: Option<String>,
  preferred: Vec<String>,
  query: String,
  needs_initial_selection: bool,
}

impl EventEmitter<SchemaPickerEvent> for SchemaPicker {}

enum SchemaRow {
  Automatic,
  FromFile,
  Custom(String),
  Catalog {
    schema: &'static CatalogSchema,
    /// Byte range of the query inside the schema name, for highlighting.
    hit: Option<Range<usize>>,
  },
}

impl SchemaRow {
  fn value(&self) -> Option<&str> {
    match self {
      Self::Automatic | Self::FromFile => None,
      Self::Custom(value) => Some(value),
      Self::Catalog { schema, .. } => Some(schema.url.as_str()),
    }
  }
}

/// Catalog schemas matching `needle` (lower-case), best match first: exact name, name prefix,
/// name substring, other file patterns, then the URL. Ties keep catalog order, or `preferred` order
/// (the URLs the document's name matches) when `needle` is empty.
fn rank_catalog<'a>(
  schemas: &'a [CatalogSchema],
  needle: &str,
  preferred: &[String],
) -> Vec<(&'a CatalogSchema, Option<Range<usize>>)> {
  type Scored<'a> = (u8, usize, &'a CatalogSchema, Option<Range<usize>>);
  let preferred_at = |schema: &CatalogSchema| preferred.iter().position(|url| url == &schema.url).unwrap_or(usize::MAX);
  if needle.is_empty() {
    let mut all: Vec<_> = schemas.iter().map(|schema| (schema, None)).collect();
    if !preferred.is_empty() {
      all.sort_by_key(|(schema, _)| preferred_at(schema));
    }
    return all;
  }
  let needle = needle.to_ascii_lowercase();
  let mut ranked: Vec<Scored> = schemas
    .iter()
    .filter_map(|schema| {
      let name = schema.name.to_ascii_lowercase();
      let (tier, hit) = match name.find(&needle) {
        Some(0) if name.len() == needle.len() => (0, Some(0..name.len())),
        Some(0) => (1, Some(0..needle.len())),
        Some(start) => (2, Some(start..start + needle.len())),
        None
          if schema
            .patterns
            .iter()
            .any(|pattern| pattern.to_ascii_lowercase().contains(&needle)) =>
        {
          (3, None)
        },
        None if schema.url.to_ascii_lowercase().contains(&needle) => (4, None),
        None => return None,
      };
      Some((tier, preferred_at(schema), schema, hit))
    })
    .collect();
  ranked.sort_by_key(|(tier, preferred, _, _)| (*tier, *preferred));
  ranked.into_iter().map(|(_, _, schema, hit)| (schema, hit)).collect()
}

const ROW_HEIGHT: f32 = 26.;

/// A picker row: `label` with the `hit` byte range in bold, and a muted "(current)" tag.
fn label_item(label: String, hit: Option<Range<usize>>, current: bool) -> CommandItem {
  CommandItem::new().child(move |_, cx| {
    let muted = hsla(cx.global::<ActivePalette>().0.muted_foreground);
    let split = hit
      .as_ref()
      .and_then(|hit| Some((label.get(..hit.start)?, label.get(hit.clone())?, label.get(hit.end..)?)));
    let (before, hit_text, after) = split.unwrap_or((label.as_str(), "", ""));
    div()
      .h(px(ROW_HEIGHT))
      .w_full()
      .flex()
      .items_center()
      .gap_2()
      .text_size(px(13.))
      .child(
        div()
          .flex()
          .child(before.to_owned())
          .child(div().font_weight(FontWeight::BOLD).child(hit_text.to_owned()))
          .child(after.to_owned()),
      )
      .when(current, |row| row.child(div().text_color(muted).child("(current)")))
      .into_any_element()
  })
}

/// Highlight `row` and scroll it into view. gpui-kit scrolls the nearest edge, which would leave
/// the row on the bottom edge of a long list, so the row a few lines further down is selected
/// first and the target a frame later.
pub(crate) fn select_row_in_view(state: &Entity<CommandState>, row: usize, total: usize, window: &Window) {
  const LOOKAHEAD: usize = 8;
  let state = state.clone();
  let ahead = row.saturating_add(LOOKAHEAD).min(total.saturating_sub(1));
  if ahead <= row {
    window.on_next_frame(move |window, cx| {
      state.update(cx, |state, cx| state.set_selected_index(Some(IndexPath::new(row)), window, cx));
    });
    return;
  }
  window.on_next_frame(move |window, cx| {
    state.update(cx, |state, cx| {
      state.set_selected_index(Some(IndexPath::new(ahead)), window, cx);
    });
    window.on_next_frame(move |window, _| {
      window.on_next_frame(move |window, cx| {
        state.update(cx, |state, cx| state.set_selected_index(Some(IndexPath::new(row)), window, cx));
      });
    });
  });
}

fn looks_like_schema_ref(query: &str) -> bool {
  let query = query.trim();
  query.starts_with("http://")
    || query.starts_with("https://")
    || query.starts_with("file:")
    || query.starts_with("./")
    || query.starts_with("../")
    || std::path::Path::new(query).is_absolute()
}

impl SchemaPicker {
  /// Open on the current manual pick, with search focused.
  pub fn new(current: Option<String>, preferred: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| state.focus(window, cx));
    Self {
      state,
      current,
      preferred,
      query: String::new(),
      needs_initial_selection: true,
    }
  }

  /// Move focus to the search field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.focus(window, cx));
  }

  fn filtered(&self) -> Vec<SchemaRow> {
    let needle = self.query.trim().to_lowercase();
    let query = self.query.trim();
    let catalog = rank_catalog(catalog::schemas(), &needle, &self.preferred);
    let mut rows = Vec::new();
    if needle.is_empty() || "automatic".contains(&needle) {
      rows.push(SchemaRow::Automatic);
    }
    if needle.is_empty() || "from file".contains(&needle) {
      rows.push(SchemaRow::FromFile);
    }
    if looks_like_schema_ref(query)
      && !catalog.iter().any(|(schema, _)| schema.url == query)
      && self.current.as_deref() != Some(query)
    {
      rows.push(SchemaRow::Custom(query.to_owned()));
    }
    if let Some(current) = &self.current {
      let listed = catalog.iter().any(|(schema, _)| schema.url == *current);
      if !listed && (needle.is_empty() || current.to_lowercase().contains(&needle)) {
        rows.push(SchemaRow::Custom(current.clone()));
      }
    }
    rows.extend(catalog.into_iter().map(|(schema, hit)| SchemaRow::Catalog { schema, hit }));
    rows
  }

  fn confirm(&self, path: IndexPath, cx: &mut Context<Self>) {
    match self.filtered().get(path.row) {
      Some(SchemaRow::Automatic) => cx.emit(SchemaPickerEvent::Automatic),
      Some(SchemaRow::FromFile) => cx.emit(SchemaPickerEvent::PickFile),
      Some(SchemaRow::Custom(value)) => cx.emit(SchemaPickerEvent::Picked(value.clone())),
      Some(SchemaRow::Catalog { schema, .. }) => cx.emit(SchemaPickerEvent::Picked(schema.url.clone())),
      None => {},
    }
  }
}

impl Render for SchemaPicker {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let rows = self.filtered();
    if self.needs_initial_selection {
      self.needs_initial_selection = false;
      let row = self
        .current
        .as_ref()
        .and_then(|current| rows.iter().position(|row| row.value() == Some(current.as_str())));
      let row = row.or_else(|| rows.iter().position(|row| matches!(row, SchemaRow::Automatic)));
      if let Some(row) = row {
        select_row_in_view(&self.state, row, rows.len(), window);
      }
    }
    let palette = cx.global::<ActivePalette>().0;
    let this = cx.entity().downgrade();
    let on_query = this.clone();
    let on_confirm = this.clone();
    let on_cancel = this;
    let current = self.current.clone();
    let items: Vec<CommandItem> = rows
      .iter()
      .map(|row| {
        let is_current = match row {
          SchemaRow::Automatic => current.is_none(),
          SchemaRow::FromFile => false,
          SchemaRow::Custom(value) => current.as_deref() == Some(value.as_str()),
          SchemaRow::Catalog { schema, .. } => current.as_deref() == Some(schema.url.as_str()),
        };
        let (label, hit) = match row {
          SchemaRow::Automatic => ("Automatic".to_owned(), None),
          SchemaRow::FromFile => ("From file...".to_owned(), None),
          SchemaRow::Custom(value) => (value.clone(), None),
          SchemaRow::Catalog { schema, hit } => (schema.name.clone(), hit.clone()),
        };
        label_item(label, hit, is_current)
      })
      .collect();
    let command = Command::new(&self.state)
      .filterable(false)
      .placeholder("Select a schema...")
      .max_h(px(440.))
      .bordered(false)
      .w_full()
      .bg(hsla(palette.sidebar))
      .text_size(px(13.))
      .on_query(move |query, _, cx| {
        let query = query.to_string();
        let _ = on_query.update(cx, |this, cx| {
          this.query = query;
          cx.notify();
        });
      })
      .on_confirm(move |index, _, cx| {
        let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
      })
      .on_cancel(move |_, cx| {
        let _ = on_cancel.update(cx, |_, cx| cx.emit(SchemaPickerEvent::Close));
      })
      .group(CommandGroup::new().items(items));
    overlay_frame(
      "schema-picker-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(SchemaPickerEvent::Close)),
      move |panel| panel.child(command),
    )
    .capture_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(SchemaPickerEvent::Close)))
  }
}

/// What the go-to-line prompt decided.
pub enum GoToLineEvent {
  /// Jump to a 0-based position.
  Jump(Position),
  /// Dismissed.
  Close,
}

/// A `line[:column]` prompt showing the current position and the line count.
pub struct GoToLine {
  input: Entity<InputState>,
  current: Position,
  line_count: usize,
  /// Return was pressed on input that is not a position; the hint turns emphatic until the next edit.
  rejected: bool,
  _subscription: Subscription,
}

impl EventEmitter<GoToLineEvent> for GoToLine {}

impl GoToLine {
  /// Open with the current (0-based) position prefilled and selected.
  pub fn new(current: Position, line_count: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(":line[:column]"));
    let prefill = format!("{}:{}", current.line.saturating_add(1), current.character.saturating_add(1));
    input.update(cx, |state, cx| {
      state.set_value(prefill, window, cx);
      state.select_all(window, cx);
      state.focus(window, cx);
    });
    let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
      InputEvent::PressEnter { .. } => this.confirm(cx),
      InputEvent::Change => {
        this.rejected = false;
        cx.notify();
      },
      InputEvent::Focus | InputEvent::Blur => {},
    });
    Self {
      input,
      current,
      line_count,
      rejected: false,
      _subscription: subscription,
    }
  }

  /// Move focus to the field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.input.update(cx, |state, cx| state.focus(window, cx));
  }

  /// Parse `line[:column]` (1-based) into a 0-based position. A line past the end of the document
  /// or a missing number is `None`; a column past the end of its line is clamped by the editor.
  pub fn parse(text: &str, line_count: usize) -> Option<Position> {
    let (line, column) = Self::parse_numbers(text)?;
    if line == 0 || line > line_count.max(1) {
      return None;
    }
    Some(Position::new(
      u32::try_from(line.saturating_sub(1)).ok()?,
      u32::try_from(column.max(1).saturating_sub(1)).ok()?,
    ))
  }

  fn parse_numbers(text: &str) -> Option<(usize, usize)> {
    let mut parts = text.trim().trim_start_matches(':').splitn(2, ':');
    let line: usize = parts.next()?.trim().parse().ok()?;
    let column: usize = parts.next().map_or(Ok(1), |c| c.trim().parse()).ok()?;
    Some((line, column))
  }

  /// Why `text` is not a position, for the hint line.
  fn problem(text: &str, line_count: usize) -> String {
    match Self::parse_numbers(text) {
      Some((line, _)) if line > line_count.max(1) => {
        format!("Line {line} is past the end of the document ({line_count} lines)")
      },
      _ => format!("Type a line between 1 and {line_count}, optionally :column"),
    }
  }

  fn confirm(&mut self, cx: &mut Context<Self>) {
    let text = self.input.read(cx).value();
    if let Some(position) = Self::parse(&text, self.line_count) {
      cx.emit(GoToLineEvent::Jump(position));
    } else {
      self.rejected = true;
      cx.notify();
    }
  }
}

impl Render for GoToLine {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let text = self.input.read(cx).value();
    let valid = text.trim().is_empty() || Self::parse(&text, self.line_count).is_some();
    let hint: SharedString = if valid {
      format!(
        "Current Line: {} of {} (column {})",
        self.current.line.saturating_add(1),
        self.line_count,
        self.current.character.saturating_add(1)
      )
      .into()
    } else {
      Self::problem(&text, self.line_count).into()
    };
    let input = Input::new(&self.input).bordered(false);
    let border = hsla(palette.border);
    let hint_color = if self.rejected && !valid {
      hsla(palette.foreground)
    } else {
      hsla(palette.muted_foreground)
    };
    overlay_frame(
      "goto-line-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| cx.emit(GoToLineEvent::Close)),
      move |panel| {
        panel.child(div().px_3().py_2().text_size(px(15.)).child(input)).child(
          div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .text_size(px(13.))
            .text_color(hint_color)
            .child(hint),
        )
      },
    )
    .capture_action(cx.listener(|_, _: &gpui_kit::component::input::Escape, _, cx| cx.emit(GoToLineEvent::Close)))
  }
}

/// Backdrop plus the 500-wide framed panel used by the status bar pickers.
pub(crate) fn overlay_frame(
  id: &'static str,
  palette: &openit_core::theme::UiPalette,
  on_backdrop: impl Fn(&gpui_kit::MouseDownEvent, &mut Window, &mut App) + 'static,
  panel: impl FnOnce(gpui_kit::Div) -> gpui_kit::Div,
) -> gpui_kit::Stateful<gpui_kit::Div> {
  div()
    .id(id)
    .absolute()
    .inset_0()
    .flex()
    .justify_center()
    .items_start()
    .on_mouse_down(MouseButton::Left, on_backdrop)
    .child(panel(
      div()
        .key_context("Dialog")
        .occlude()
        .mt(px(60.))
        .w(px(500.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(hsla(palette.sidebar))
        .border_1()
        .border_color(overlay_border(palette))
        .rounded_lg()
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
    ))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[core::prelude::v1::test]
  fn parse_accepts_line_and_optional_column_and_rejects_past_the_end() {
    assert_eq!(GoToLine::parse("46:2", 308), Some(Position::new(45, 1)));
    assert_eq!(GoToLine::parse("46", 308), Some(Position::new(45, 0)));
    assert_eq!(GoToLine::parse(":7", 308), Some(Position::new(6, 0)));
    assert_eq!(GoToLine::parse("308", 308), Some(Position::new(307, 0)));
    assert_eq!(GoToLine::parse("309", 308), None);
    assert_eq!(GoToLine::parse("9999", 308), None);
    assert_eq!(GoToLine::parse("0", 308), None);
    assert_eq!(GoToLine::parse("abc", 308), None);
    assert_eq!(GoToLine::parse("", 308), None);
    assert!(GoToLine::problem("9999", 308).contains("past the end"));
    assert!(GoToLine::problem("abc", 308).starts_with("Type a line"));
  }

  #[core::prelude::v1::test]
  fn labels_read_well_and_internal_grammars_are_hidden() {
    assert_eq!(language_label("text"), "Plain Text");
    assert_eq!(language_label("json"), "JSON");
    assert_eq!(language_label("rust"), "Rust");
    assert_eq!(language_label("graphql"), "GraphQL");
    let names = languages();
    assert!(names.contains(&"rust"));
    assert!(!names.contains(&"markdown_inline"));
    assert!(!names.contains(&"jsdoc"));
    let labels: Vec<String> = names.iter().map(|n| language_label(n).to_lowercase()).collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(labels, sorted);
  }

  #[core::prelude::v1::test]
  fn schema_search_ranks_name_matches_before_patterns_and_urls() {
    let schema = |name: &str, patterns: &[&str], url: &str| CatalogSchema {
      name: name.to_owned(),
      url: url.to_owned(),
      patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
    };
    let schemas = [
      schema(".cfgu.json", &[".cfgu.json"], "https://x.test/packages/schema/.cfgu.json"),
      schema("npm-package.json", &["npm-package.json"], "https://x.test/a.json"),
      schema("package.json", &["package.json"], "https://x.test/b.json"),
      schema("tsconfig.json", &["tsconfig.json", "pack.json"], "https://x.test/c.json"),
    ];
    let ranked = rank_catalog(&schemas, "pack", &[]);
    let names: Vec<&str> = ranked.iter().map(|(schema, _)| schema.name.as_str()).collect();
    assert_eq!(names, ["package.json", "npm-package.json", "tsconfig.json", ".cfgu.json"]);
    assert_eq!(ranked[0].1, Some(0..4));
    assert_eq!(ranked[1].1, Some(4..8));
    assert_eq!(ranked[2].1, None);
  }
}
