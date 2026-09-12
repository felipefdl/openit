//! Bundled `SchemaStore` catalog index and filename matching.
//!
//! The binary embeds `fileMatch` plus `url` only. Schema documents are not
//! bundled. Matching is in-process; this module does not touch the network.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::kind;

const CATALOG_JSON: &str = include_str!("../../../assets/schema-catalog.json");

/// How many catalog schemas match a document filename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogMatch {
  /// Exactly one schema URL.
  One(String),
  /// Two or more schema URLs; the caller should ask which to use.
  Ambiguous(Vec<String>),
  /// No schema, or the document is not JSON-family.
  None,
}

/// A catalog schema the picker can offer, named from the URL file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSchema {
  /// File name taken from the schema URL, used as the picker label.
  pub name: String,
  /// Schema document URL.
  pub url: String,
}

#[derive(Debug, Deserialize)]
struct CatalogEntry {
  #[serde(rename = "fileMatch")]
  file_match: Vec<String>,
  url: String,
}

fn entries() -> &'static [CatalogEntry] {
  static ENTRIES: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
  ENTRIES.get_or_init(|| match serde_json::from_str::<Vec<CatalogEntry>>(CATALOG_JSON) {
    Ok(parsed) => parsed,
    Err(error) => {
      tracing::error!(%error, "bundled schema catalog is invalid");
      Vec::new()
    },
  })
}

/// Every unique catalog schema, sorted by label then URL.
pub fn schemas() -> &'static [CatalogSchema] {
  static SCHEMAS: OnceLock<Vec<CatalogSchema>> = OnceLock::new();
  SCHEMAS.get_or_init(|| {
    let mut seen = HashSet::new();
    let mut schemas = Vec::new();
    for entry in entries() {
      if !seen.insert(entry.url.as_str()) {
        continue;
      }
      schemas.push(CatalogSchema {
        name: display_name(&entry.url),
        url: entry.url.clone(),
      });
    }
    schemas.sort_by(|left, right| left.name.cmp(&right.name).then(left.url.cmp(&right.url)));
    schemas
  })
}

fn display_name(url: &str) -> String {
  url
    .rsplit('/')
    .find(|part| !part.is_empty())
    .map_or_else(|| "schema".to_owned(), str::to_owned)
}

/// Look up catalog schema URLs for `path` by file name.
///
/// Only JSON-family documents (`.json`, `.jsonc`, `.json5`) consult the catalog.
/// Directory-bearing `fileMatch` patterns still apply when `path` includes those
/// segments. A bare file name is matched as if it were at the workspace root.
pub fn match_filename(path: &Path) -> CatalogMatch {
  if !is_json_family(path) {
    return CatalogMatch::None;
  }
  let Some(normalized) = normalize_path(path) else {
    return CatalogMatch::None;
  };

  let mut urls = Vec::new();
  for entry in entries() {
    if entry.file_match.iter().any(|pattern| pattern_matches(&normalized, pattern)) && !urls.contains(&entry.url) {
      urls.push(entry.url.clone());
    }
  }

  match urls.as_slice() {
    [] => CatalogMatch::None,
    [url] => CatalogMatch::One(url.clone()),
    _ => CatalogMatch::Ambiguous(urls),
  }
}

fn is_json_family(path: &Path) -> bool {
  kind::detect(path).language() == Some("json")
}

fn normalize_path(path: &Path) -> Option<String> {
  let raw = path.to_str()?;
  Some(raw.replace('\\', "/"))
}

fn pattern_matches(path: &str, pattern: &str) -> bool {
  let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
  if pattern.is_empty() || pattern.starts_with('!') {
    return false;
  }
  let path_bytes = path.as_bytes();
  let pattern_bytes = pattern.as_bytes();
  glob_match(path_bytes, pattern_bytes) || prefixed_glob_match(path_bytes, pattern_bytes)
}

/// `SchemaStore` / VS Code: a pattern also matches any ancestor prefix (`**/pattern`).
fn prefixed_glob_match(path: &[u8], pattern: &[u8]) -> bool {
  if pattern.starts_with(b"**/") {
    return false;
  }
  let mut index = 0_usize;
  while index < path.len() {
    if path.get(index) == Some(&b'/') {
      let rest = path.get(index.saturating_add(1)..).unwrap_or(&[]);
      if glob_match(rest, pattern) {
        return true;
      }
    }
    index = index.saturating_add(1);
  }
  false
}

