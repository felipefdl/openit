//! Zed-format theme parsing and role-color resolution.

/// Palette role resolution.
pub mod palette;
/// Zed theme family parsing and color values.
pub mod spec;
/// GPUI-compatible syntax style serialization.
pub mod syntax;

pub use palette::UiPalette;
pub use spec::{
  Player, Rgba, SyntaxToken, ThemeKind, ThemeMeta, ThemeSpec, ThemeStyle, parse_theme_family, parse_theme_family_meta,
};
pub use syntax::syntax_styles;
