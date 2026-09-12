//! JSON-family parse with source ranges, `$ref` targets for schema loading, schema validation, and completions. No rewrite.

mod complete;
mod validate;

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use jsonc_parser::ast::Value as AstValue;
use jsonc_parser::common::Ranged as _;
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};

use crate::resource::{self, Resolved};

pub use complete::{CompletionKind, SchemaCompletion, completions, schema_keyword};
pub use validate::{CompileError, CompiledSchema, SchemaDiagnostic, SchemaDocuments, SchemaIdentity};
/// JSON dialect that selects [`ParseOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonFamily {
  /// Strict JSON. Every parse flag is off.
  Json,
  /// Comments and trailing commas only.
  Jsonc,
  /// Same allowances as [`JsonFamily::Jsonc`].
  Json5,
}

impl JsonFamily {
  /// Dialect for a path's extension, if it is JSON-family.
  pub fn from_path(path: &Path) -> Option<Self> {
    path.extension().and_then(|ext| ext.to_str()).and_then(Self::from_extension)
  }

  /// Dialect for a filename extension, if it is JSON-family.
  pub const fn from_extension(ext: &str) -> Option<Self> {
    if ext.eq_ignore_ascii_case("json") {
      Some(Self::Json)
    } else if ext.eq_ignore_ascii_case("jsonc") {
      Some(Self::Jsonc)
    } else if ext.eq_ignore_ascii_case("json5") {
      Some(Self::Json5)
    } else {
      None
    }
  }
}

/// Byte offsets (`start` inclusive, `end` exclusive) in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRange {
  /// First byte of the span.
  pub start: usize,
  /// Byte after the span.
  pub end: usize,
}

impl SourceRange {
  /// Slice of `text` covered by this range, when the offsets are in bounds.
  pub fn slice(self, text: &str) -> Option<&str> {
    text.get(self.start..self.end)
  }
}

/// A successful parse: the value plus JSON-pointer ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedJson {
  /// Document value. Comments and trailing commas are not present.
  pub value: serde_json::Value,
  /// JSON Pointer (RFC 6901) to the source range of that node.
  ///
  /// Object members use the property span (name through value).
  pub ranges: BTreeMap<String, SourceRange>,
}

impl ParsedJson {
  /// Range stored for `pointer`, when the document has that node.
  pub fn range(&self, pointer: &str) -> Option<SourceRange> {
    self.ranges.get(pointer).copied()
  }
}

/// Syntax failure. `range` covers the bad token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SyntaxError {
  /// Span of the unexpected token, or the whole text when no value parsed.
  pub range: SourceRange,
  /// Parser message.
  pub message: String,
}

/// Parse `text` in `family`. The source is not rewritten.
pub fn parse(family: JsonFamily, text: &str) -> Result<ParsedJson, SyntaxError> {
  let parsed = parse_to_ast(text, &CollectOptions::default(), &parse_options(family)).map_err(|error| SyntaxError {
    range: source_range(error.range()),
    message: error.to_string(),
  })?;
  let Some(root) = parsed.value else {
    return Err(SyntaxError {
      range: SourceRange { start: 0, end: text.len() },
      message: "document has no JSON value".to_owned(),
    });
  };
  let mut ranges = BTreeMap::new();
  let value = collect(&root, "", source_range(root.range()), &mut ranges)?;
  Ok(ParsedJson { value, ranges })
}

const fn parse_options(family: JsonFamily) -> ParseOptions {
  match family {
    JsonFamily::Json => ParseOptions {
      allow_comments: false,
      allow_loose_object_property_names: false,
      allow_trailing_commas: false,
      allow_missing_commas: false,
      allow_single_quoted_strings: false,
      allow_hexadecimal_numbers: false,
      allow_unary_plus_numbers: false,
    },
    JsonFamily::Jsonc | JsonFamily::Json5 => ParseOptions {
      allow_comments: true,
      allow_loose_object_property_names: false,
      allow_trailing_commas: true,
      allow_missing_commas: false,
      allow_single_quoted_strings: false,
      allow_hexadecimal_numbers: false,
      allow_unary_plus_numbers: false,
    },
  }
}

fn collect(
  value: &AstValue<'_>,
  pointer: &str,
  range: SourceRange,
  ranges: &mut BTreeMap<String, SourceRange>,
) -> Result<serde_json::Value, SyntaxError> {
  ranges.insert(pointer.to_owned(), range);
  match value {
    AstValue::StringLit(lit) => Ok(serde_json::Value::String(lit.value.as_ref().to_owned())),
    AstValue::NumberLit(lit) => {
      let number = serde_json::Number::from_str(lit.value).map_err(|_| SyntaxError {
        range: source_range(lit.range),
        message: format!("invalid number {}", lit.value),
      })?;
      Ok(serde_json::Value::Number(number))
    },
    AstValue::BooleanLit(lit) => Ok(serde_json::Value::Bool(lit.value)),
    AstValue::NullKeyword(_) => Ok(serde_json::Value::Null),
    AstValue::Object(object) => {
      let mut map = serde_json::Map::with_capacity(object.properties.len());
      for property in &object.properties {
        let name = property.name.as_str();
        let child = append_pointer(pointer, name);
        let child_value = collect(&property.value, &child, source_range(property.range), ranges)?;
        map.insert(name.to_owned(), child_value);
      }
      Ok(serde_json::Value::Object(map))
    },
    AstValue::Array(array) => {
      let mut elements = Vec::with_capacity(array.elements.len());
      for (index, element) in array.elements.iter().enumerate() {
        let child = append_pointer(pointer, &index.to_string());
        elements.push(collect(element, &child, source_range(element.range()), ranges)?);
      }
      Ok(serde_json::Value::Array(elements))
    },
  }
}

