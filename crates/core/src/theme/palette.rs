use super::spec::{Rgba, ThemeKind, ThemeStyle};

/// Role colors for application chrome, resolved from a Zed theme's `style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPalette {
  /// Theme appearance.
  pub kind: ThemeKind,
  /// Main editor background.
  pub background: Rgba,
  /// Main editor foreground.
  pub foreground: Rgba,
  /// Sidebar background.
  pub sidebar: Rgba,
  /// Sidebar foreground.
  pub sidebar_foreground: Rgba,
  /// Sidebar border.
  pub sidebar_border: Rgba,
  /// Title bar background.
  pub title_bar: Rgba,
  /// Title bar foreground.
  pub title_bar_foreground: Rgba,
  /// Status bar background.
  pub status_bar: Rgba,
  /// Status bar foreground.
  pub status_bar_foreground: Rgba,
  /// Default border.
  pub border: Rgba,
  /// Primary accent color.
  pub primary: Rgba,
  /// Foreground on primary controls.
  pub primary_foreground: Rgba,
  /// Hovered primary accent.
  pub primary_hover: Rgba,
  /// Secondary surface color.
  pub secondary: Rgba,
  /// Foreground on secondary surfaces.
  pub secondary_foreground: Rgba,
  /// Hovered secondary surface.
  pub secondary_hover: Rgba,
  /// Muted surface color.
  pub muted: Rgba,
  /// Muted foreground color.
  pub muted_foreground: Rgba,
  /// General accent color.
  pub accent: Rgba,
  /// Input background.
  pub input: Rgba,
  /// Input border.
  pub input_border: Rgba,
  /// Focus ring color.
  pub ring: Rgba,
  /// Caret color.
  pub caret: Rgba,
  /// Active list item background.
  pub list_active: Rgba,
  /// Active list item foreground.
  pub list_active_foreground: Rgba,
  /// Hovered list item background.
  pub list_hover: Rgba,
  /// Inactive list item background.
  pub list_inactive: Rgba,
  /// Popover background.
  pub popover: Rgba,
  /// Popover foreground.
  pub popover_foreground: Rgba,
  /// Popover border.
  pub popover_border: Rgba,
  /// Selection color.
  pub selection: Rgba,
  /// Link color.
  pub link: Rgba,
  /// Hovered link color.
  pub link_hover: Rgba,
  /// Error color.
  pub danger: Rgba,
  /// Warning color.
  pub warning: Rgba,
  /// Success color.
  pub success: Rgba,
  /// Informational color.
  pub info: Rgba,
  /// Scrollbar thumb color.
  pub scrollbar_thumb: Rgba,
  /// Hovered scrollbar thumb color.
  pub scrollbar_thumb_hover: Rgba,
  /// Badge background.
  pub badge: Rgba,
  /// Badge foreground.
  pub badge_foreground: Rgba,
  /// Overlay color.
  pub overlay: Rgba,
  /// Git added color.
  pub git_added: Rgba,
  /// Git modified color.
  pub git_modified: Rgba,
  /// Git deleted color.
  pub git_deleted: Rgba,
  /// Git renamed color.
  pub git_renamed: Rgba,
  /// Git untracked color.
  pub git_untracked: Rgba,
  /// Git ignored color.
  pub git_ignored: Rgba,
  /// Git conflict color.
  pub git_conflicting: Rgba,
  /// Git staged modified color.
  pub git_staged_modified: Rgba,
  /// Git staged deleted color.
  pub git_staged_deleted: Rgba,
  /// Inserted diff line background.
  pub diff_inserted_line: Rgba,
  /// Removed diff line background.
  pub diff_removed_line: Rgba,
  /// Inserted diff text background.
  pub diff_inserted_text: Rgba,
  /// Removed diff text background.
  pub diff_removed_text: Rgba,
  /// Added gutter color.
  pub gutter_added: Rgba,
  /// Modified gutter color.
  pub gutter_modified: Rgba,
  /// Deleted gutter color.
  pub gutter_deleted: Rgba,
  /// Terminal background.
  pub terminal_background: Rgba,
  /// Terminal foreground.
  pub terminal_foreground: Rgba,
  /// Terminal cursor.
  pub terminal_cursor: Rgba,
  /// Terminal ANSI colors in normal and bright order.
  pub terminal_ansi: [Rgba; 16],
}

