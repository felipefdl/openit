//! Schema-driven completions for the JSON family. No GPUI.

use std::collections::BTreeSet;

use jsonc_parser::errors::ParseErrorKind;
use jsonc_parser::tokens::Token;
use jsonc_parser::{Scanner, ScannerOptions};
use serde_json::Value;

use super::validate::{document_uri, resolve_ref};
use super::{JsonFamily, REF_DEPTH, SchemaDocuments, SourceRange};

/// Property name or `enum` value from the loaded schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCompletion {
  /// Menu label. Property name, or the JSON form of an `enum` value.
  pub label: String,
  /// Schema type, or `"enum"`.
  pub detail: Option<String>,
  /// Whether this item is a property name or an `enum` member.
  pub kind: CompletionKind,
  /// Source span to replace with [`Self::insert`].
  pub replace: SourceRange,
  /// Plain insert text. Never a snippet.
  pub insert: String,
}

/// What the cursor is completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
  /// Object property name.
  Property,
  /// Schema `enum` member.
  Enum,
}

/// Completions at `offset` in `text` from `documents`. No schema yields no items.
pub fn completions(
  family: JsonFamily,
  text: &str,
  offset: usize,
  documents: Option<&SchemaDocuments>,
) -> Vec<SchemaCompletion> {
  let Some(documents) = documents else {
    return Vec::new();
  };
  let Some(cursor) = locate(family, text, offset) else {
    return Vec::new();
  };
  match cursor.slot {
    Slot::Property => property_items(documents, &cursor.path, cursor.replace),
    Slot::Value => enum_items(documents, &cursor.path, cursor.replace),
  }
}

/// Root `$schema` string, including when the rest of the document does not parse.
pub fn schema_keyword(text: &str) -> Option<String> {
  let mut scanner = Scanner::new(text, &SCANNER_OPTIONS);
  let mut depth: u32 = 0;
  let mut expect_key = false;
  let mut take_schema = false;
  loop {
    match scanner.scan() {
      Ok(Some(Token::CommentLine(_) | Token::CommentBlock(_))) => {},
      Ok(Some(Token::OpenBrace)) => {
        depth = depth.saturating_add(1);
        if depth == 1 {
          expect_key = true;
        }
        take_schema = false;
      },
      Ok(Some(Token::OpenBracket)) => {
        depth = depth.saturating_add(1);
        expect_key = false;
        take_schema = false;
      },
      Ok(Some(Token::CloseBrace | Token::CloseBracket)) => {
        depth = depth.saturating_sub(1);
        expect_key = false;
        take_schema = false;
        if depth == 0 {
          return None;
        }
      },
      Ok(Some(Token::Comma)) => {
        expect_key = depth == 1;
        take_schema = false;
      },
      Ok(Some(Token::Colon)) => expect_key = false,
      Ok(Some(Token::String(value))) => {
        if take_schema {
          return Some(value.into_owned());
        }
        if depth == 1 && expect_key {
          take_schema = value.as_ref() == "$schema";
          expect_key = false;
        } else {
          take_schema = false;
        }
      },
      Ok(Some(Token::Word(value))) => {
        if take_schema {
          return Some(value.to_owned());
        }
        if depth == 1 && expect_key {
          take_schema = value == "$schema";
          expect_key = false;
        } else {
          take_schema = false;
        }
      },
      Ok(Some(_)) => {
        take_schema = false;
        expect_key = false;
      },
      Ok(None) | Err(_) => return None,
    }
  }
}

#[derive(Debug, Clone, Copy)]
enum Slot {
  Property,
  Value,
}

struct Cursor {
  path: Vec<String>,
  slot: Slot,
  replace: SourceRange,
}

fn property_items(documents: &SchemaDocuments, path: &[String], replace: SourceRange) -> Vec<SchemaCompletion> {
  let Some(schema) = schema_at(documents, path) else {
    return Vec::new();
  };
  let mut names = BTreeSet::new();
  let mut items = Vec::new();
  for node in flatten(documents, schema) {
    let Some(properties) = node.value.get("properties").and_then(Value::as_object) else {
      continue;
    };
    for (name, child) in properties {
      if !names.insert(name.clone()) {
        continue;
      }
      items.push(SchemaCompletion {
        label: name.clone(),
        detail: type_detail(BoundSchema { base: node.base, value: child }),
        kind: CompletionKind::Property,
        replace,
        insert: json_string(name),
      });
    }
  }
  items
}

