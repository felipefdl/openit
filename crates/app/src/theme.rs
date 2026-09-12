use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read as _, Result as IoResult};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::highlighter::HighlightThemeStyle;
use gpui_kit::component::theme::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode as GpuiThemeMode};
use gpui_kit::{App, Global, Hsla, SharedString, Window, WindowAppearance};
use openit_core::settings::ThemeMode as SettingsThemeMode;
use openit_core::theme::{Rgba, ThemeKind, ThemeSpec, ThemeStyle, UiPalette, parse_theme_family, syntax_styles};
use serde_json::Value;

use crate::assets;
use crate::settings::AppSettings;

const MAX_THEME_BYTES: usize = 1024 * 1024;

/// The user theme directory selected by the application controller.
#[derive(Debug, Clone, Default)]
pub struct ThemeDirs {
  /// The directory containing user-authored `.json` theme families.
  pub user: Option<PathBuf>,
}

impl Global for ThemeDirs {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
  /// Stable theme id.
  pub id: String,
  /// Authored theme name shown to users.
  pub label: String,
  /// Appearance targeted by this theme.
  pub kind: ThemeKind,
}

/// Every bundled and user theme, parsed when the catalog is built.
pub struct ThemeCatalog {
  /// Themes sorted by label.
  pub(crate) entries: Vec<ThemeEntry>,
  specs: HashMap<String, ThemeSpec>,
  palettes: HashMap<String, UiPalette>,
  dark_base: ThemeStyle,
  light_base: ThemeStyle,
  /// How many catalogs this application has built, counting this one.
  #[cfg(test)]
  builds: usize,
}

impl Global for ThemeCatalog {}

impl ThemeCatalog {
  /// Return the application theme catalog.
  pub fn get(cx: &App) -> &Self {
    cx.global::<Self>()
  }

  /// How many catalogs this application has built.
  #[cfg(test)]
  pub(crate) const fn builds(&self) -> usize {
    self.builds
  }

  /// Return the resolved palette for `id`.
  pub fn palette(&self, id: &str) -> Option<UiPalette> {
    self.palettes.get(id).copied()
  }

  /// Return the parsed specification for `id`.
  pub fn spec(&self, id: &str) -> Option<&ThemeSpec> {
    self.specs.get(id)
  }

  /// Return the appearance kind for `id`.
  pub fn kind(&self, id: &str) -> Option<ThemeKind> {
    self.entries.iter().find_map(|entry| (entry.id == id).then_some(entry.kind))
  }

  const fn base(&self, kind: ThemeKind) -> &ThemeStyle {
    match kind {
      ThemeKind::Dark => &self.dark_base,
      ThemeKind::Light => &self.light_base,
    }
  }
}

/// The palette of the theme in effect for application-owned surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivePalette(pub UiPalette);

impl Global for ActivePalette {}

impl ActivePalette {
  /// Brand mark color for application-owned surfaces.
  pub fn mark(self) -> Hsla {
    hsla(self.0.primary)
  }
}