/// Resolves a theme first and then a same-appearance base theme.
struct Resolver<'a> {
  theme: &'a ThemeStyle,
  base: &'a ThemeStyle,
}

impl Resolver<'_> {
  fn pick(&self, keys: &[&str]) -> Option<Rgba> {
    keys
      .iter()
      .find_map(|key| self.theme.color(key))
      .or_else(|| keys.iter().find_map(|key| self.base.color(key)))
  }
  fn pick_pair(&self, key: &str, alias: &str) -> Option<Rgba> {
    self
      .theme
      .color(key)
      .or_else(|| self.theme.color(alias))
      .or_else(|| self.base.color(key))
      .or_else(|| self.base.color(alias))
  }

  fn cursor(&self) -> Option<Rgba> {
    self.theme.cursor().or_else(|| self.base.cursor())
  }

  fn selection(&self) -> Option<Rgba> {
    self.theme.selection().or_else(|| self.base.selection())
  }
}

impl UiPalette {
  /// Resolves every role from `style`, with `base` behind missing values.
  #[expect(
    clippy::too_many_lines,
    reason = "each palette role maps directly to a theme key"
  )]
  pub fn from_style(style: &ThemeStyle, kind: ThemeKind, base: &ThemeStyle) -> Self {
    let resolver = Resolver { theme: style, base };
    let dark = kind.is_dark();
    let background = resolver.pick(&["editor.background", "background"]).unwrap_or(if dark {
      Rgba::rgb(30, 30, 30)
    } else {
      Rgba::rgb(255, 255, 255)
    });
    let foreground = resolver.pick(&["editor.foreground", "text"]).unwrap_or(if dark {
      Rgba::rgb(212, 212, 212)
    } else {
      Rgba::rgb(51, 51, 51)
    });
    let hairline = foreground.with_alpha(38);
    let hover = foreground.with_alpha(20);
    let border = resolver.pick(&["border", "border.variant"]).unwrap_or(hairline);
    let sidebar = resolver.pick(&["panel.background", "surface.background"]).unwrap_or(background);
    let sidebar_foreground = resolver.pick(&["text"]).unwrap_or(foreground);
    let muted_foreground = resolver
      .pick(&["text.muted", "text.placeholder"])
      .unwrap_or_else(|| foreground.with_alpha(180));
    let primary = resolver.pick(&["text.accent"]).unwrap_or(if dark {
      Rgba::rgb(14, 99, 156)
    } else {
      Rgba::rgb(0, 122, 204)
    });
    let primary_foreground = if primary.is_dark() {
      Rgba::rgb(255, 255, 255)
    } else {
      Rgba::rgb(20, 20, 20)
    };
    let primary_hover = primary.mix(foreground, 0.15);
    let input = resolver.pick(&["editor.background", "background"]).unwrap_or(background);
    let surface = resolver
      .pick(&["element.background", "ghost_element.background", "surface.background"])
      .unwrap_or(input);
    let list_hover = resolver.pick(&["element.hover", "ghost_element.hover"]).unwrap_or(hover);
    let list_active = resolver
      .pick(&["element.selected", "ghost_element.selected"])
      .unwrap_or_else(|| primary.with_alpha(120));
    let caret = resolver
      .cursor()
      .or_else(|| resolver.pick(&["editor.foreground", "text"]))
      .unwrap_or(foreground);
    let git_added = resolver
      .pick(&["version_control.added", "created"])
      .unwrap_or(Rgba::rgb(129, 184, 139));
    let git_modified = resolver
      .pick(&["version_control.modified", "modified"])
      .unwrap_or(Rgba::rgb(226, 192, 141));
    let git_deleted = resolver
      .pick(&["version_control.deleted", "deleted"])
      .unwrap_or(Rgba::rgb(200, 116, 112));
    let terminal_foreground = resolver.pick(&["terminal.foreground", "text"]).unwrap_or(foreground);
    Self {
      kind,
      background,
      foreground,
      sidebar,
      sidebar_foreground,
      sidebar_border: resolver.pick(&["border.variant", "border"]).unwrap_or(border),
      title_bar: resolver
        .pick(&["title_bar.background", "surface.background"])
        .unwrap_or(sidebar),
      title_bar_foreground: sidebar_foreground,
      status_bar: resolver
        .pick(&["status_bar.background", "surface.background"])
        .unwrap_or(sidebar),
      status_bar_foreground: muted_foreground,
      border,
      primary,
      primary_foreground,
      primary_hover,
      secondary: surface,
      secondary_foreground: foreground,
      secondary_hover: resolver
        .pick(&["element.hover", "ghost_element.hover"])
        .unwrap_or_else(|| surface.mix(foreground, 0.1)),
      muted: surface,
      muted_foreground,
      accent: list_hover,
      input,
      input_border: resolver.pick(&["border", "border.variant"]).unwrap_or(border),
      ring: resolver.pick(&["border.focused", "border.selected"]).unwrap_or(primary),
      caret,
      list_active,
      list_active_foreground: foreground,
      list_hover,
      list_inactive: resolver
        .pick(&["element.active", "element.background"])
        .unwrap_or_else(|| list_active.with_alpha(90)),
      popover: resolver
        .pick(&["elevated_surface.background", "surface.background"])
        .unwrap_or(sidebar),
      popover_foreground: foreground,
      popover_border: resolver.pick(&["border.variant", "border"]).unwrap_or(border),
      selection: resolver
        .selection()
        .or_else(|| resolver.pick(&["element.selected"]))
        .unwrap_or_else(|| primary.with_alpha(90)),
      link: resolver.pick(&["text.accent"]).unwrap_or(primary),
      link_hover: resolver.pick(&["link_text.hover", "text.accent"]).unwrap_or(primary_hover),
      danger: resolver.pick(&["error"]).unwrap_or(Rgba::rgb(241, 76, 76)),
      warning: resolver.pick(&["warning"]).unwrap_or(Rgba::rgb(204, 167, 0)),
      success: resolver
        .pick(&["success", "created", "terminal.ansi.green"])
        .unwrap_or(Rgba::rgb(137, 209, 133)),
      info: resolver
        .pick(&["info", "text.accent", "terminal.ansi.blue"])
        .unwrap_or(Rgba::rgb(55, 148, 255)),
      scrollbar_thumb: resolver
        .pick(&["scrollbar.thumb.background"])
        .unwrap_or_else(|| foreground.with_alpha(60)),
      scrollbar_thumb_hover: resolver
        .pick(&["scrollbar.thumb.hover_background", "scrollbar.thumb.background"])
        .unwrap_or_else(|| foreground.with_alpha(100)),
      badge: resolver.pick(&["element.selected", "element.background"]).unwrap_or(primary),
      badge_foreground: foreground,
      overlay: Rgba { r: 0, g: 0, b: 0, a: 102 },
      git_added,
      git_modified,
      git_deleted,
      git_renamed: resolver.pick(&["version_control.renamed", "renamed"]).unwrap_or(git_added),
      git_untracked: git_added,
      git_ignored: resolver
        .pick(&["ignored", "text.disabled", "text.muted"])
        .unwrap_or(muted_foreground),
      git_conflicting: resolver
        .pick(&["version_control.conflict", "conflict"])
        .unwrap_or(Rgba::rgb(229, 148, 0)),
      git_staged_modified: git_modified,
      git_staged_deleted: git_deleted,
      diff_inserted_line: resolver
        .pick(&["created.background"])
        .unwrap_or_else(|| git_added.with_alpha(38)),
      diff_removed_line: resolver
        .pick(&["deleted.background"])
        .unwrap_or_else(|| git_deleted.with_alpha(38)),
      diff_inserted_text: resolver
        .pick(&["version_control.word_added"])
        .unwrap_or_else(|| git_added.with_alpha(90)),
      diff_removed_text: resolver
        .pick(&["version_control.word_deleted"])
        .unwrap_or_else(|| git_deleted.with_alpha(90)),
      gutter_added: git_added,
      gutter_modified: git_modified,
      gutter_deleted: git_deleted,
      terminal_background: resolver
        .pick(&["terminal.background", "editor.background", "background"])
        .unwrap_or(background),
      terminal_foreground,
      terminal_cursor: resolver
        .cursor()
        .or_else(|| resolver.pick(&["terminal.foreground"]))
        .unwrap_or(caret),
      terminal_ansi: ansi_palette(&resolver, dark),
    }
  }
}

