use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, BorrowAppContext as _, Menu, MenuItem, OsAction};
use openit_core::settings::{DiffLayout, MarkdownPreviewWidth, Settings, ThemeMode};

use crate::actions::{
  ActualSize, CheckForUpdates, CloseWindow, CodeFont, ColorTheme, ConvertToMarkdown, CycleBackground, Export, Find,
  FlipHorizontal, FlipVertical, GoToFile, GoToPage, InstallCommandLineTools, NewFromClipboard, OpenFile, PdfPages,
  Quit, RotateLeft, RotateRight, Save, SelectNextOccurrence, SetDiffLayout, SetImageBackground,
  SetMarkdownPreviewWidth, SetThemeMode, ShowChanges, ToggleAlwaysShowStatusBar, ToggleMode, UiFont, ZoomIn, ZoomOut,
  ZoomToFit,
};
use crate::image_view::ImageBackground;

/// Build the application's native menu bar for the current settings.
#[cfg(test)]
pub(crate) fn build(settings: &Settings) -> Vec<Menu> {
  build_with_background(settings, None)
}

/// The menu bar with the check on View > Image Background set for `current_background`.
fn build_with_background(settings: &Settings, current_background: Option<ImageBackground>) -> Vec<Menu> {
  let mode = settings.theme.mode;
  let preview_width = Menu::new("Markdown Preview Width").items([
    MenuItem::action("Readable", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::Readable })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::Readable),
    MenuItem::action("Wide", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::Wide })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::Wide),
    MenuItem::action("Full Width", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::FullWidth })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::FullWidth),
  ]);
  let diff_layout = Menu::new("Diff Layout").items([
    MenuItem::action("Automatic", SetDiffLayout { layout: DiffLayout::Auto })
      .checked(settings.diff_layout == DiffLayout::Auto),
    MenuItem::action("Side by Side", SetDiffLayout { layout: DiffLayout::SideBySide })
      .checked(settings.diff_layout == DiffLayout::SideBySide),
    MenuItem::action("Inline", SetDiffLayout { layout: DiffLayout::Inline })
      .checked(settings.diff_layout == DiffLayout::Inline),
  ]);
  let appearance = Menu::new("Appearance").items([
    MenuItem::action("System", SetThemeMode(ThemeMode::System)).checked(mode == ThemeMode::System),
    MenuItem::action("Light", SetThemeMode(ThemeMode::Light)).checked(mode == ThemeMode::Light),
    MenuItem::action("Dark", SetThemeMode(ThemeMode::Dark)).checked(mode == ThemeMode::Dark),
  ]);
  let font = Menu::new("Font").items([
    MenuItem::action("UI Font...", UiFont),
    MenuItem::action("Code Font...", CodeFont),
  ]);
  let background_item = |label: &'static str, background: ImageBackground| {
    MenuItem::action(label, SetImageBackground { background }).checked(current_background == Some(background))
  };
  let image_background = Menu::new("Image Background").items([
    background_item("Theme", ImageBackground::Theme),
    background_item("White", ImageBackground::White),
    background_item("Black", ImageBackground::Black),
    background_item("Checkerboard", ImageBackground::Checkerboard),
    MenuItem::separator(),
    MenuItem::action("Next Background", CycleBackground),
  ]);
  let mut app_items = Vec::new();
  if cfg!(target_os = "macos") {
    app_items.push(MenuItem::action("Install Command Line Tools...", InstallCommandLineTools));
    app_items.push(MenuItem::separator());
  }
  app_items.push(MenuItem::action("Check for Updates...", CheckForUpdates));
  app_items.push(MenuItem::separator());
  app_items.push(MenuItem::action("Quit OpenIt", Quit));
  vec![
    Menu::new("OpenIt").items(app_items),
    Menu::new("File").items([
      MenuItem::action("New from Clipboard", NewFromClipboard),
      MenuItem::action("Open...", OpenFile),
      MenuItem::action("Go to File...", GoToFile),
      MenuItem::separator(),
      MenuItem::action("Close Window", CloseWindow),
      MenuItem::action("Save", Save),
      MenuItem::action("Export...", Export),
      MenuItem::action("Convert to Markdown", ConvertToMarkdown),
    ]),
    Menu::new("Edit").items([
      MenuItem::os_action("Undo", Undo, OsAction::Undo),
      MenuItem::os_action("Redo", Redo, OsAction::Redo),
      MenuItem::separator(),
      MenuItem::os_action("Cut", Cut, OsAction::Cut),
      MenuItem::os_action("Copy", Copy, OsAction::Copy),
      MenuItem::os_action("Paste", Paste, OsAction::Paste),
      MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
      MenuItem::separator(),
      MenuItem::action("Find...", Find),
      MenuItem::action("Select Next Occurrence", SelectNextOccurrence),
    ]),
    Menu::new("View").items([
      MenuItem::action("Toggle Preview / Edit", ToggleMode),
      MenuItem::action("Show Changes", ShowChanges),
      MenuItem::submenu(diff_layout),
      MenuItem::action("Always Show Status Bar", ToggleAlwaysShowStatusBar).checked(settings.always_show_status_bar),
      MenuItem::submenu(preview_width),
      MenuItem::separator(),
      MenuItem::action("Zoom In", ZoomIn),
      MenuItem::action("Zoom Out", ZoomOut),
      MenuItem::action("Fit", ZoomToFit),
      MenuItem::action("Actual Size", ActualSize),
      MenuItem::action("Go to Page...", GoToPage),
      MenuItem::action("PDF Pages", PdfPages),
      MenuItem::submenu(image_background),
      MenuItem::separator(),
      MenuItem::submenu(appearance),
      MenuItem::action("Color Theme...", ColorTheme),
      MenuItem::submenu(font),
    ]),
    Menu::new("Tools").items([
      MenuItem::action("Rotate Left", RotateLeft),
      MenuItem::action("Rotate Right", RotateRight),
      MenuItem::separator(),
      MenuItem::action("Flip Horizontal", FlipHorizontal),
      MenuItem::action("Flip Vertical", FlipVertical),
    ]),
  ]
}