/// Build gpui-component's theme config for one OpenIt theme.
pub fn theme_config(spec: &ThemeSpec, palette: &UiPalette, _base: &ThemeStyle) -> ThemeConfig {
  // ThemeConfigColors has private base fields, so each supported color is assigned explicitly.
  let mut colors = ThemeConfigColors::default();
  macro_rules! set_color {
    ($field:ident, $value:expr) => {
      colors.$field = Some(hex($value));
    };
  }
  set_color!(background, palette.background);
  set_color!(foreground, palette.foreground);
  set_color!(sidebar, palette.sidebar);
  set_color!(sidebar_foreground, palette.sidebar_foreground);
  set_color!(sidebar_border, palette.sidebar_border);
  set_color!(sidebar_accent, palette.list_hover);
  set_color!(sidebar_accent_foreground, palette.foreground);
  set_color!(sidebar_primary, palette.primary);
  set_color!(sidebar_primary_foreground, palette.primary_foreground);
  set_color!(title_bar, palette.title_bar);
  set_color!(title_bar_border, palette.border);
  set_color!(status_bar, palette.status_bar);
  set_color!(status_bar_border, palette.border);
  set_color!(border, palette.border);
  set_color!(primary, palette.primary);
  set_color!(primary_foreground, palette.primary_foreground);
  set_color!(primary_hover, palette.primary_hover);
  set_color!(primary_active, palette.primary_hover);
  set_color!(secondary, palette.secondary);
  set_color!(secondary_foreground, palette.secondary_foreground);
  set_color!(secondary_hover, palette.secondary_hover);
  set_color!(secondary_active, palette.secondary_hover);
  set_color!(muted, palette.muted);
  set_color!(muted_foreground, palette.muted_foreground);
  set_color!(accent, palette.accent);
  set_color!(accent_foreground, palette.foreground);
  set_color!(input, palette.input_border);
  set_color!(ring, palette.ring);
  set_color!(caret, palette.caret);
  set_color!(list, palette.sidebar);
  set_color!(list_active, palette.list_active);
  set_color!(list_active_border, palette.ring);
  set_color!(list_hover, palette.list_hover);
  set_color!(list_even, palette.sidebar);
  set_color!(list_head, palette.sidebar);
  set_color!(popover, palette.popover);
  set_color!(popover_foreground, palette.popover_foreground);
  set_color!(selection, palette.selection);
  set_color!(link, palette.link);
  set_color!(link_hover, palette.link_hover);
  set_color!(link_active, palette.link_hover);
  set_color!(danger, palette.danger);
  set_color!(danger_foreground, Rgba::rgb(255, 255, 255));
  set_color!(warning, palette.warning);
  set_color!(success, palette.success);
  set_color!(info, palette.info);
  set_color!(scrollbar, palette.background.with_alpha(0));
  set_color!(scrollbar_thumb, palette.scrollbar_thumb);
  set_color!(scrollbar_thumb_hover, palette.scrollbar_thumb_hover);
  set_color!(tab_bar, palette.sidebar);
  set_color!(tab, palette.sidebar);
  set_color!(tab_active, palette.background);
  set_color!(tab_foreground, palette.muted_foreground);
  set_color!(tab_active_foreground, palette.foreground);
  set_color!(overlay, palette.overlay);

  let mut highlight = syntax_styles(&spec.style);
  if let Value::Object(fields) = &mut highlight {
    fields.insert("editor.background".to_owned(), Value::String(palette.background.to_hex()));
    fields.insert("editor.foreground".to_owned(), Value::String(palette.foreground.to_hex()));
  }
  let highlight: Option<HighlightThemeStyle> = match serde_json::from_value(highlight) {
    Ok(highlight) => Some(highlight),
    Err(error) => {
      tracing::warn!(theme = %spec.id, %error, "could not deserialize theme highlight styles");
      None
    },
  };

  ThemeConfig {
    name: spec.name.clone().into(),
    mode: if spec.kind == ThemeKind::Dark {
      GpuiThemeMode::Dark
    } else {
      GpuiThemeMode::Light
    },
    colors,
    highlight,
    ..Default::default()
  }
}

fn hex(color: Rgba) -> SharedString {
  color.to_hex().into()
}

/// A palette color as a GPUI `Hsla`, for elements gpui-component does not paint.
pub fn hsla(color: Rgba) -> Hsla {
  gpui_kit::Rgba {
    r: f32::from(color.r) / 255.0,
    g: f32::from(color.g) / 255.0,
    b: f32::from(color.b) / 255.0,
    a: f32::from(color.a) / 255.0,
  }
  .into()
}

/// Parse bundled and user themes and apply the saved theme.
pub fn init(cx: &mut App) {
  reload_user_themes(None, cx);
}

/// Rescan user themes, replace the catalog, and keep the configured theme applied.
pub fn reload_user_themes(window: Option<&mut Window>, cx: &mut App) {
  let catalog = load_catalog(cx);
  cx.set_global(catalog);
  apply_for_appearance(cx.window_appearance(), window, cx);
}

