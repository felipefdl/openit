//! Schema selection for JSON-family documents.
//!
//! Order: a manual pick (local file or URL), then `$schema` on the parsed value,
//! then an unambiguous catalog `fileMatch`.

use std::path::{Path, PathBuf};

use url::Url;

use crate::catalog::{self, CatalogMatch};
use crate::resource::{self, DenyReason, Resolved};

/// How a JSON-family document chose its schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaSelection {
  /// A local schema file.
  Local(PathBuf),
  /// A remote schema URL.
  Remote(Url),
  /// Several catalog hits; the caller should ask which to use.
  Ask(Vec<String>),
  /// No schema applies.
  None,
  /// The chosen reference cannot be resolved.
  Denied(DenyReason),
}

impl SchemaSelection {
  /// Quiet status label: the schema file name, or "No schema".
  pub fn status_name(&self) -> String {
    match self {
      Self::Local(path) => path
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| "schema".to_owned(), ToOwned::to_owned),
      Self::Remote(url) => remote_status_name(url),
      Self::Ask(_) | Self::None | Self::Denied(_) => "No schema".to_owned(),
    }
  }
}

fn remote_status_name(url: &Url) -> String {
  if let Some(name) = url
    .path_segments()
    .and_then(|segments| segments.rev().find(|part| !part.is_empty()))
  {
    return name.to_owned();
  }
  url.host_str().unwrap_or("schema").to_owned()
}

/// Choose a schema from `manual`, then `$schema`, then the catalog.
///
/// `path` is `None` for an untitled document. Relative local references then
/// become [`DenyReason::NoLocalBase`]. An http(s) `$schema` still selects.
pub fn select(path: Option<&Path>, value: &serde_json::Value, manual: Option<&str>) -> SchemaSelection {
  let base_dir = path.and_then(Path::parent);
  if let Some(reference) = manual {
    return from_reference(reference, base_dir);
  }
  if let Some(reference) = schema_keyword(value) {
    return from_reference(reference, base_dir);
  }
  let Some(path) = path else {
    return SchemaSelection::None;
  };
  match catalog::match_filename(path) {
    CatalogMatch::One(url) => from_reference(&url, base_dir),
    CatalogMatch::Ambiguous(urls) => SchemaSelection::Ask(urls),
    CatalogMatch::None => SchemaSelection::None,
  }
}

fn schema_keyword(value: &serde_json::Value) -> Option<&str> {
  value.get("$schema").and_then(serde_json::Value::as_str)
}

fn from_reference(reference: &str, base_dir: Option<&Path>) -> SchemaSelection {
  match resource::resolve(reference, base_dir) {
    Resolved::Local(path) => SchemaSelection::Local(path),
    Resolved::Remote(url) => SchemaSelection::Remote(url),
    Resolved::Denied(reason) => SchemaSelection::Denied(reason),
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::{SchemaSelection, select};
  use crate::resource::DenyReason;

  fn remote(url: &str) -> SchemaSelection {
    SchemaSelection::Remote(url::Url::parse(url).unwrap())
  }

  #[test]
  fn manual_wins_over_schema_keyword() {
    let value = serde_json::json!({ "$schema": "https://example.com/from-keyword.json" });
    let selection = select(Some(Path::new("package.json")), &value, Some("https://example.com/manual.json"));
    assert_eq!(selection, remote("https://example.com/manual.json"));
  }

  #[test]
  fn schema_keyword_wins_over_unambiguous_catalog() {
    let value = serde_json::json!({ "$schema": "https://example.com/from-keyword.json" });
    let selection = select(Some(Path::new("package.json")), &value, None);
    assert_eq!(selection, remote("https://example.com/from-keyword.json"));
  }

  #[test]
  fn one_catalog_hit_without_schema_keyword() {
    let value = serde_json::json!({ "name": "x" });
    let selection = select(Some(Path::new("package.json")), &value, None);
    assert_eq!(selection, remote("https://www.schemastore.org/package.json"));
  }

  #[test]
  fn two_catalog_hits_ask() {
    let value = serde_json::json!({});
    let SchemaSelection::Ask(urls) = select(Some(Path::new("manifest.json")), &value, None) else {
      panic!("expected several catalog hits");
    };
    assert!(urls.len() >= 2, "got {urls:?}");
  }

  #[test]
  fn untitled_relative_schema_is_denied() {
    let value = serde_json::json!({ "$schema": "./schema.json" });
    let selection = select(None, &value, None);
    assert_eq!(selection, SchemaSelection::Denied(DenyReason::NoLocalBase));
  }

  #[test]
  fn untitled_https_schema_selects() {
    let value = serde_json::json!({ "$schema": "https://example.com/schema.json" });
    let selection = select(None, &value, None);
    assert_eq!(selection, remote("https://example.com/schema.json"));
  }

  #[test]
  fn no_match_and_no_schema_is_none() {
    let value = serde_json::json!({});
    let selection = select(Some(Path::new("zzzz-openit-unknown.json")), &value, None);
    assert_eq!(selection, SchemaSelection::None);
  }

  #[test]
  fn status_name_is_the_schema_file_or_no_schema() {
    assert_eq!(remote("https://www.schemastore.org/package.json").status_name(), "package.json");
    assert_eq!(
      SchemaSelection::Local(Path::new("/tmp/local.schema.json").to_path_buf()).status_name(),
      "local.schema.json"
    );
    assert_eq!(SchemaSelection::None.status_name(), "No schema");
    assert_eq!(SchemaSelection::Ask(vec!["a".into(), "b".into()]).status_name(), "No schema");
  }
}
