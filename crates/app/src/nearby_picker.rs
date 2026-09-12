//! Nearby-files picker: a transient palette over one directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{Disableable as _, Icon, IndexPath, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
  AnyElement, AppContext, Context, Entity, EventEmitter, FontWeight, Hsla, IntoElement, Render, Window, div, px,
};
use openit_core::browse::{self, Entry, Ranked};
use openit_core::kind::DocumentKind;

use crate::status_pickers::overlay_frame;
use crate::theme::{ActivePalette, hsla};
use crate::window::replace_document;

const PLACEHOLDER: &str = "Go to File";
const DEBOUNCE_MS: u64 = 100;
const ROW_HEIGHT: f32 = 26.;

/// What the nearby-files picker decided.
pub enum NearbyPickerEvent {
  /// Dismissed without opening a file, or after a file was chosen.
  Close,
}

enum Listing {
  Pending,
  Rows { dir: PathBuf, items: Vec<Ranked> },
  Failed { message: String },
}

enum BrowseOutcome {
  Rows(Vec<Ranked>),
  Failed(String),
}

/// Fuzzy file palette rooted at one directory.
pub struct NearbyPicker {
  state: Entity<CommandState>,
  origin: PathBuf,
  prefix_dir: PathBuf,
  input: String,
  listing: Listing,
  generation: u64,
}

impl EventEmitter<NearbyPickerEvent> for NearbyPicker {}

impl NearbyPicker {
  /// Open the picker rooted at `document_path`'s directory, or the home directory when there is no path.
  ///
  /// Views create the entity with `cx.new(|cx| NearbyPicker::new(path, window, cx))` and subscribe
  /// to [`NearbyPickerEvent::Close`].
  pub fn new(document_path: Option<&Path>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let origin = listing_root(document_path);
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| state.focus(window, cx));
    let mut picker = Self {
      state,
      prefix_dir: origin.clone(),
      origin,
      input: String::new(),
      listing: Listing::Pending,
      generation: 0,
    };
    picker.schedule(false, cx);
    picker
  }

  /// Move focus to the search field.
  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.focus(window, cx));
  }

  fn on_query_change(&mut self, input: &str, cx: &mut Context<Self>) {
    input.clone_into(&mut self.input);
    let (dir, _) = browse::parse(&self.input, &self.origin);
    self.prefix_dir = dir;
    self.schedule(true, cx);
    cx.notify();
  }

  fn schedule(&mut self, debounce: bool, cx: &Context<Self>) {
    self.generation = self.generation.saturating_add(1);
    let generation = self.generation;
    let input = self.input.clone();
    let origin = self.origin.clone();
    cx.spawn(async move |this, cx| {
      if debounce {
        cx.background_executor().timer(Duration::from_millis(DEBOUNCE_MS)).await;
      }
      let Some((dir, needle)) = this
        .update(cx, |this, _| {
          (this.generation == generation).then(|| browse::parse(&input, &origin))
        })
        .ok()
        .flatten()
      else {
        return;
      };
      let listed_dir = dir.clone();
      let outcome = cx.background_spawn(async move { browse_outcome(&dir, &needle) }).await;
      let _ = this.update(cx, |this, cx| this.apply_outcome(&input, listed_dir, outcome, cx));
    })
    .detach();
  }

  fn apply_outcome(&mut self, input: &str, dir: PathBuf, outcome: BrowseOutcome, cx: &mut Context<Self>) {
    if self.input != input {
      return;
    }
    self.prefix_dir.clone_from(&dir);
    self.listing = match outcome {
      BrowseOutcome::Rows(items) => Listing::Rows { dir, items },
      BrowseOutcome::Failed(message) => Listing::Failed { message },
    };
    cx.notify();
  }

  fn confirm(&self, index: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
    let Listing::Rows { dir, items } = &self.listing else {
      return;
    };
    let Some(ranked) = items.get(index.row) else {
      return;
    };
    if ranked.entry.is_dir {
      let next = input_for_entered_dir(&self.input, &ranked.entry.name);
      self.state.update(cx, |state, cx| state.set_query(next, window, cx));
      return;
    }
    let path = dir.join(&ranked.entry.name);
    replace_document(path, window, cx);
    Self::close(cx);
  }

  fn close(cx: &mut Context<Self>) {
    cx.emit(NearbyPickerEvent::Close);
  }
}

fn listing_root(document_path: Option<&Path>) -> PathBuf {
  document_path
    .filter(|path| !path.as_os_str().is_empty())
    .and_then(Path::parent)
    .filter(|parent| !parent.as_os_str().is_empty())
    .map(Path::to_path_buf)
    .or_else(dirs::home_dir)
    .unwrap_or_else(|| PathBuf::from("."))
}

fn format_prefix(dir: &Path) -> String {
  let mut text = dir.display().to_string();
  if text.is_empty() {
    text.push('.');
  }
  if !text.ends_with(std::path::MAIN_SEPARATOR) {
    text.push(std::path::MAIN_SEPARATOR);
  }
  text
}