/// Switch gpui-component and the active application palette to `id`.
/// Unknown ids fall back to the default of `wanted`.
pub fn apply_theme(id: &str, wanted: ThemeKind, window: Option<&mut Window>, cx: &mut App) {
  let Some((kind, palette, spec, base)) = resolve_visual(id, wanted, cx) else {
    tracing::warn!(%id, ?wanted, "could not resolve theme");
    return;
  };
  let config = Rc::new(theme_config(&spec, &palette, &base));
  Theme::global_mut(cx).apply_config(&config);
  Theme::change(
    if kind == ThemeKind::Dark {
      GpuiThemeMode::Dark
    } else {
      GpuiThemeMode::Light
    },
    window,
    cx,
  );
  cx.set_global(ActivePalette(palette));
}

/// Apply the configured light or dark theme for a window appearance.
pub fn apply_for_appearance(appearance: WindowAppearance, window: Option<&mut Window>, cx: &mut App) {
  let settings = cx.global::<AppSettings>().0.theme.clone();
  let is_dark = matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark);
  let (wanted, id) = match settings.mode {
    SettingsThemeMode::System => {
      if is_dark {
        (ThemeKind::Dark, settings.dark)
      } else {
        (ThemeKind::Light, settings.light)
      }
    },
    SettingsThemeMode::Light => (ThemeKind::Light, settings.light),
    SettingsThemeMode::Dark => (ThemeKind::Dark, settings.dark),
  };
  apply_theme(&id, wanted, window, cx);
}

/// Return the bundled default id for an appearance kind.
pub const fn default_for(kind: ThemeKind) -> &'static str {
  match kind {
    ThemeKind::Dark => "warm-burnout-dark",
    ThemeKind::Light => "warm-burnout-light",
  }
}

fn load_catalog(cx: &App) -> ThemeCatalog {
  #[cfg(test)]
  let builds = cx.try_global::<ThemeCatalog>().map_or(0, |catalog| catalog.builds) + 1;
  let mut bundled = Vec::new();
  for (file_name, json) in assets::theme_files() {
    match parse_theme_family(&json) {
      Ok(specs) => bundled.extend(specs),
      Err(error) => tracing::warn!(%error, "skipping bundled theme family {file_name}"),
    }
  }

  let dark_base = base_style(&bundled, ThemeKind::Dark, "One Dark");
  let light_base = base_style(&bundled, ThemeKind::Light, "One Light");
  let bundled_ids: HashSet<String> = bundled.iter().map(|spec| spec.id.clone()).collect();
  let mut all_specs = bundled;
  let user_dir = cx.global::<ThemeDirs>().user.as_deref();
  load_user_specs(&mut all_specs, &bundled_ids, user_dir);

  let mut entries = Vec::new();
  let mut specs = HashMap::new();
  let mut palettes = HashMap::new();

  for spec in all_specs {
    let id = spec.id.clone();
    if specs.contains_key(&id) {
      tracing::warn!(%id, "skipping duplicate theme id");
      continue;
    }
    let base = match spec.kind {
      ThemeKind::Dark => &dark_base,
      ThemeKind::Light => &light_base,
    };
    let palette = UiPalette::from_style(&spec.style, spec.kind, base);
    entries.push(ThemeEntry {
      id: id.clone(),
      label: spec.name.clone(),
      kind: spec.kind,
    });
    palettes.insert(id.clone(), palette);
    specs.insert(id, spec);
  }
  entries.sort_by_key(|entry| entry.label.to_lowercase());

  ThemeCatalog {
    entries,
    specs,
    palettes,
    dark_base,
    light_base,
    #[cfg(test)]
    builds,
  }
}

fn base_style(specs: &[ThemeSpec], kind: ThemeKind, preferred_name: &str) -> ThemeStyle {
  specs
    .iter()
    .find(|spec| spec.kind == kind && spec.name == preferred_name)
    .or_else(|| specs.iter().find(|spec| spec.kind == kind))
    .map_or_else(ThemeStyle::default, |spec| spec.style.clone())
}