#[derive(Clone, Copy)]
struct BoundSchema<'a> {
  base: &'a str,
  value: &'a Value,
}

fn enum_items(documents: &SchemaDocuments, path: &[String], replace: SourceRange) -> Vec<SchemaCompletion> {
  let Some(schema) = schema_at(documents, path) else {
    return Vec::new();
  };
  let mut seen = BTreeSet::new();
  let mut items = Vec::new();
  for node in flatten(documents, schema) {
    let Some(values) = node.value.get("enum").and_then(Value::as_array) else {
      continue;
    };
    for value in values {
      let insert = value.to_string();
      if !seen.insert(insert.clone()) {
        continue;
      }
      items.push(SchemaCompletion {
        label: insert.clone(),
        detail: Some("enum".to_owned()),
        kind: CompletionKind::Enum,
        replace,
        insert,
      });
    }
  }
  items
}

fn schema_at<'a>(documents: &'a SchemaDocuments, path: &[String]) -> Option<BoundSchema<'a>> {
  let root = documents.documents.get(&documents.root)?;
  let mut current = BoundSchema {
    base: documents.root.as_str(),
    value: root,
  };
  current = follow(documents, current)?;
  for segment in path {
    current = child_schema(documents, current, segment)?;
  }
  Some(current)
}

fn child_schema<'a>(documents: &'a SchemaDocuments, parent: BoundSchema<'a>, segment: &str) -> Option<BoundSchema<'a>> {
  let parent = follow(documents, parent)?;
  if let Some(properties) = parent.value.get("properties").and_then(Value::as_object)
    && let Some(child) = properties.get(segment)
  {
    return follow(documents, BoundSchema { base: parent.base, value: child });
  }
  if parent.value.get("items").is_some() && segment.bytes().all(|b| b.is_ascii_digit()) {
    return items_schema(documents, parent, segment);
  }
  for node in flatten(documents, parent) {
    if let Some(properties) = node.value.get("properties").and_then(Value::as_object)
      && let Some(child) = properties.get(segment)
    {
      return follow(documents, BoundSchema { base: node.base, value: child });
    }
    if node.value.get("items").is_some() && segment.bytes().all(|b| b.is_ascii_digit()) {
      return items_schema(documents, node, segment);
    }
  }
  None
}

fn items_schema<'a>(documents: &'a SchemaDocuments, parent: BoundSchema<'a>, index: &str) -> Option<BoundSchema<'a>> {
  match parent.value.get("items")? {
    Value::Array(prefix) => {
      let parsed = index.parse::<usize>().ok()?;
      prefix
        .get(parsed)
        .or_else(|| prefix.last())
        .and_then(|item| follow(documents, BoundSchema { base: parent.base, value: item }))
    },
    other => follow(documents, BoundSchema { base: parent.base, value: other }),
  }
}

fn flatten<'a>(documents: &'a SchemaDocuments, schema: BoundSchema<'a>) -> Vec<BoundSchema<'a>> {
  let mut out = Vec::new();
  push_flat(documents, schema, 0, &mut out);
  out
}

fn push_flat<'a>(documents: &'a SchemaDocuments, schema: BoundSchema<'a>, depth: u32, out: &mut Vec<BoundSchema<'a>>) {
  if depth > REF_DEPTH {
    return;
  }
  let Some(resolved) = follow(documents, schema) else {
    return;
  };
  out.push(resolved);
  for key in ["allOf", "anyOf", "oneOf"] {
    let Some(list) = resolved.value.get(key).and_then(Value::as_array) else {
      continue;
    };
    for item in list {
      push_flat(
        documents,
        BoundSchema { base: resolved.base, value: item },
        depth.saturating_add(1),
        out,
      );
    }
  }
}

fn follow<'a>(documents: &'a SchemaDocuments, schema: BoundSchema<'a>) -> Option<BoundSchema<'a>> {
  follow_depth(documents, schema, 0)
}