fn append_pointer(parent: &str, token: &str) -> String {
  let mut out = String::with_capacity(parent.len() + token.len() + 1);
  out.push_str(parent);
  out.push('/');
  for ch in token.chars() {
    match ch {
      '~' => out.push_str("~0"),
      '/' => out.push_str("~1"),
      _ => out.push(ch),
    }
  }
  out
}

const fn source_range(range: jsonc_parser::common::Range) -> SourceRange {
  SourceRange { start: range.start, end: range.end }
}

/// Maximum `$ref` document hops loaded for one schema. Each hop may fetch.
pub const REF_DEPTH: u32 = 8;

/// `$ref` values that name another document. Same-document fragments are omitted
/// when resolving; they still appear here so the caller can skip them.
pub fn document_refs(value: &serde_json::Value) -> Vec<String> {
  let mut refs = Vec::new();
  collect_refs(value, &mut refs);
  refs
}

fn collect_refs(value: &serde_json::Value, refs: &mut Vec<String>) {
  match value {
    serde_json::Value::Object(map) => {
      if let Some(serde_json::Value::String(reference)) = map.get("$ref") {
        refs.push(reference.clone());
      }
      for nested in map.values() {
        collect_refs(nested, refs);
      }
    },
    serde_json::Value::Array(items) => {
      for item in items {
        collect_refs(item, refs);
      }
    },
    serde_json::Value::Null
    | serde_json::Value::Bool(_)
    | serde_json::Value::Number(_)
    | serde_json::Value::String(_) => {},
  }
}

/// Resolve a `$ref` against the schema document it was found in.
///
/// A fragment-only reference (`#/…`) stays in the current document and yields
/// `None`, so it is not a fetch. The fragment on an external `$ref` is stripped
/// before resolution.
pub fn resolve_ref(reference: &str, base: &Resolved) -> Option<Resolved> {
  let document = strip_fragment(reference);
  if document.is_empty() {
    return None;
  }
  match base {
    Resolved::Remote(url) => {
      let joined = url.join(document).ok()?;
      Some(resource::resolve(joined.as_str(), None))
    },
    Resolved::Local(path) => Some(resource::resolve(document, path.parent())),
    Resolved::Denied(_) => None,
  }
}

fn strip_fragment(reference: &str) -> &str {
  reference.split_once('#').map_or(reference, |(head, _)| head)
}

#[cfg(test)]
mod tests {
  use super::{JsonFamily, parse};

  const WITH_COMMENT_AND_COMMA: &str = "{\n  // title\n  \"name\": \"Ada\",\n}\n";
  const NAME_DOCUMENT: &str = "{ \"name\": \"Ada\" }";

  fn slice(text: &str, start: usize, end: usize) -> &str {
    text.get(start..end).expect("range in bounds")
  }

  #[test]
  fn strict_json_rejects_a_comment() {
    let text = "{ \"name\": \"Ada\" } // trailing";
    let error = parse(JsonFamily::Json, text).expect_err("comment is illegal in json");
    assert!(
      slice(text, error.range.start, error.range.end).contains("//"),
      "error range {:?} does not cover the comment",
      error.range
    );
  }

  #[test]
  fn jsonc_allows_a_comment_and_trailing_comma() {
    let parsed = parse(JsonFamily::Jsonc, WITH_COMMENT_AND_COMMA).expect("jsonc");
    assert_eq!(parsed.value["name"], "Ada");
  }

  #[test]
  fn json5_allows_a_comment_and_trailing_comma() {
    let parsed = parse(JsonFamily::Json5, WITH_COMMENT_AND_COMMA).expect("json5");
    assert_eq!(parsed.value["name"], "Ada");
  }

  #[test]
  fn syntax_error_range_covers_the_bad_token() {
    let text = "{ \"name\": tru }";
    let error = parse(JsonFamily::Json, text).expect_err("tru is not a value");
    assert!(
      slice(text, error.range.start, error.range.end).contains("tru"),
      "error range {:?} does not cover tru",
      error.range
    );
  }

  #[test]
  fn name_pointer_range_covers_that_property() {
    let parsed = parse(JsonFamily::Json, NAME_DOCUMENT).expect("json");
    let range = parsed.range("/name").expect("/name");
    let covered = slice(NAME_DOCUMENT, range.start, range.end);
    assert!(covered.contains("name"), "{covered:?} does not cover the name property");
  }

  #[test]
  fn document_refs_collect_nested_and_array_values() {
    let value = serde_json::json!({
      "$ref": "root.json",
      "properties": {
        "child": { "$ref": "child.json#/defs/item" }
      },
      "allOf": [{ "$ref": "#/defs/local" }]
    });
    let mut refs = super::document_refs(&value);
    refs.sort();
    assert_eq!(refs, ["#/defs/local", "child.json#/defs/item", "root.json"]);
  }

  #[test]
  fn resolve_ref_skips_same_document_fragments() {
    let base = crate::resource::resolve("https://example.com/schema.json", None);
    assert_eq!(super::resolve_ref("#/defs/item", &base), None);
    assert_eq!(super::resolve_ref("#", &base), None);
  }

  #[test]
  fn resolve_ref_joins_remote_and_local_documents() {
    let remote = crate::resource::resolve("https://example.com/schemas/root.json", None);
    assert_eq!(
      super::resolve_ref("defs.json#/item", &remote),
      Some(crate::resource::resolve("https://example.com/schemas/defs.json", None))
    );

    let dir = tempfile::tempdir().unwrap();
    let schema = dir.path().join("schema.json");
    let base = crate::resource::Resolved::Local(schema);
    let expected = dir.path().join("defs.json");
    assert_eq!(
      super::resolve_ref("defs.json", &base),
      Some(crate::resource::Resolved::Local(expected))
    );
  }
}