fn load_user_specs(specs: &mut Vec<ThemeSpec>, bundled_ids: &HashSet<String>, themes_dir: Option<&Path>) {
  let mut taken = bundled_ids.clone();
  let Some(themes_dir) = themes_dir else {
    return;
  };
  let mut paths = Vec::new();
  let entries = match fs::read_dir(themes_dir) {
    Ok(entries) => entries,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
    Err(error) => {
      tracing::warn!(path = %themes_dir.display(), %error, "could not scan user themes directory");
      return;
    },
  };
  for entry in entries {
    match entry {
      Ok(entry) if entry.path().extension() == Some(OsStr::new("json")) => paths.push(entry.path()),
      Ok(_) => {},
      Err(error) => tracing::warn!(path = %themes_dir.display(), %error, "could not read user theme entry"),
    }
  }
  paths.sort();

  for path in paths {
    let json = match read_theme_file(&path) {
      Ok(json) => json,
      Err(error) => {
        tracing::warn!(path = %path.display(), %error, "skipping user theme file");
        continue;
      },
    };
    let family = match parse_theme_family(&json) {
      Ok(family) => family,
      Err(error) => {
        tracing::warn!(path = %path.display(), %error, "skipping user theme file");
        continue;
      },
    };
    for spec in family {
      let id = spec.id.clone();
      if !taken.insert(id.clone()) {
        if bundled_ids.contains(&id) {
          tracing::warn!(%id, path = %path.display(), "skipping user theme with a bundled id");
        } else {
          tracing::warn!(%id, path = %path.display(), "skipping duplicate user theme id");
        }
        continue;
      }
      specs.push(spec);
    }
  }
}