fn follow_depth<'a>(documents: &'a SchemaDocuments, schema: BoundSchema<'a>, depth: u32) -> Option<BoundSchema<'a>> {
  if depth > REF_DEPTH {
    return None;
  }
  let Some(reference) = schema.value.get("$ref").and_then(Value::as_str) else {
    return Some(schema);
  };
  let resolved = resolve_ref(schema.base, reference).ok()?;
  let (base, root) = lookup(documents, &resolved)?;
  let target = fragment(&resolved)
    .and_then(|pointer| pointer_value(root, pointer))
    .unwrap_or(root);
  follow_depth(documents, BoundSchema { base, value: target }, depth.saturating_add(1))
}

fn lookup<'a>(documents: &'a SchemaDocuments, uri: &str) -> Option<(&'a str, &'a Value)> {
  let target = document_uri(uri);
  documents
    .documents
    .iter()
    .find_map(|(key, value)| (document_uri(key) == target).then_some((key.as_str(), value)))
}

fn fragment(uri: &str) -> Option<&str> {
  uri.split_once('#').map(|(_, rest)| rest).filter(|rest| !rest.is_empty())
}

fn pointer_value<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
  let mut current = value;
  for token in pointer.split('/').filter(|token| !token.is_empty()) {
    let decoded = decode_pointer(token);
    current = match current {
      Value::Object(map) => map.get(&decoded)?,
      Value::Array(items) => items.get(decoded.parse::<usize>().ok()?)?,
      _ => return None,
    };
  }
  Some(current)
}

fn decode_pointer(token: &str) -> String {
  token.replace("~1", "/").replace("~0", "~")
}

fn type_detail(schema: BoundSchema<'_>) -> Option<String> {
  if schema.value.get("enum").is_some() {
    return Some("enum".to_owned());
  }
  match schema.value.get("type") {
    Some(Value::String(kind)) => Some(kind.clone()),
    Some(Value::Array(kinds)) => {
      let joined = kinds.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | ");
      (!joined.is_empty()).then_some(joined)
    },
    _ => None,
  }
}

fn json_string(name: &str) -> String {
  Value::String(name.to_owned()).to_string()
}

const SCANNER_OPTIONS: ScannerOptions = ScannerOptions {
  allow_single_quoted_strings: false,
  allow_hexadecimal_numbers: false,
  allow_unary_plus_numbers: false,
};

fn locate(_family: JsonFamily, text: &str, offset: usize) -> Option<Cursor> {
  let offset = offset.min(text.len());
  let mut walker = Walker { text, offset, stack: Vec::new() };
  let mut scanner = Scanner::new(text, &SCANNER_OPTIONS);
  loop {
    match scanner.scan() {
      Ok(Some(token)) => {
        let range = SourceRange {
          start: scanner.token_start(),
          end: scanner.token_end(),
        };
        if range.start >= offset {
          return walker.cursor_at_gap();
        }
        if let Some(cursor) = walker.take_token(token, range) {
          return Some(cursor);
        }
      },
      Ok(None) => return walker.cursor_at_gap(),
      Err(error) => return walker.take_error(error.kind(), error.range().start),
    }
  }
}

struct Walker<'a> {
  text: &'a str,
  offset: usize,
  stack: Vec<Frame>,
}

struct Frame {
  kind: FrameKind,
  expect: Expect,
  key: Option<String>,
  segment: Option<String>,
}

enum FrameKind {
  Object,
  Array { next_index: usize },
}

#[derive(Clone, Copy)]
enum Expect {
  Key,
  Colon,
  Value,
  AfterValue,
}