/// The background the frontmost image window shows, for the checkmark in
/// View > Image Background. `None` until an image window reports one.
#[derive(Default)]
pub(crate) struct ActiveImageBackground(pub(crate) Option<ImageBackground>);

impl gpui_kit::Global for ActiveImageBackground {}

/// Record the background of the frontmost image window and refresh the check in
/// View > Image Background when it changed.
pub(crate) fn set_active_image_background(background: ImageBackground, cx: &mut App) {
  let changed = cx.update_default_global::<ActiveImageBackground, _>(|active, _| {
    let changed = active.0 != Some(background);
    active.0 = Some(background);
    changed
  });
  if changed {
    install(cx);
  }
}

/// Install the application's native menu bar.
pub fn install(cx: &App) {
  let Some(settings) = cx.try_global::<crate::settings::AppSettings>() else {
    return;
  };
  let background = cx.try_global::<ActiveImageBackground>().and_then(|active| active.0);
  cx.set_menus(build_with_background(&settings.0, background));
}

#[cfg(test)]
mod tests {
  use gpui_kit::MenuItem;
  use openit_core::settings::Settings;

  #[test]
  fn view_menu_lists_font_submenu() {
    let items = super::build(&Settings::default())
      .into_iter()
      .filter(|menu| menu.name == "View")
      .flat_map(|menu| menu.items)
      .find_map(|item| match item {
        MenuItem::Submenu(menu) if menu.name == "Font" => Some(menu.items),
        _ => None,
      })
      .expect("the View menu offers Font");
    let names: Vec<_> = items
      .iter()
      .filter_map(|item| match item {
        MenuItem::Action { name, .. } => Some(name.as_str()),
        _ => None,
      })
      .collect();
    assert_eq!(names, ["UI Font...", "Code Font..."]);
  }

  #[test]
  fn file_menu_lists_go_to_file() {
    let listed = super::build(&Settings::default())
      .into_iter()
      .filter(|menu| menu.name == "File")
      .flat_map(|menu| menu.items)
      .any(|item| matches!(item, MenuItem::Action { name, .. } if name == "Go to File..."));
    assert!(listed, "the File menu lists Go to File...");
  }

  #[test]
  fn openit_menu_lists_check_for_updates() {
    let listed = super::build(&Settings::default())
      .into_iter()
      .filter(|menu| menu.name == "OpenIt")
      .flat_map(|menu| menu.items)
      .any(|item| matches!(item, MenuItem::Action { name, .. } if name == "Check for Updates..."));
    assert!(listed, "the OpenIt menu lists Check for Updates...");
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn openit_menu_lists_install_command_line_tools() {
    let listed = super::build(&Settings::default())
      .into_iter()
      .filter(|menu| menu.name == "OpenIt")
      .flat_map(|menu| menu.items)
      .any(|item| matches!(item, MenuItem::Action { name, .. } if name == "Install Command Line Tools..."));
    assert!(listed, "the OpenIt menu lists Install Command Line Tools...");
  }

  #[test]
  fn image_background_menu_checks_only_the_background_in_use() {
    use crate::image_view::ImageBackground;

    let checked: Vec<_> = super::build_with_background(&Settings::default(), Some(ImageBackground::Black))
      .into_iter()
      .filter(|menu| menu.name == "View")
      .flat_map(|menu| menu.items)
      .find_map(|item| match item {
        MenuItem::Submenu(menu) if menu.name == "Image Background" => Some(menu.items),
        _ => None,
      })
      .expect("the View menu offers Image Background")
      .into_iter()
      .filter_map(|item| match item {
        MenuItem::Action { name, checked: true, .. } => Some(name),
        _ => None,
      })
      .collect();
    assert_eq!(checked, ["Black"]);
  }
}
