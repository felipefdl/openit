use serde_json::{Map, Value};

use super::spec::{Rgba, ThemeStyle};

/// Converts the typed Zed syntax map into the `HighlightThemeStyle` JSON shape.
pub fn syntax_styles(style: &ThemeStyle) -> Value {
  let mut syntax = Map::new();
  for (capture, token) in &style.syntax {
    let mut entry = Map::new();
    if let Some(color) = token.color.as_deref().and_then(Rgba::parse) {
      entry.insert("color".to_owned(), Value::String(color.to_hex()));
    }
    if let Some(font_style) = token.font_style.as_deref().and_then(normalize_font_style) {
      entry.insert("font_style".to_owned(), Value::String(font_style.to_owned()));
    }
    if let Some(font_weight) = token.font_weight.filter(|weight| is_supported_font_weight(*weight)) {
      entry.insert("font_weight".to_owned(), Value::from(font_weight));
    }
    if !entry.is_empty() {
      syntax.insert(capture.clone(), Value::Object(entry));
    }
  }
  let mut result = Map::new();
  result.insert("syntax".to_owned(), Value::Object(syntax));
  Value::Object(result)
}

fn normalize_font_style(style: &str) -> Option<&str> {
  match style {
    "oblique" => Some("italic"),
    "normal" | "italic" | "underline" => Some(style),
    _ => None,
  }
}

const fn is_supported_font_weight(weight: u16) -> bool {
  matches!(weight, 100 | 200 | 300 | 400 | 500 | 600 | 700 | 800 | 900)
}
#[cfg(test)]
mod tests {
  use super::*;
  use crate::theme::{ThemeKind, ThemeStyle, parse_theme_family};

  fn style(syntax: &str) -> ThemeStyle {
    let json = format!(r#"{{"name":"T","themes":[{{"name":"T","appearance":"dark","style":{{"syntax":{syntax}}}}}]}}"#);
    parse_theme_family(&json).unwrap().remove(0).style
  }

  #[test]
  fn authored_captures_pass_straight_through() {
    let source = style(
      r##"{"keyword":{"color":"#b477cf","font_style":"italic"},"string":{"color":"#a1c181","font_weight":700}}"##,
    );
    let styles = syntax_styles(&source);
    assert_eq!(styles["syntax"]["keyword"]["color"], "#b477cf");
    assert_eq!(styles["syntax"]["keyword"]["font_style"], "italic");
    assert_eq!(styles["syntax"]["string"]["font_weight"], 700);
  }

  #[test]
  fn a_null_token_is_not_registered() {
    let source = style(r#"{"keyword":{"color":null,"font_style":null,"font_weight":null}}"#);
    let styles = syntax_styles(&source);
    assert!(styles["syntax"].get("keyword").is_none());
  }

  #[test]
  fn invalid_colors_are_not_emitted() {
    let source = style(r#"{"keyword":{"color":"not-a-color","font_style":"italic"}}"#);
    let styles = syntax_styles(&source);
    assert_eq!(styles["syntax"]["keyword"]["font_style"], "italic");
    assert!(styles["syntax"]["keyword"].get("color").is_none());
  }
  #[test]
  fn unsupported_font_tokens_are_omitted_and_oblique_is_italic() {
    let source = style(
      r#"{"keyword":{"font_style":"oblique","font_weight":650},"string":{"font_style":"cursive","font_weight":401},"comment":{"font_weight":700}}"#,
    );
    let styles = syntax_styles(&source);
    assert_eq!(styles["syntax"]["keyword"]["font_style"], "italic");
    assert!(styles["syntax"]["keyword"].get("font_weight").is_none());
    assert!(styles["syntax"]["string"].get("font_style").is_none());
    assert!(styles["syntax"]["string"].get("font_weight").is_none());
    assert_eq!(styles["syntax"]["comment"]["font_weight"], 700);
  }

  #[test]
  fn theme_kind_deserializes_from_appearance() {
    let source = style(r##"{"keyword":{"color":"#b477cf"}}"##);
    assert_eq!(source.syntax["keyword"].color.as_deref(), Some("#b477cf"));
    let json = r#"{"name":"T","themes":[{"name":"T","appearance":"light","style":{}}]}"#;
    assert_eq!(parse_theme_family(json).unwrap().remove(0).kind, ThemeKind::Light);
  }
}