impl Walker<'_> {
  fn take_token(&mut self, token: Token<'_>, range: SourceRange) -> Option<Cursor> {
    if matches!(token, Token::CommentLine(_) | Token::CommentBlock(_)) {
      return None;
    }
    if offset_inside(range, self.offset) {
      return self.cursor_in_token(&token, range);
    }
    self.apply(token, range);
    None
  }

  fn take_error(&self, kind: &ParseErrorKind, start: usize) -> Option<Cursor> {
    let unterminated = matches!(
      kind,
      ParseErrorKind::String(jsonc_parser::ParseStringErrorKind::UnterminatedStringLiteral)
    );
    if !unterminated || start >= self.offset {
      return self.cursor_at_gap();
    }
    let replace = SourceRange { start, end: self.offset };
    match self.top_expect() {
      Some(Expect::Key | Expect::Colon) => Some(Cursor {
        path: self.object_path(),
        slot: Slot::Property,
        replace,
      }),
      Some(Expect::Value) => Some(Cursor {
        path: self.value_path(),
        slot: Slot::Value,
        replace,
      }),
      Some(Expect::AfterValue) | None => None,
    }
  }

  fn cursor_in_token(&self, token: &Token<'_>, range: SourceRange) -> Option<Cursor> {
    match token {
      Token::String(_) | Token::Word(_) => match self.top_expect() {
        Some(Expect::Key | Expect::Colon) => Some(Cursor {
          path: self.object_path(),
          slot: Slot::Property,
          replace: self.clamp_to_caret(range),
        }),
        Some(Expect::Value) => Some(Cursor {
          path: self.value_path(),
          slot: Slot::Value,
          replace: self.clamp_to_caret(range),
        }),
        Some(Expect::AfterValue) | None => None,
      },
      Token::OpenBrace => Some(Cursor {
        path: self.child_object_path(),
        slot: Slot::Property,
        replace: SourceRange { start: self.offset, end: self.offset },
      }),
      Token::OpenBracket | Token::Colon => Some(Cursor {
        path: self.value_path(),
        slot: Slot::Value,
        replace: SourceRange { start: self.offset, end: self.offset },
      }),
      Token::Number(_) | Token::Boolean(_) | Token::Null => match self.top_expect() {
        Some(Expect::Value) => Some(Cursor {
          path: self.value_path(),
          slot: Slot::Value,
          replace: range,
        }),
        _ => None,
      },
      _ => self.cursor_at_gap(),
    }
  }

  /// A string token that swallowed a newline (an unterminated `"` followed by
  /// the next line) only replaces up to the caret, so the next line stays. A
  /// token on one line is replaced whole.
  fn clamp_to_caret(&self, range: SourceRange) -> SourceRange {
    let spans_newline = self.text.get(range.start..range.end).is_some_and(|slice| slice.contains('\n'));
    if !spans_newline {
      return range;
    }
    SourceRange {
      start: range.start,
      end: range.end.min(self.offset.max(range.start)),
    }
  }

  fn cursor_at_gap(&self) -> Option<Cursor> {
    let replace = SourceRange { start: self.offset, end: self.offset };
    match self.top_expect() {
      Some(Expect::Key | Expect::Colon) => Some(Cursor {
        path: self.object_path(),
        slot: Slot::Property,
        replace,
      }),
      Some(Expect::Value) => Some(Cursor {
        path: self.value_path(),
        slot: Slot::Value,
        replace,
      }),
      Some(Expect::AfterValue) | None => None,
    }
  }

  fn apply(&mut self, token: Token<'_>, range: SourceRange) {
    match token {
      Token::OpenBrace => self.open_object(),
      Token::OpenBracket => self.open_array(),
      Token::CloseBrace | Token::CloseBracket => self.close(),
      Token::Colon => self.set_expect(Expect::Value),
      Token::Comma => self.comma(),
      Token::String(value) => self.string_or_word(value.as_ref(), range),
      Token::Word(value) => self.string_or_word(value, range),
      Token::Number(_) | Token::Boolean(_) | Token::Null => self.finish_value(),
      Token::CommentLine(_) | Token::CommentBlock(_) => {},
    }
  }

  fn string_or_word(&mut self, value: &str, _range: SourceRange) {
    match self.top_expect() {
      Some(Expect::Key) => {
        if let Some(frame) = self.stack.last_mut() {
          frame.key = Some(value.to_owned());
          frame.expect = Expect::Colon;
        }
      },
      Some(Expect::Value) => self.finish_value(),
      Some(Expect::Colon | Expect::AfterValue) | None => {},
    }
  }

  fn open_object(&mut self) {
    let segment = self.value_segment();
    self.stack.push(Frame {
      kind: FrameKind::Object,
      expect: Expect::Key,
      key: None,
      segment,
    });
  }

  fn open_array(&mut self) {
    let segment = self.value_segment();
    self.stack.push(Frame {
      kind: FrameKind::Array { next_index: 0 },
      expect: Expect::Value,
      key: None,
      segment,
    });
  }

  fn close(&mut self) {
    self.stack.pop();
    self.finish_value();
  }

  fn comma(&mut self) {
    let Some(frame) = self.stack.last_mut() else {
      return;
    };
    match frame.kind {
      FrameKind::Object => {
        frame.key = None;
        frame.expect = Expect::Key;
      },
      FrameKind::Array { .. } => frame.expect = Expect::Value,
    }
  }

  fn finish_value(&mut self) {
    let Some(frame) = self.stack.last_mut() else {
      return;
    };
    frame.expect = Expect::AfterValue;
    if let FrameKind::Array { next_index } = &mut frame.kind {
      *next_index = next_index.saturating_add(1);
    }
  }

  fn set_expect(&mut self, expect: Expect) {
    if let Some(frame) = self.stack.last_mut() {
      frame.expect = expect;
    }
  }

  fn top_expect(&self) -> Option<Expect> {
    self.stack.last().map(|frame| frame.expect)
  }

  fn object_path(&self) -> Vec<String> {
    self.stack.iter().filter_map(|frame| frame.segment.clone()).collect()
  }

  fn child_object_path(&self) -> Vec<String> {
    let mut path = self.object_path();
    if let Some(segment) = self.value_segment() {
      path.push(segment);
    }
    path
  }

  fn value_path(&self) -> Vec<String> {
    let mut path = self.object_path();
    if let Some(segment) = self.value_segment() {
      path.push(segment);
    }
    path
  }

  fn value_segment(&self) -> Option<String> {
    let frame = self.stack.last()?;
    match &frame.kind {
      FrameKind::Object => frame.key.clone(),
      FrameKind::Array { next_index } => Some(next_index.to_string()),
    }
  }
}