const ANSI_KEYS: [(&str, &str); 16] = [
  ("terminal.ansi.black", "terminal.ansi.dim_black"),
  ("terminal.ansi.red", "terminal.ansi.dim_red"),
  ("terminal.ansi.green", "terminal.ansi.dim_green"),
  ("terminal.ansi.yellow", "terminal.ansi.dim_yellow"),
  ("terminal.ansi.blue", "terminal.ansi.dim_blue"),
  ("terminal.ansi.magenta", "terminal.ansi.dim_magenta"),
  ("terminal.ansi.cyan", "terminal.ansi.dim_cyan"),
  ("terminal.ansi.white", "terminal.ansi.dim_white"),
  ("terminal.ansi.bright_black", "terminal.ansi.black"),
  ("terminal.ansi.bright_red", "terminal.ansi.red"),
  ("terminal.ansi.bright_green", "terminal.ansi.green"),
  ("terminal.ansi.bright_yellow", "terminal.ansi.yellow"),
  ("terminal.ansi.bright_blue", "terminal.ansi.blue"),
  ("terminal.ansi.bright_magenta", "terminal.ansi.magenta"),
  ("terminal.ansi.bright_cyan", "terminal.ansi.cyan"),
  ("terminal.ansi.bright_white", "terminal.ansi.white"),
];

