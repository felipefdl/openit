use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::error::Error;

/// The appearance a theme targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
  /// A dark appearance.
  Dark,
  /// A light appearance.
  Light,
}

impl ThemeKind {
  /// Returns whether this is a dark appearance.
  pub const fn is_dark(self) -> bool {
    matches!(self, Self::Dark)
  }
}

/// One `syntax` entry. The key is already a tree-sitter capture name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SyntaxToken {
  /// The token color in Zed's hexadecimal format.
  pub color: Option<String>,
  /// An optional font style such as `italic` or `oblique`.
  pub font_style: Option<String>,
  /// An optional numeric font weight.
  pub font_weight: Option<u16>,
}

/// One cursor slot. Zed authors the caret and selection wash here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Player {
  /// The caret color.
  pub cursor: Option<String>,
  /// The cursor background color.
  pub background: Option<String>,
  /// The selection color.
  pub selection: Option<String>,
}

/// A theme's `style`: flat color keys, syntax tokens, and cursor players.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ThemeStyle {
  /// Syntax token styles keyed by tree-sitter capture name.
  #[serde(default)]
  pub syntax: BTreeMap<String, SyntaxToken>,
  /// Cursor and selection slots authored by the theme.
  #[serde(default)]
  pub players: Vec<Player>,
  /// Every other `style` key. Values are usually hexadecimal strings, sometimes null.
  #[serde(flatten)]
  pub colors: BTreeMap<String, Value>,
}

impl ThemeStyle {
  /// Returns a parsed color declared at `key`.
  pub fn color(&self, key: &str) -> Option<Rgba> {
    Rgba::parse(self.colors.get(key)?.as_str()?)
  }

  /// Returns the first player's caret color, when it parses.
  pub fn cursor(&self) -> Option<Rgba> {
    Rgba::parse(self.players.first()?.cursor.as_deref()?)
  }

  /// Returns the first player's selection color, when it parses.
  pub fn selection(&self) -> Option<Rgba> {
    Rgba::parse(self.players.first()?.selection.as_deref()?)
  }
}

/// One theme inside a family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSpec {
  /// Stable catalog id derived from the authored name.
  pub id: String,
  /// Authored theme name shown in the picker.
  pub name: String,
  /// Appearance targeted by this theme.
  pub kind: ThemeKind,
  /// Color and syntax data authored by the theme.
  pub style: ThemeStyle,
}

#[derive(Debug, Deserialize)]
struct ThemeFamily {
  name: String,
  themes: Vec<RawThemeSpec>,
}

#[derive(Debug, Deserialize)]
struct RawThemeSpec {
  name: String,
  #[serde(rename = "appearance")]
  kind: ThemeKind,
  #[serde(default)]
  style: ThemeStyle,
}

/// Parses a Zed theme family and assigns stable ids to its themes.
///
/// Empty families are rejected because they cannot register a theme.
pub fn parse_theme_family(json: &str) -> std::result::Result<Vec<ThemeSpec>, Error> {
  let family: ThemeFamily = serde_json::from_str(json).map_err(|source| Error::Theme {
    name: "theme family".to_owned(),
    reason: source.to_string(),
  })?;
  if family.themes.is_empty() {
    return Err(Error::Theme {
      name: family.name,
      reason: "declares no themes".to_owned(),
    });
  }
  Ok(
    family
      .themes
      .into_iter()
      .map(|theme| {
        let id = slug(&theme.name);
        ThemeSpec {
          id,
          name: theme.name,
          kind: theme.kind,
          style: theme.style,
        }
      })
      .collect(),
  )
}

fn slug(name: &str) -> String {
  let mut id = String::with_capacity(name.len());
  for ch in name.chars() {
    if ch.is_ascii_alphanumeric() {
      id.push(ch.to_ascii_lowercase());
    } else if !id.ends_with('-') {
      id.push('-');
    }
  }
  while id.ends_with('-') {
    id.pop();
  }
  id
}

/// A red, green, blue, and alpha color channel tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
  /// Red channel.
  pub r: u8,
  /// Green channel.
  pub g: u8,
  /// Blue channel.
  pub b: u8,
  /// Alpha channel.
  pub a: u8,
}