fn input_for_entered_dir(input: &str, name: &str) -> String {
  match input.rsplit_once('/') {
    Some(("", _)) => format!("/{name}/"),
    Some((left, _)) => format!("{left}/{name}/"),
    None => format!("{name}/"),
  }
}

fn browse_outcome(dir: &Path, needle: &str) -> BrowseOutcome {
  let show_dotfiles = needle.starts_with('.');
  match browse::list(dir, show_dotfiles) {
    Ok(entries) => BrowseOutcome::Rows(browse::rank(&entries, needle)),
    Err(error) => {
      tracing::debug!(path = %dir.display(), %error, "directory is unreadable");
      BrowseOutcome::Failed(error.to_string())
    },
  }
}

fn command_items(listing: &Listing) -> Vec<CommandItem> {
  match listing {
    Listing::Pending => Vec::new(),
    Listing::Rows { items, .. } => items.iter().map(command_item).collect(),
    Listing::Failed { message } => vec![CommandItem::new().label(message.clone()).disabled(true)],
  }
}

fn command_item(ranked: &Ranked) -> CommandItem {
  let ranked = ranked.clone();
  CommandItem::new().child(move |_, cx| {
    let muted = hsla(cx.global::<ActivePalette>().0.muted_foreground);
    highlighted_row(&ranked, muted)
  })
}

/// The Lucide icon for a row, by what a pick would open.
const fn row_icon(entry: &Entry) -> &'static str {
  if entry.is_dir {
    return "icons/folder.svg";
  }
  match entry.kind {
    DocumentKind::Markdown => "icons/file-text.svg",
    DocumentKind::Text { language: Some(_) } => "icons/file-code.svg",
    DocumentKind::Text { language: None } | DocumentKind::Unsupported => "icons/file.svg",
    DocumentKind::Image | DocumentKind::Svg => "icons/image.svg",
    DocumentKind::Pdf => "icons/book-text.svg",
  }
}

fn highlighted_row(ranked: &Ranked, muted: Hsla) -> AnyElement {
  let matched: HashSet<u32> = ranked.positions.iter().copied().collect();
  let mut runs: Vec<(String, bool)> = Vec::new();
  for (index, ch) in ranked.entry.name.chars().enumerate() {
    let hit = u32::try_from(index).is_ok_and(|index| matched.contains(&index));
    match runs.last_mut() {
      Some((text, flag)) if *flag == hit => text.push(ch),
      _ => runs.push((ch.to_string(), hit)),
    }
  }
  if ranked.entry.is_dir {
    match runs.last_mut() {
      Some((text, false)) => text.push('/'),
      _ => runs.push(("/".to_owned(), false)),
    }
  }
  div()
    .h(px(ROW_HEIGHT))
    .w_full()
    .flex()
    .items_center()
    .gap_2()
    .text_size(px(13.))
    .child(Icon::empty().path(row_icon(&ranked.entry)).small().text_color(muted))
    .child(
      div().flex().children(
        runs
          .into_iter()
          .map(|(text, hit)| div().when(hit, |el| el.font_weight(FontWeight::BOLD)).child(text)),
      ),
    )
    .into_any_element()
}

impl Render for NearbyPicker {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let palette = cx.global::<ActivePalette>().0;
    let this = cx.entity().downgrade();
    let prefix = format_prefix(&self.prefix_dir);
    let muted = hsla(palette.muted_foreground);
    let items = command_items(&self.listing);
    let on_query = this.clone();
    let on_confirm = this.clone();
    let on_cancel = this;
    let command = Command::new(&self.state)
      .filterable(false)
      .placeholder(PLACEHOLDER)
      .max_h(px(440.))
      .bordered(false)
      .w_full()
      .bg(hsla(palette.sidebar))
      .text_size(px(13.))
      .header(move |_, _, _| div().px_3().py_1().text_size(px(12.)).text_color(muted).child(prefix.clone()))
      .on_query(move |query, _, cx| {
        let query = query.to_string();
        let _ = on_query.update(cx, |this, cx| this.on_query_change(&query, cx));
      })
      .on_confirm(move |index, window, cx| {
        let _ = on_confirm.update(cx, |this, cx| this.confirm(index, window, cx));
      })
      .on_cancel(move |_, cx| {
        let _ = on_cancel.update(cx, |_, cx| Self::close(cx));
      })
      .items(items);
    overlay_frame(
      "nearby-picker-backdrop",
      &palette,
      cx.listener(|_, _, _, cx| Self::close(cx)),
      move |panel| panel.child(command),
    )
  }
}

#[cfg(test)]
impl NearbyPicker {
  fn prefix_text(&self) -> String {
    format_prefix(&self.prefix_dir)
  }
  fn query_text(&self, cx: &gpui_kit::App) -> String {
    self.state.read(cx).query(cx).to_string()
  }

  fn row_names(&self) -> Vec<String> {
    match &self.listing {
      Listing::Pending => Vec::new(),
      Listing::Rows { items, .. } => items.iter().map(Ranked::display_name).collect(),
      Listing::Failed { message } => vec![message.clone()],
    }
  }