const ANSI_DARK: [Rgba; 16] = [
  Rgba::rgb(0x00, 0x00, 0x00),
  Rgba::rgb(0xcd, 0x31, 0x31),
  Rgba::rgb(0x0d, 0xbc, 0x79),
  Rgba::rgb(0xe5, 0xe5, 0x10),
  Rgba::rgb(0x24, 0x72, 0xc8),
  Rgba::rgb(0xbc, 0x3f, 0xbc),
  Rgba::rgb(0x11, 0xa8, 0xcd),
  Rgba::rgb(0xe5, 0xe5, 0xe5),
  Rgba::rgb(0x66, 0x66, 0x66),
  Rgba::rgb(0xf1, 0x4c, 0x4c),
  Rgba::rgb(0x23, 0xd1, 0x8b),
  Rgba::rgb(0xf5, 0xf5, 0x43),
  Rgba::rgb(0x3b, 0x8e, 0xea),
  Rgba::rgb(0xd6, 0x70, 0xd6),
  Rgba::rgb(0x29, 0xb8, 0xdb),
  Rgba::rgb(0xe5, 0xe5, 0xe5),
];

const ANSI_LIGHT: [Rgba; 16] = [
  Rgba::rgb(0x00, 0x00, 0x00),
  Rgba::rgb(0xcd, 0x31, 0x31),
  Rgba::rgb(0x00, 0xbc, 0x00),
  Rgba::rgb(0x94, 0x98, 0x00),
  Rgba::rgb(0x04, 0x51, 0xa5),
  Rgba::rgb(0xbc, 0x05, 0xbc),
  Rgba::rgb(0x05, 0x98, 0xbc),
  Rgba::rgb(0x55, 0x55, 0x55),
  Rgba::rgb(0x66, 0x66, 0x66),
  Rgba::rgb(0xcd, 0x31, 0x31),
  Rgba::rgb(0x14, 0xce, 0x14),
  Rgba::rgb(0xb5, 0xba, 0x00),
  Rgba::rgb(0x04, 0x51, 0xa5),
  Rgba::rgb(0xbc, 0x05, 0xbc),
  Rgba::rgb(0x05, 0x98, 0xbc),
  Rgba::rgb(0xa5, 0xa5, 0xa5),
];