impl Rgba {
  /// Creates an opaque RGB color.
  pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
    Self { r, g, b, a: 255 }
  }

  /// Parses `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`.
  pub fn parse(text: &str) -> Option<Self> {
    let hex = text.trim().strip_prefix('#')?;
    let mut digits = hex.bytes();
    match hex.len() {
      3 => {
        let r = expand_digit(digits.next()?)?;
        let g = expand_digit(digits.next()?)?;
        let b = expand_digit(digits.next()?)?;
        digits.next().is_none().then_some(Self::rgb(r, g, b))
      },
      4 => {
        let r = expand_digit(digits.next()?)?;
        let g = expand_digit(digits.next()?)?;
        let b = expand_digit(digits.next()?)?;
        let a = expand_digit(digits.next()?)?;
        digits.next().is_none().then_some(Self { r, g, b, a })
      },
      6 => {
        let r = pair(digits.next()?, digits.next()?)?;
        let g = pair(digits.next()?, digits.next()?)?;
        let b = pair(digits.next()?, digits.next()?)?;
        digits.next().is_none().then_some(Self::rgb(r, g, b))
      },
      8 => {
        let r = pair(digits.next()?, digits.next()?)?;
        let g = pair(digits.next()?, digits.next()?)?;
        let b = pair(digits.next()?, digits.next()?)?;
        let a = pair(digits.next()?, digits.next()?)?;
        digits.next().is_none().then_some(Self { r, g, b, a })
      },
      _ => None,
    }
  }

  /// Returns the color as lowercase hexadecimal, omitting opaque alpha.
  pub fn to_hex(self) -> String {
    if self.a == 255 {
      format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    } else {
      format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
  }

  /// Returns this color with a replacement alpha channel.
  pub const fn with_alpha(self, a: u8) -> Self {
    Self { a, ..self }
  }

  /// Linearly blends toward `other` by `t`, clamped to `0..=1`.
  pub fn mix(self, other: Self, t: f32) -> Self {
    let t = if t.is_finite() {
      t.clamp(0.0, 1.0)
    } else {
      0.0
    };
    Self {
      r: mix_channel(self.r, other.r, t),
      g: mix_channel(self.g, other.g, t),
      b: mix_channel(self.b, other.b, t),
      a: mix_channel(self.a, other.a, t),
    }
  }

  /// Returns whether perceived luminance is below the midpoint.
  pub fn is_dark(self) -> bool {
    let luma = u32::from(self.r) * 2_126 + u32::from(self.g) * 7_152 + u32::from(self.b) * 722;
    luma < 1_280_000
  }
}

const fn hex_digit(digit: u8) -> Option<u8> {
  match digit {
    b'0'..=b'9' => Some(digit - b'0'),
    b'a'..=b'f' => Some(digit - b'a' + 10),
    b'A'..=b'F' => Some(digit - b'A' + 10),
    _ => None,
  }
}

fn expand_digit(digit: u8) -> Option<u8> {
  Some(hex_digit(digit)? * 17)
}

fn pair(first: u8, second: u8) -> Option<u8> {
  Some(hex_digit(first)? * 16 + hex_digit(second)?)
}

fn mix_channel(first: u8, second: u8, t: f32) -> u8 {
  let value = (f32::from(second) - f32::from(first)).mul_add(t, f32::from(first));
  rounded_channel(value)
}