  fn type_query(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
    self.state.update(cx, |state, cx| state.set_query(input, window, cx));
    self.on_query_change(input, cx);
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::time::Duration;

  use gpui_kit::{TestAppContext, VisualTestContext};
  use openit_core::browse::{list, rank};

  use super::*;

  fn open_picker<'a>(
    cx: &'a mut TestAppContext,
    document_path: &Path,
  ) -> (gpui_kit::Entity<NearbyPicker>, &'a mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let (_dir, _store) = crate::document_view::tests::install_globals(cx);
    let (picker, cx) = cx.add_window_view(|window, cx| NearbyPicker::new(Some(document_path), window, cx));
    cx.run_until_parked();
    (picker, cx)
  }

  fn wait_for_listing(cx: &VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(DEBOUNCE_MS));
    cx.run_until_parked();
  }

  fn markdown_tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(folder.join("sub")).unwrap();
    fs::write(folder.join("sub").join("inner.md"), "inner\n").unwrap();
    fs::write(folder.join("notes.md"), "# Hi\n").unwrap();
    fs::write(dir.path().join("sibling.md"), "sib\n").unwrap();
    let notes = folder.join("notes.md");
    (dir, folder, notes)
  }

  #[gpui_kit::test]
  fn typing_dotdot_on_a_markdown_window_changes_the_prefix_and_rows(cx: &mut TestAppContext) {
    let (_keep, folder, notes) = markdown_tree();
    let (picker, cx) = open_picker(cx, &notes);

    let before_prefix = picker.read_with(cx, |picker, _| picker.prefix_text());
    let before_rows = picker.read_with(cx, |picker, _| picker.row_names());
    assert!(before_prefix.contains(folder.file_name().unwrap().to_str().unwrap()));
    assert_eq!(before_rows, ["sub/", "notes.md"]);

    picker.update_in(cx, |picker, window, cx| picker.type_query("../", window, cx));
    wait_for_listing(cx);

    let after_prefix = picker.read_with(cx, |picker, _| picker.prefix_text());
    let after_rows = picker.read_with(cx, |picker, _| picker.row_names());
    assert_ne!(after_prefix, before_prefix);
    assert_ne!(after_rows, before_rows);
    assert!(after_rows.iter().any(|name| name == "folder/" || name == "sibling.md"));
  }
  #[gpui_kit::test]
  fn enter_on_a_directory_row_lists_that_directory(cx: &mut TestAppContext) {
    let (_keep, _folder, notes) = markdown_tree();
    let (picker, cx) = open_picker(cx, &notes);
    assert_eq!(picker.read_with(cx, |picker, _| picker.row_names()), ["sub/", "notes.md"]);

    picker.update_in(cx, |picker, window, cx| picker.confirm(IndexPath::new(0), window, cx));
    cx.run_until_parked();
    wait_for_listing(cx);

    assert_eq!(picker.read_with(cx, NearbyPicker::query_text), "sub/");
    assert_eq!(picker.read_with(cx, |picker, _| picker.row_names()), ["inner.md"]);
  }

  #[gpui_kit::test]
  fn a_stale_listing_is_dropped_when_a_newer_query_is_showing(cx: &mut TestAppContext) {
    let (_keep, folder, notes) = markdown_tree();
    let (picker, cx) = open_picker(cx, &notes);

    picker.update_in(cx, |picker, window, cx| picker.type_query("notes", window, cx));
    picker.update_in(cx, |picker, window, cx| picker.type_query("sub", window, cx));

    let first = list(&folder, false).unwrap();
    let first_rows = rank(&first, "notes");
    picker.update(cx, |picker, cx| {
      picker.apply_outcome("notes", folder.clone(), BrowseOutcome::Rows(first_rows), cx);
    });
    assert_ne!(picker.read_with(cx, |picker, _| picker.row_names()), ["notes.md"]);

    let second = list(&folder, false).unwrap();
    let second_rows = rank(&second, "sub");
    picker.update(cx, |picker, cx| {
      picker.apply_outcome("sub", folder, BrowseOutcome::Rows(second_rows), cx);
    });
    assert_eq!(picker.read_with(cx, |picker, _| picker.row_names()), ["sub/"]);
  }

  #[cfg(unix)]
  #[gpui_kit::test]
  fn an_unreadable_directory_shows_an_error_row_and_dotdot_restores_a_listing(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let (_keep, folder, notes) = markdown_tree();
    let locked = folder.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let (picker, cx) = open_picker(cx, &notes);
    picker.update_in(cx, |picker, window, cx| picker.type_query("locked/", window, cx));
    wait_for_listing(cx);

    let error_rows = picker.read_with(cx, |picker, _| picker.row_names());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(error_rows.len(), 1, "the unreadable directory is one error row");
    assert!(
      error_rows[0].contains("locked"),
      "the error names the directory: {}",
      error_rows[0]
    );

    picker.update_in(cx, |picker, window, cx| picker.type_query("../", window, cx));
    wait_for_listing(cx);
    let restored = picker.read_with(cx, |picker, _| picker.row_names());
    assert!(
      restored.iter().any(|name| name == "folder/" || name == "sibling.md"),
      "typing ../ restores a listing, got {restored:?}"
    );
  }
}