fn read_theme_file(path: &Path) -> IoResult<String> {
  let file = File::open(path)?;
  if !file.metadata()?.is_file() {
    return Err(std::io::Error::other("not a regular file"));
  }
  let limit = u64::try_from(MAX_THEME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
  let mut bytes = Vec::new();
  file.take(limit).read_to_end(&mut bytes)?;
  if bytes.len() > MAX_THEME_BYTES {
    return Err(std::io::Error::other("theme file exceeds the 1 MiB limit"));
  }
  String::from_utf8(bytes).map_err(std::io::Error::other)
}

fn resolve_visual(id: &str, wanted: ThemeKind, cx: &App) -> Option<(ThemeKind, UiPalette, ThemeSpec, ThemeStyle)> {
  let catalog = ThemeCatalog::get(cx);
  let resolved_id = if catalog.specs.contains_key(id) {
    id.to_owned()
  } else {
    default_for(wanted).to_owned()
  };
  let kind = catalog.kind(&resolved_id)?;
  let spec = catalog.spec(&resolved_id)?.clone();
  let palette = catalog.palette(&resolved_id)?;
  let base = catalog.base(kind).clone();
  Some((kind, palette, spec, base))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::AppSettings;
  use gpui_kit::component::theme::Theme;
  use gpui_kit::{BorrowAppContext, TestAppContext, WindowAppearance};
  use openit_core::settings::{Settings, ThemeMode as SettingsThemeMode};

  fn init_theme(cx: &TestAppContext) {
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(ThemeDirs::default());
      init(cx);
    });
  }

  #[gpui_kit::test]
  fn init_registers_every_bundled_theme(cx: &TestAppContext) {
    init_theme(cx);
    cx.update(|cx| {
      assert!(!ThemeCatalog::get(cx).entries.is_empty());
      assert_eq!(ThemeCatalog::get(cx).kind("one-dark"), Some(ThemeKind::Dark));
      assert_eq!(ThemeCatalog::get(cx).kind("one-light"), Some(ThemeKind::Light));
      assert!(ThemeCatalog::get(cx).kind("gruvbox-dark-hard").is_some());
      assert!(ThemeCatalog::get(cx).kind("ayu-mirage").is_some());
    });
  }

  #[gpui_kit::test]
  fn apply_theme_with_an_unknown_id_falls_back_to_the_default_of_its_kind(cx: &TestAppContext) {
    init_theme(cx);
    cx.update(|cx| {
      apply_theme("nope", ThemeKind::Dark, None, cx);
      assert!(Theme::global(cx).mode.is_dark());
      let expected = ThemeCatalog::get(cx).palette("warm-burnout-dark").expect("bundled default");
      assert_eq!(cx.global::<ActivePalette>().0, expected);
    });
  }

  #[gpui_kit::test]
  fn system_mode_follows_the_appearance(cx: &TestAppContext) {
    init_theme(cx);
    cx.update(|cx| {
      apply_for_appearance(WindowAppearance::Light, None, cx);
      assert!(!Theme::global(cx).mode.is_dark());
      apply_for_appearance(WindowAppearance::Dark, None, cx);
      assert!(Theme::global(cx).mode.is_dark());
    });
  }

  #[gpui_kit::test]
  fn a_pinned_mode_ignores_the_appearance(cx: &TestAppContext) {
    init_theme(cx);
    cx.update(|cx| {
      cx.update_global::<AppSettings, _>(|settings, _| settings.0.theme.mode = SettingsThemeMode::Dark);
      apply_for_appearance(WindowAppearance::Light, None, cx);
      assert!(Theme::global(cx).mode.is_dark());
    });
  }

  #[gpui_kit::test]
  fn a_user_theme_file_is_loaded_and_a_bundled_id_wins(cx: &TestAppContext) {
    let dir = tempfile::tempdir().expect("theme directory");
    std::fs::write(
      dir.path().join("mine.json"),
      r##"{"name":"Mine","themes":[{"name":"Mine Dark","appearance":"dark","style":{"editor.background":"#010203"}}]}"##,
    )
    .expect("user theme");
    std::fs::write(
      dir.path().join("override.json"),
      r##"{"name":"Override","themes":[{"name":"One Dark","appearance":"dark","style":{"editor.background":"#ffffff"}}]}"##,
    )
    .expect("duplicate bundled theme");
    std::fs::write(
      dir.path().join("a-user.json"),
      r##"{"name":"First","themes":[{"name":"Same User","appearance":"dark","style":{"editor.background":"#010203"}}]}"##,
    )
    .expect("first duplicate user theme");
    std::fs::write(
      dir.path().join("z-user.json"),
      r##"{"name":"Second","themes":[{"name":"Same User","appearance":"dark","style":{"editor.background":"#ffffff"}}]}"##,
    )
    .expect("second duplicate user theme");
    let mut oversized = r#"{"name":"Too Big","themes":[{"name":"Too Big","appearance":"dark","style":{}}]}"#.to_owned();
    oversized.push_str(&" ".repeat(2 * 1024 * 1024));
    std::fs::write(dir.path().join("too-big.json"), oversized).expect("large theme");
    std::fs::write(
      dir.path().join("broken.json"),
      r#"{"name":"Broken","themes":[{"name":"Broken","appearance":"dark","style":{}"#,
    )
    .expect("broken theme");

    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(Settings::default()));
      cx.set_global(ThemeDirs { user: Some(dir.path().to_path_buf()) });
      init(cx);
      reload_user_themes(None, cx);
      assert_eq!(ThemeCatalog::get(cx).kind("mine-dark"), Some(ThemeKind::Dark));
      assert_eq!(
        ThemeCatalog::get(cx).spec("one-dark").map(|spec| spec.name.as_str()),
        Some("One Dark")
      );
      assert_eq!(ThemeCatalog::get(cx).kind("too-big"), None);
      assert_eq!(ThemeCatalog::get(cx).kind("broken"), None);
      assert_eq!(ThemeCatalog::get(cx).kind("same-user"), Some(ThemeKind::Dark));
      assert_eq!(
        ThemeCatalog::get(cx).palette("same-user").map(|palette| palette.background),
        Some(Rgba::rgb(1, 2, 3))
      );
    });
  }

  #[gpui_kit::test]
  fn an_oblique_syntax_token_still_builds_a_highlight_style(cx: &TestAppContext) {
    let dir = tempfile::tempdir().expect("theme directory");
    std::fs::write(
      dir.path().join("oblique.json"),
      r##"{"name":"Oblique","themes":[{"name":"Oblique Dark","appearance":"dark","style":{"syntax":{"keyword":{"color":"#ffffff","font_style":"oblique"}}}}]}"##,
    )
    .expect("user theme");
    let mut settings = Settings::default();
    settings.theme.mode = SettingsThemeMode::Dark;
    settings.theme.dark = "oblique-dark".to_owned();
    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(settings));
      cx.set_global(ThemeDirs { user: Some(dir.path().to_path_buf()) });
      init(cx);
    });

    let keyword = cx
      .read_global::<Theme, _>(|theme, _| theme.highlight_theme.style.syntax.style("keyword"))
      .expect("keyword highlight style");
    assert_eq!(keyword.color, Some(hsla(Rgba::rgb(255, 255, 255))));
    assert_eq!(keyword.font_style, Some(gpui_kit::FontStyle::Italic));
  }

  #[gpui_kit::test]
  fn reloading_user_themes_replaces_the_catalog_and_active_colors(cx: &TestAppContext) {
    let dir = tempfile::tempdir().expect("theme directory");
    let path = dir.path().join("editable.json");
    let mut settings = Settings::default();
    settings.theme.mode = SettingsThemeMode::Dark;
    settings.theme.dark = "editable-dark".to_owned();
    std::fs::write(
      &path,
      r##"{"name":"Editable","themes":[{"name":"Editable Dark","appearance":"dark","style":{"editor.background":"#010203"}}]}"##,
    )
    .expect("initial theme");

    cx.update(|cx| {
      gpui_kit::init(cx);
      cx.set_global(AppSettings(settings));
      cx.set_global(ThemeDirs { user: Some(dir.path().to_path_buf()) });
      init(cx);
      assert_eq!(ThemeCatalog::get(cx).kind("editable-dark"), Some(ThemeKind::Dark));
      assert_eq!(Theme::global(cx).background, hsla(Rgba::rgb(1, 2, 3)));
    });

    std::fs::write(
      &path,
      r##"{"name":"Editable","themes":[{"name":"Editable Dark","appearance":"dark","style":{"editor.background":"#040506"}}]}"##,
    )
    .expect("updated theme");
    cx.update(|cx| reload_user_themes(None, cx));
    cx.update(|cx| {
      assert_eq!(
        ThemeCatalog::get(cx).palette("editable-dark").map(|palette| palette.background),
        Some(Rgba::rgb(4, 5, 6))
      );
      assert_eq!(Theme::global(cx).background, hsla(Rgba::rgb(4, 5, 6)));
    });

    std::fs::write(
      &path,
      r##"{"name":"Renamed","themes":[{"name":"Renamed Dark","appearance":"dark","style":{"editor.background":"#070809"}}]}"##,
    )
    .expect("renamed theme");
    cx.update(|cx| {
      cx.update_global::<AppSettings, _>(|settings, _| settings.0.theme.dark = "renamed-dark".to_owned());
      reload_user_themes(None, cx);
      assert_eq!(ThemeCatalog::get(cx).kind("editable-dark"), None);
      assert_eq!(ThemeCatalog::get(cx).kind("renamed-dark"), Some(ThemeKind::Dark));
      assert_eq!(Theme::global(cx).background, hsla(Rgba::rgb(7, 8, 9)));
    });

    std::fs::remove_file(&path).expect("remove theme");
    cx.update(|cx| reload_user_themes(None, cx));
    cx.update(|cx| {
      assert_eq!(ThemeCatalog::get(cx).kind("editable-dark"), None);
      assert_eq!(ThemeCatalog::get(cx).kind("renamed-dark"), None);
    });
  }
}