fn glob_match(text: &[u8], pattern: &[u8]) -> bool {
  glob_match_at(text, 0, pattern, 0)
}

fn glob_match_at(text: &[u8], text_index: usize, pattern: &[u8], pattern_index: usize) -> bool {
  if pattern_index == pattern.len() {
    return text_index == text.len();
  }
  let Some(&token) = pattern.get(pattern_index) else {
    return false;
  };
  if token == b'*' {
    if pattern.get(pattern_index.saturating_add(1)) == Some(&b'*') {
      return globstar(text, text_index, pattern, pattern_index);
    }
    return star(text, text_index, pattern, pattern_index);
  }
  if token == b'?' {
    return match text.get(text_index) {
      Some(&b'/') | None => false,
      Some(_) => glob_match_at(text, text_index.saturating_add(1), pattern, pattern_index.saturating_add(1)),
    };
  }
  match text.get(text_index) {
    Some(&byte) if byte == token => {
      glob_match_at(text, text_index.saturating_add(1), pattern, pattern_index.saturating_add(1))
    },
    _ => false,
  }
}

fn star(text: &[u8], text_index: usize, pattern: &[u8], pattern_index: usize) -> bool {
  let next_pattern = pattern_index.saturating_add(1);
  let mut index = text_index;
  loop {
    if glob_match_at(text, index, pattern, next_pattern) {
      return true;
    }
    match text.get(index) {
      Some(&b'/') | None => return false,
      Some(_) => index = index.saturating_add(1),
    }
  }
}

fn globstar(text: &[u8], text_index: usize, pattern: &[u8], pattern_index: usize) -> bool {
  let mut next_pattern = pattern_index.saturating_add(2);
  while pattern.get(next_pattern) == Some(&b'*') {
    next_pattern = next_pattern.saturating_add(1);
  }
  if pattern.get(next_pattern) == Some(&b'/') {
    let after_slash = next_pattern.saturating_add(1);
    if glob_match_at(text, text_index, pattern, after_slash) {
      return true;
    }
  }
  let mut index = text_index;
  loop {
    if glob_match_at(text, index, pattern, next_pattern) {
      return true;
    }
    if index == text.len() {
      return false;
    }
    index = index.saturating_add(1);
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::{CatalogMatch, entries, glob_match, match_filename, pattern_matches, schemas};

  #[test]
  fn bundled_catalog_parses() {
    assert!(!entries().is_empty());
  }

  #[test]
  fn schemas_list_package_json() {
    assert!(
      schemas()
        .iter()
        .any(|schema| schema.url == "https://www.schemastore.org/package.json" && schema.name == "package.json")
    );
  }

  #[test]
  fn package_json_is_one_match() {
    assert_eq!(
      match_filename(Path::new("package.json")),
      CatalogMatch::One(String::from("https://www.schemastore.org/package.json"))
    );
    assert_eq!(
      match_filename(Path::new("src/package.json")),
      CatalogMatch::One(String::from("https://www.schemastore.org/package.json"))
    );
  }

  #[test]
  fn manifest_json_is_ambiguous() {
    let CatalogMatch::Ambiguous(urls) = match_filename(Path::new("manifest.json")) else {
      panic!("expected several catalog hits");
    };
    assert!(urls.len() >= 2, "got {urls:?}");
  }

  #[test]
  fn unknown_json_name_is_none() {
    assert_eq!(match_filename(Path::new("zzzz-openit-unknown.json")), CatalogMatch::None);
  }

  #[test]
  fn non_json_family_does_not_consult_the_catalog() {
    assert_eq!(match_filename(Path::new("package.toml")), CatalogMatch::None);
    assert_eq!(match_filename(Path::new("README.md")), CatalogMatch::None);
  }

  #[test]
  fn jsonc_filename_still_matches() {
    let CatalogMatch::One(url) = match_filename(Path::new("deno.jsonc")) else {
      panic!("expected one catalog hit");
    };
    assert!(url.contains("deno"), "{url}");
  }

  #[test]
  fn glob_patterns_need_their_directories() {
    assert!(pattern_matches("package.json", "package.json"));
    assert!(pattern_matches("src/package.json", "package.json"));
    assert!(!pattern_matches("package.json", "**/cassettes/*.json"));
    assert!(pattern_matches("foo/cassettes/bar.json", "**/cassettes/*.json"));
    assert!(!glob_match(b"io-package.json", b"package.json"));
  }
}
