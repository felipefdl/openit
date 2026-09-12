use std::borrow::Cow;
use std::cell::RefCell;
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
use openit_core::theme::{
  Rgba, ThemeKind, ThemeSpec, ThemeStyle, UiPalette, parse_theme_family, parse_theme_family_meta, syntax_styles,
};
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

enum FamilySource {
  Bundled(Cow<'static, [u8]>),
  User(PathBuf),
}

/// Every bundled and user theme. Style bodies are parsed on demand.
pub struct ThemeCatalog {
  /// Themes sorted by label.
  pub(crate) entries: Vec<ThemeEntry>,
  sources: HashMap<String, Rc<FamilySource>>,
  parsed: RefCell<HashMap<String, ThemeSpec>>,
  palettes: RefCell<HashMap<String, UiPalette>>,
  configs: RefCell<HashMap<String, Rc<ThemeConfig>>>,
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
    self.ensure_parsed(id);
    self.palettes.borrow().get(id).copied()
  }

  /// Return the parsed specification for `id`.
  #[cfg(test)]
  pub fn spec(&self, id: &str) -> Option<ThemeSpec> {
    self.ensure_parsed(id);
    self.parsed.borrow().get(id).cloned()
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

  fn remember(&self, specs: Vec<ThemeSpec>) {
    let mut parsed = self.parsed.borrow_mut();
    let mut palettes = self.palettes.borrow_mut();
    for spec in specs {
      if parsed.contains_key(&spec.id) {
        continue;
      }
      let base = match spec.kind {
        ThemeKind::Dark => &self.dark_base,
        ThemeKind::Light => &self.light_base,
      };
      palettes.insert(spec.id.clone(), UiPalette::from_style(&spec.style, spec.kind, base));
      parsed.insert(spec.id.clone(), spec);
    }
  }

  fn ensure_parsed(&self, id: &str) {
    if self.parsed.borrow().contains_key(id) {
      return;
    }
    let Some(source) = self.sources.get(id).cloned() else {
      return;
    };
    let Some(specs) = parse_family_source(&source) else {
      return;
    };
    self.remember(specs);
  }

  fn cached_config(&self, id: &str) -> Option<Rc<ThemeConfig>> {
    if let Some(config) = self.configs.borrow().get(id) {
      return Some(Rc::clone(config));
    }
    self.ensure_parsed(id);
    let spec = self.parsed.borrow().get(id)?.clone();
    let palette = *self.palettes.borrow().get(id)?;
    let config = Rc::new(theme_config(&spec, &palette, self.base(spec.kind)));
    self.configs.borrow_mut().insert(id.to_owned(), Rc::clone(&config));
    Some(config)
  }

  fn visual(&self, id: &str) -> Option<(ThemeKind, UiPalette, Rc<ThemeConfig>)> {
    let kind = self.kind(id)?;
    let palette = self.palette(id)?;
    let config = self.cached_config(id)?;
    Some((kind, palette, config))
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
  let resolved = {
    let catalog = ThemeCatalog::get(cx);
    if catalog.kind(id).is_some() {
      id
    } else {
      default_for(wanted)
    }
  };
  let Some((kind, palette, config)) = ThemeCatalog::get(cx).visual(resolved) else {
    tracing::warn!(%id, ?wanted, "could not resolve theme");
    return;
  };
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
  let mut entries = Vec::new();
  let mut sources = HashMap::new();
  let mut taken = HashSet::new();

  for (file_name, bytes) in assets::theme_files() {
    let json = match std::str::from_utf8(&bytes) {
      Ok(json) => json,
      Err(error) => {
        tracing::warn!(%error, "skipping bundled theme family {file_name}");
        continue;
      },
    };
    let meta = match parse_theme_family_meta(json) {
      Ok(meta) => meta,
      Err(error) => {
        tracing::warn!(%error, "skipping bundled theme family {file_name}");
        continue;
      },
    };
    let source = Rc::new(FamilySource::Bundled(bytes));
    for theme in meta {
      if !taken.insert(theme.id.clone()) {
        tracing::warn!(id = %theme.id, "skipping duplicate theme id");
        continue;
      }
      entries.push(ThemeEntry {
        id: theme.id.clone(),
        label: theme.name,
        kind: theme.kind,
      });
      sources.insert(theme.id, Rc::clone(&source));
    }
  }

  let bundled_ids = taken.clone();
  let user_dir = cx.global::<ThemeDirs>().user.as_deref();
  load_user_families(&mut entries, &mut sources, &mut taken, &bundled_ids, user_dir);

  let (dark_base, light_base, base_specs) = bases_from(&sources);
  entries.sort_by_key(|entry| entry.label.to_lowercase());
  let catalog = ThemeCatalog {
    entries,
    sources,
    parsed: RefCell::new(HashMap::new()),
    palettes: RefCell::new(HashMap::new()),
    configs: RefCell::new(HashMap::new()),
    dark_base,
    light_base,
    #[cfg(test)]
    builds,
  };
  catalog.remember(base_specs);
  let settings = &cx.global::<AppSettings>().0.theme;
  catalog.ensure_parsed(&settings.dark);
  catalog.ensure_parsed(&settings.light);
  catalog
}

fn bases_from(sources: &HashMap<String, Rc<FamilySource>>) -> (ThemeStyle, ThemeStyle, Vec<ThemeSpec>) {
  let specs = sources
    .get("one-dark")
    .and_then(|source| parse_family_source(source))
    .unwrap_or_default();
  let dark = specs
    .iter()
    .find(|spec| spec.name == "One Dark")
    .map(|spec| spec.style.clone())
    .unwrap_or_default();
  let light = specs
    .iter()
    .find(|spec| spec.name == "One Light")
    .map(|spec| spec.style.clone())
    .unwrap_or_default();
  (dark, light, specs)
}

fn parse_family_source(source: &FamilySource) -> Option<Vec<ThemeSpec>> {
  match source {
    FamilySource::Bundled(bytes) => parse_family_json(std::str::from_utf8(bytes).ok()?),
    FamilySource::User(path) => parse_family_json(&read_theme_file(path).ok()?),
  }
}

fn parse_family_json(json: &str) -> Option<Vec<ThemeSpec>> {
  match parse_theme_family(json) {
    Ok(specs) => Some(specs),
    Err(error) => {
      tracing::warn!(%error, "could not parse theme family");
      None
    },
  }
}

fn load_user_families(
  entries: &mut Vec<ThemeEntry>,
  sources: &mut HashMap<String, Rc<FamilySource>>,
  taken: &mut HashSet<String>,
  bundled_ids: &HashSet<String>,
  themes_dir: Option<&Path>,
) {
  let Some(themes_dir) = themes_dir else {
    return;
  };
  let mut paths = Vec::new();
  let dir_entries = match fs::read_dir(themes_dir) {
    Ok(dir_entries) => dir_entries,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
    Err(error) => {
      tracing::warn!(path = %themes_dir.display(), %error, "could not scan user themes directory");
      return;
    },
  };
  for entry in dir_entries {
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
    let meta = match parse_theme_family_meta(&json) {
      Ok(meta) => meta,
      Err(error) => {
        tracing::warn!(path = %path.display(), %error, "skipping user theme file");
        continue;
      },
    };
    let source = Rc::new(FamilySource::User(path.clone()));
    for theme in meta {
      let id = theme.id.clone();
      if !taken.insert(id.clone()) {
        if bundled_ids.contains(&id) {
          tracing::warn!(%id, path = %path.display(), "skipping user theme with a bundled id");
        } else {
          tracing::warn!(%id, path = %path.display(), "skipping duplicate user theme id");
        }
        continue;
      }
      entries.push(ThemeEntry {
        id: theme.id.clone(),
        label: theme.name,
        kind: theme.kind,
      });
      sources.insert(theme.id, Rc::clone(&source));
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
        ThemeCatalog::get(cx).spec("one-dark").map(|spec| spec.name),
        Some("One Dark".to_owned())
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