fn rounded_channel(value: f32) -> u8 {
  if !value.is_finite() || value <= 0.0 {
    return 0;
  }
  if value >= 254.5 {
    return 255;
  }
  if value < 0.5 {
    return 0;
  }
  let bits = value.to_bits();
  let exponent = (bits >> 23) & 0xff;
  let mantissa = (bits & 0x7f_ffff) | 0x80_0000;
  let shift = 150_u32 - exponent;
  let integer = mantissa >> shift;
  let remainder = mantissa & ((1_u32 << shift) - 1);
  let rounded = integer + u32::from(remainder >= (1_u32 << (shift - 1)));
  u8::try_from(rounded).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::{Settings, ThemeMode};

  const FAMILY: &str = r##"{
    "name": "Test Family",
    "author": "Nobody",
    "themes": [
      {
        "name": "Test Dark",
        "appearance": "dark",
        "style": {
          "background": "#101010",
          "text": "#eeeeee",
          "border.transparent": "#00000000",
          "missing": null,
          "players": [{ "cursor": "#ff0000", "selection": "#ff000033" }],
          "syntax": { "keyword": { "color": "#b477cf", "font_style": "italic", "font_weight": null } }
        }
      },
      { "name": "Test Light", "appearance": "light", "style": {} }
    ]
  }"##;

  #[test]
  fn parses_every_theme_in_a_family() {
    let themes = parse_theme_family(FAMILY).unwrap();
    assert_eq!(themes.len(), 2);
    assert_eq!(themes[0].name, "Test Dark");
    assert_eq!(themes[0].kind, ThemeKind::Dark);
    assert_eq!(themes[1].kind, ThemeKind::Light);
  }

  #[test]
  fn style_reads_colors_players_and_syntax() {
    let themes = parse_theme_family(FAMILY).unwrap();
    let style = &themes[0].style;
    assert_eq!(style.color("background"), Some(Rgba::rgb(0x10, 0x10, 0x10)));
    assert_eq!(style.color("border.transparent"), Some(Rgba::rgb(0, 0, 0).with_alpha(0)));
    assert_eq!(style.color("missing"), None);
    assert_eq!(style.color("absent"), None);
    assert_eq!(style.cursor(), Some(Rgba::rgb(0xff, 0, 0)));
    assert_eq!(style.selection(), Some(Rgba::rgb(0xff, 0, 0).with_alpha(0x33)));
    assert_eq!(style.syntax["keyword"].font_style.as_deref(), Some("italic"));
    assert!(!style.colors.contains_key("syntax"));
    assert!(!style.colors.contains_key("players"));
  }

  #[test]
  fn ids_are_slugs_of_the_name() {
    let themes = parse_theme_family(FAMILY).unwrap();
    assert_eq!(themes[0].id, "test-dark");
    let odd = parse_theme_family(
      r#"{"name":"Warm Burnout","themes":[{"name":"Warm Burnout: Night!","appearance":"dark","style":{}}]}"#,
    )
    .unwrap();
    assert_eq!(odd[0].id, "warm-burnout-night");
  }

  #[test]
  fn bundled_one_family_yields_one_dark_and_one_light() {
    let specs = parse_theme_family(include_str!("../../tests/fixtures/one.json")).unwrap();
    let ids: Vec<_> = specs.iter().map(|s| (s.id.as_str(), s.kind)).collect();
    assert_eq!(ids, vec![("one-dark", ThemeKind::Dark), ("one-light", ThemeKind::Light)]);
  }

  #[test]
  fn a_family_without_themes_is_an_error_not_a_panic() {
    assert!(parse_theme_family(r#"{"name":"x","themes":[]}"#).is_err());
    assert!(parse_theme_family("not json").is_err());
  }

  #[test]
  fn theme_settings_default_to_system_and_the_one_family() {
    let s = Settings::default();
    assert_eq!(s.theme.mode, ThemeMode::System);
    assert_eq!(s.theme.light, "warm-burnout-light");
    assert_eq!(s.theme.dark, "warm-burnout-dark");
  }

  #[test]
  fn theme_settings_parse_from_toml_with_partial_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "[theme]\nmode = \"dark\"\ndark = \"gruvbox-dark-hard\"\n").unwrap();
    let s = Settings::load(&path).unwrap();
    assert_eq!(s.theme.mode, ThemeMode::Dark);
    assert_eq!(s.theme.dark, "gruvbox-dark-hard");
    assert_eq!(s.theme.light, "warm-burnout-light");
  }

  #[test]
  fn a_family_parser_reports_typed_theme_errors() {
    let err = parse_theme_family("not json").unwrap_err();
    assert!(matches!(err, crate::error::Error::Theme { .. }));
  }

  #[test]
  fn hex_parses_every_length() {
    assert_eq!(Rgba::parse("#fff"), Some(Rgba::rgb(255, 255, 255)));
    assert_eq!(Rgba::parse("#0f08"), Some(Rgba::rgb(0, 255, 0).with_alpha(0x88)));
    assert_eq!(Rgba::parse("#74ade8"), Some(Rgba::rgb(0x74, 0xad, 0xe8)));
    assert_eq!(Rgba::parse("#74ade83d"), Some(Rgba::rgb(0x74, 0xad, 0xe8).with_alpha(0x3d)));
    assert_eq!(Rgba::parse("74ade8"), None);
    assert_eq!(Rgba::parse("#12345"), None);
  }

  #[test]
  fn hex_round_trips_and_mixes() {
    assert_eq!(Rgba::rgb(0, 0, 0).to_hex(), "#000000");
    assert_eq!(Rgba::rgb(0, 0, 0).with_alpha(0x80).to_hex(), "#00000080");
    assert_eq!(Rgba::rgb(0, 0, 0).mix(Rgba::rgb(255, 255, 255), 0.5), Rgba::rgb(128, 128, 128));
    assert!(Rgba::rgb(20, 20, 20).is_dark());
    assert!(!Rgba::rgb(240, 240, 240).is_dark());
  }
}
