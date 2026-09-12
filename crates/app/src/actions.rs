//! Application actions. Bound to keys in `main.rs` and dispatched by menus.
#![expect(
  clippy::derive_partial_eq_without_eq,
  reason = "gpui-kit action macro omits Eq from its generated unit structs"
)]

use gpui_kit::{Action, actions};
use openit_core::settings::{MarkdownPreviewWidth, ThemeMode};
use serde::Serialize;

/// Select the appearance mode used by the application.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Action)]
#[action(namespace = openit, no_json)]
pub struct SetThemeMode(pub ThemeMode);

/// Select the preset width used by the Markdown preview column.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Action)]
#[action(namespace = openit, no_json)]
pub struct SetMarkdownPreviewWidth {
  pub width: MarkdownPreviewWidth,
}

/// Select what fills the space behind an image.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Action)]
#[action(namespace = openit, no_json)]
pub struct SetImageBackground {
  pub background: crate::image_view::ImageBackground,
}
actions!(
  openit,
  [
    /// Write the current buffer to its path.
    Save,
    /// Switch a Markdown document between Preview and Edit.
    ToggleMode,
    /// Open the color theme picker.
    ColorTheme,
    /// Pin the status bar visible, or let preview hide it again.
    ToggleAlwaysShowStatusBar,
    /// Show the file picker and open the chosen files, one window each.
    OpenFile,
    /// Open the nearby-files picker.
    GoToFile,
    /// Open a new untitled document holding the clipboard text.
    NewFromClipboard,
    /// Close the current window, prompting before discarding unsaved changes.
    CloseWindow,
    /// Quit the application.
    Quit,
    /// Open the image export dialog.
    Export,
    /// Scale the image up one step.
    ZoomIn,
    /// Scale the image down one step.
    ZoomOut,
    /// Scale the image to fit the window.
    ZoomToFit,
    /// Show the image at one image pixel per screen pixel.
    ActualSize,
    /// Move to the next image background.
    CycleBackground,
    /// Rotate the image a quarter turn counter-clockwise.
    RotateLeft,
    /// Rotate the image a quarter turn clockwise.
    RotateRight,
    /// Mirror the image left to right.
    FlipHorizontal,
    /// Mirror the image top to bottom.
    FlipVertical,
    /// Open the find bar in a PDF window.
    Find,
    /// Step to the next search match.
    NextMatch,
    /// Step to the previous search match.
    PreviousMatch,
    /// Open the go-to-page prompt.
    GoToPage,
    /// Move down one page.
    PageDown,
    /// Move up one page.
    PageUp,
    /// Move to the first page.
    FirstPage,
    /// Move to the last page.
    LastPage,
    /// Select every character of the PDF text layer.
    SelectAll,
    /// Copy the selected PDF text.
    Copy,
    /// Convert the PDF to Markdown, or move through the Markdown views.
    ConvertToMarkdown,
    /// Show the PDF pages again.
    PdfPages,
    /// Open the command line tools install window.
    InstallCommandLineTools,
  ]
);