const fn offset_inside(range: SourceRange, offset: usize) -> bool {
  range.start <= offset && offset < range.end
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use serde_json::{Value, json};

  use super::{CompletionKind, completions};
  use crate::schema::{JsonFamily, SchemaDocuments};

  const ROOT: &str = "https://openit.test/root.json";
  const PERSON: &str = "https://openit.test/person.json";

  fn documents(root: Value, extras: &[(&str, Value)]) -> SchemaDocuments {
    let mut documents = BTreeMap::new();
    documents.insert(ROOT.to_owned(), root);
    for (uri, value) in extras {
      documents.insert((*uri).to_owned(), value.clone());
    }
    SchemaDocuments { root: ROOT.to_owned(), documents }
  }

  fn labels(text: &str, needle: &str, schema: Value) -> Vec<String> {
    let offset = text.find(needle).unwrap() + needle.len();
    completions(JsonFamily::Json, text, offset, Some(&documents(schema, &[])))
      .into_iter()
      .map(|item| item.label)
      .collect()
  }

  fn kinds(text: &str, needle: &str, schema: Value) -> Vec<CompletionKind> {
    let offset = text.find(needle).unwrap() + needle.len();
    completions(JsonFamily::Json, text, offset, Some(&documents(schema, &[])))
      .into_iter()
      .map(|item| item.kind)
      .collect()
  }

  fn object_schema() -> Value {
    json!({
      "type": "object",
      "properties": {
        "status": { "enum": ["draft", "live"] },
        "author": {
          "type": "object",
          "properties": {
            "name": { "type": "string" },
            "email": { "type": "string" }
          }
        }
      }
    })
  }

  #[test]
  fn schema_keyword_from_incomplete_json() {
    let text = "{\n  \"$schema\": \"https://openit.test/root.json\",\n  \"status\": \n}\n";
    assert_eq!(super::schema_keyword(text).as_deref(), Some(ROOT));
  }

  #[test]
  fn no_schema_yields_no_items() {
    let items = completions(JsonFamily::Json, "{ ", 2, None);
    assert!(items.is_empty());
  }

  #[test]
  fn properties_at_the_root_object() {
    let text = "{\n  \n}\n";
    let found = labels(text, "{\n  ", object_schema());
    assert!(found.contains(&"status".to_owned()), "{found:?}");
    assert!(found.contains(&"author".to_owned()), "{found:?}");
  }

  #[test]
  fn properties_when_schema_and_status_are_present() {
    let url = "https://raw.githubusercontent.com/schema.json";
    let text = format!("{{\n  \"$schema\": \"{url}\",\n  \"status\": \n}}\n");
    let offset = text.find("{\n  ").unwrap() + "{\n  ".len();
    let found: Vec<String> = completions(JsonFamily::Json, &text, offset, Some(&documents(object_schema(), &[])))
      .into_iter()
      .map(|item| item.label)
      .collect();
    assert!(
      found.contains(&"status".to_owned()),
      "offset={offset} text={text:?} found={found:?}"
    );
    let value = text.find("\"status\": ").unwrap() + "\"status\": ".len();
    let enums: Vec<String> = completions(JsonFamily::Json, &text, value, Some(&documents(object_schema(), &[])))
      .into_iter()
      .map(|item| item.label)
      .collect();
    assert!(enums.contains(&"\"draft\"".to_owned()), "offset={value} enums={enums:?}");
  }

  #[test]
  fn nested_object_properties() {
    let text = "{\n  \"author\": {\n    \n  }\n}\n";
    let found = labels(text, "\"author\": {\n    ", object_schema());
    assert!(found.contains(&"name".to_owned()), "{found:?}");
    assert!(found.contains(&"email".to_owned()), "{found:?}");
    assert!(!found.contains(&"status".to_owned()), "{found:?}");
  }

  #[test]
  fn enum_values_at_a_property() {
    let text = "{\n  \"status\": \n}\n";
    let found = labels(text, "\"status\": ", object_schema());
    assert!(found.contains(&"\"draft\"".to_owned()), "{found:?}");
    assert!(found.contains(&"\"live\"".to_owned()), "{found:?}");
    assert!(
      kinds(text, "\"status\": ", object_schema())
        .iter()
        .all(|kind| *kind == CompletionKind::Enum)
    );
  }

  #[test]
  fn ref_in_the_registry_offers_nested_properties() {
    let schema = json!({
      "type": "object",
      "properties": {
        "person": { "$ref": "#/$defs/person" }
      },
      "$defs": {
        "person": {
          "type": "object",
          "properties": { "name": { "type": "string" } }
        }
      }
    });
    let text = "{\n  \"person\": {\n    \n  }\n}\n";
    let found = labels(text, "\"person\": {\n    ", schema);
    assert!(found.contains(&"name".to_owned()), "{found:?}");
  }

  #[test]
  fn remote_ref_in_the_registry_is_followed() {
    let root = json!({
      "type": "object",
      "properties": {
        "person": { "$ref": "https://openit.test/person.json" }
      }
    });
    let person = json!({
      "type": "object",
      "properties": { "name": { "type": "string" } }
    });
    let text = "{\n  \"person\": {\n    \n  }\n}\n";
    let offset = text.find("\"person\": {\n    ").unwrap() + "\"person\": {\n    ".len();
    let found: Vec<String> = completions(JsonFamily::Json, text, offset, Some(&documents(root, &[(PERSON, person)])))
      .into_iter()
      .map(|item| item.label)
      .collect();
    assert!(found.contains(&"name".to_owned()), "{found:?}");
  }

  #[test]
  fn missing_ref_does_not_offer_that_object() {
    let schema = json!({
      "type": "object",
      "properties": {
        "person": { "$ref": "https://openit.test/missing.json" }
      }
    });
    let text = "{\n  \"person\": {\n    \n  }\n}\n";
    let found = labels(text, "\"person\": {\n    ", schema);
    assert!(found.is_empty(), "{found:?}");
  }

  #[test]
  fn property_insert_is_plain_json_not_a_snippet() {
    let text = "{\n  \n}\n";
    let offset = text.find("{\n  ").unwrap() + "{\n  ".len();
    let items = completions(JsonFamily::Json, text, offset, Some(&documents(object_schema(), &[])));
    assert!(items.iter().all(|item| !item.insert.contains('$')), "{items:?}");
  }

  #[test]
  fn an_unterminated_quote_does_not_replace_the_next_line() {
    let text = "{\n  \"\n  \"status\": \"draft\"\n}\n";
    let offset = text.find("  \"\n").unwrap() + 3;
    let items = completions(JsonFamily::Json, text, offset, Some(&documents(object_schema(), &[])));
    let item = items.first().expect("properties offered");
    assert_eq!(text.get(item.replace.start..item.replace.end), Some("\""));
  }

  #[test]
  fn an_auto_closed_key_is_replaced_whole() {
    let text = "{\n  \"\"\n  \"status\": \"draft\"\n}\n";
    let offset = text.find("  \"\"").unwrap() + 3;
    let items = completions(JsonFamily::Json, text, offset, Some(&documents(object_schema(), &[])));
    let item = items.first().expect("properties offered");
    assert_eq!(text.get(item.replace.start..item.replace.end), Some("\"\""));
  }
}
