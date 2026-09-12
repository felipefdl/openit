use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, Menu, MenuItem, OsAction};
use openit_core::settings::{MarkdownPreviewWidth, Settings, ThemeMode};

use crate::actions::{
  ActualSize, CloseWindow, CodeFont, ColorTheme, ConvertToMarkdown, CycleBackground, Export, Find, FlipHorizontal,
  FlipVertical, GoToFile, GoToPage, InstallCommandLineTools, NewFromClipboard, OpenFile, PdfPages, Quit, RotateLeft,
  RotateRight, Save, SetImageBackground, SetMarkdownPreviewWidth, SetThemeMode, ToggleAlwaysShowStatusBar,
  ToggleMode, UiFont, ZoomIn, ZoomOut, ZoomToFit,
};
use crate::image_view::ImageBackground;

/// Build the application's native menu bar for the current settings.
pub(crate) fn build(settings: &Settings) -> Vec<Menu> {
  let mode = settings.theme.mode;
  let preview_width = Menu::new("Markdown Preview Width").items([
    MenuItem::action("Readable", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::Readable })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::Readable),
    MenuItem::action("Wide", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::Wide })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::Wide),
    MenuItem::action("Full Width", SetMarkdownPreviewWidth { width: MarkdownPreviewWidth::FullWidth })
      .checked(settings.markdown_preview_width == MarkdownPreviewWidth::FullWidth),
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
  let image_background = Menu::new("Image Background").items([
    MenuItem::action("Theme", SetImageBackground { background: ImageBackground::Theme }),
    MenuItem::action("White", SetImageBackground { background: ImageBackground::White }),
    MenuItem::action("Black", SetImageBackground { background: ImageBackground::Black }),
    MenuItem::action(
      "Checkerboard",
      SetImageBackground {
        background: ImageBackground::Checkerboard,
      },
    ),
    MenuItem::separator(),
    MenuItem::action("Next Background", CycleBackground),
  ]);
  let mut app_items = Vec::new();
  if cfg!(target_os = "macos") {
    app_items.push(MenuItem::action("Install Command Line Tools...", InstallCommandLineTools));
    app_items.push(MenuItem::separator());
  }
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
    ]),
    Menu::new("View").items([
      MenuItem::action("Toggle Preview / Edit", ToggleMode),
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

/// Install the application's native menu bar.
pub fn install(cx: &App) {
  cx.set_menus(build(&cx.global::<crate::settings::AppSettings>().0));
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
}