fn ansi_palette(style: &Resolver<'_>, dark: bool) -> [Rgba; 16] {
  let mut colors = if dark { ANSI_DARK } else { ANSI_LIGHT };
  for (slot, (key, alias)) in ANSI_KEYS.iter().enumerate() {
    if let Some(color) = style.pick_pair(key, alias)
      && let Some(slot_color) = colors.get_mut(slot)
    {
      *slot_color = color;
    }
  }
  colors
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::{ThemeKind, ThemeStyle, parse_theme_family};

  fn style(json: &str) -> ThemeStyle {
    let json = format!(r#"{{"name":"T","themes":[{{"name":"T","appearance":"dark","style":{json}}}]}}"#);
    parse_theme_family(&json).unwrap().remove(0).style
  }

  #[test]
  fn roles_come_from_the_theme_that_declares_them() {
    let theme = style(
      r##"{
        "editor.background":"#101010",
        "editor.foreground":"#eeeeee",
        "panel.background":"#202020",
        "status_bar.background":"#303030",
        "border":"#404040",
        "element.hover":"#505050",
        "border.focused":"#606060",
        "error":"#ff0000",
        "players":[{"cursor":"#ffffff","selection":"#ffffff33"}]
      }"##,
    );
    let palette = UiPalette::from_style(&theme, ThemeKind::Dark, &ThemeStyle::default());
    assert_eq!(Some(palette.background), theme.color("editor.background"));
    assert_eq!(Some(palette.sidebar), theme.color("panel.background"));
    assert_eq!(Some(palette.status_bar), theme.color("status_bar.background"));
    assert_eq!(Some(palette.border), theme.color("border"));
    assert_eq!(Some(palette.list_hover), theme.color("element.hover"));
    assert_eq!(Some(palette.ring), theme.color("border.focused"));
    assert_eq!(Some(palette.caret), theme.cursor());
    assert_eq!(Some(palette.selection), theme.selection());
    assert_eq!(Some(palette.danger), theme.color("error"));
  }

  #[test]
  fn an_absent_key_takes_a_sibling_before_the_base_theme() {
    let theme = style(r##"{"created":"#118833","modified":"#ccaa55","deleted":"#cc3344"}"##);
    let base = style(r##"{"version_control.added":"#aabbcc"}"##);
    let palette = UiPalette::from_style(&theme, ThemeKind::Dark, &base);
    assert!(theme.color("version_control.added").is_none());
    assert_eq!(Some(palette.git_added), theme.color("created"));
    assert_ne!(Some(palette.git_added), base.color("version_control.added"));
    assert_eq!(Some(palette.git_modified), theme.color("modified"));
    assert_eq!(Some(palette.git_deleted), theme.color("deleted"));
  }

  #[test]
  fn the_base_theme_fills_what_the_theme_and_its_siblings_lack() {
    let theme = ThemeStyle::default();
    let base = style(
      r##"{
        "editor.background":"#101010",
        "version_control.added":"#118833",
        "scrollbar.thumb.background":"#777777"
      }"##,
    );
    let palette = UiPalette::from_style(&theme, ThemeKind::Dark, &base);
    assert_eq!(Some(palette.background), base.color("editor.background"));
    assert_eq!(Some(palette.git_added), base.color("version_control.added"));
    assert_eq!(Some(palette.scrollbar_thumb), base.color("scrollbar.thumb.background"));
    assert_eq!(palette.kind, ThemeKind::Dark);
  }

  #[test]
  fn bright_ansi_slots_fall_back_to_their_normal_slot() {
    let theme = style(r##"{"terminal.ansi.red":"#ff0000"}"##);
    let palette = UiPalette::from_style(&theme, ThemeKind::Dark, &ThemeStyle::default());
    assert_eq!(palette.terminal_ansi[1], Rgba::rgb(0xff, 0, 0));
    assert_eq!(palette.terminal_ansi[9], Rgba::rgb(0xff, 0, 0));
    assert_eq!(palette.terminal_ansi[0], Rgba::rgb(0, 0, 0));
  }

  #[test]
  fn caret_falls_back_to_the_base_theme_player() {
    let theme = ThemeStyle::default();
    let base = style(r##"{"players":[{"cursor":"#abcdef"}]}"##);
    let palette = UiPalette::from_style(&theme, ThemeKind::Dark, &base);
    assert_eq!(palette.caret, Rgba::rgb(0xab, 0xcd, 0xef));
  }

  #[test]
  fn light_themes_carry_light_defaults() {
    let theme = style(r##"{"editor.background":"#f0f0f0"}"##);
    let palette = UiPalette::from_style(&theme, ThemeKind::Light, &theme);
    assert_eq!(palette.kind, ThemeKind::Light);
    assert!(!palette.background.is_dark());
    assert_eq!(palette.terminal_ansi[0], Rgba::rgb(0, 0, 0));
  }
}
