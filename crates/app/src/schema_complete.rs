//! Install a schema `CompletionProvider` on JSON-family editors. No language server.

use std::rc::Rc;

use gpui_kit::component::input::{CompletionProvider, EditorState, RopeExt};
use gpui_kit::{App, AppContext, Entity, Result, Task, Window};
use lsp_types::{
  CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, InsertTextFormat,
  TextEdit,
};
use openit_core::schema::{self, CompletionKind, JsonFamily, SchemaCompletion};
use ropey::Rope;

use crate::schema_cache::{DocumentSchemaCache, SchemaDocs};

/// Schema completions from the document's loaded registry.
pub(crate) struct SchemaCompletionProvider {
  cache: Entity<DocumentSchemaCache>,
  family: JsonFamily,
}

impl SchemaCompletionProvider {
  pub(crate) fn new(cache: Entity<DocumentSchemaCache>, family: JsonFamily) -> Rc<Self> {
    Rc::new(Self { cache, family })
  }
}

impl CompletionProvider for SchemaCompletionProvider {
  fn completions(
    &self,
    text: &Rope,
    offset: usize,
    _trigger: CompletionContext,
    _window: &mut Window,
    cx: &mut App,
  ) -> Task<Result<CompletionResponse>> {
    let documents = match self.cache.read(cx).documents() {
      SchemaDocs::Ready(documents) => Some(documents),
      SchemaDocs::Pending | SchemaDocs::None => None,
    };
    let family = self.family;
    let source = rope_prefix(text, offset);
    let rope = text.clone();
    cx.background_spawn(async move {
      let items = schema::completions(family, &source, offset, documents.as_ref())
        .into_iter()
        .map(|item| to_item(&rope, item))
        .collect();
      Ok(CompletionResponse::Array(items))
    })
  }

  fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
    is_json_completion_trigger(new_text)
  }
}

/// Attach this provider when the document is JSON-family.
pub(crate) fn install(state: &mut EditorState, cache: Entity<DocumentSchemaCache>, family: JsonFamily) {
  state.lsp_mut().completion_provider = Some(SchemaCompletionProvider::new(cache, family));
}

fn is_json_completion_trigger(new_text: &str) -> bool {
  new_text.chars().any(is_json_completion_char)
}

const fn is_json_completion_char(ch: char) -> bool {
  matches!(ch, '"' | '{' | ',' | ':' | '[' | '_' | '$') || ch.is_ascii_alphanumeric()
}

fn rope_prefix(rope: &Rope, offset: usize) -> String {
  let end = offset.min(rope.len());
  let mut source = String::with_capacity(end);
  if let Ok(slice) = rope.try_slice(..end) {
    source.extend(slice.chunks());
  } else {
    source.extend(rope.chunks());
  }
  source
}

fn to_item(rope: &Rope, item: SchemaCompletion) -> CompletionItem {
  let start = rope.offset_to_position(item.replace.start);
  let end = rope.offset_to_position(item.replace.end);
  let kind = match item.kind {
    CompletionKind::Property => CompletionItemKind::PROPERTY,
    CompletionKind::Enum => CompletionItemKind::ENUM_MEMBER,
  };
  CompletionItem {
    label: item.label,
    detail: item.detail,
    kind: Some(kind),
    insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
      range: lsp_types::Range { start, end },
      new_text: item.insert,
    })),
    ..CompletionItem::default()
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use openit_core::schema::{CompletionKind, JsonFamily, SchemaDocuments, completions};
  use ropey::Rope;
  use serde_json::json;

  use super::{is_json_completion_trigger, rope_prefix};

  fn documents(root: serde_json::Value) -> SchemaDocuments {
    let mut documents = BTreeMap::new();
    documents.insert("https://openit.test/schema.json".to_owned(), root);
    SchemaDocuments {
      root: "https://openit.test/schema.json".to_owned(),
      documents,
    }
  }

  #[test]
  fn provider_mapping_uses_plain_insert_text() {
    let schema = json!({
      "type": "object",
      "properties": { "name": { "type": "string" } }
    });
    let text = "{\n  \n}\n";
    let offset = text.find("{\n  ").unwrap() + "{\n  ".len();
    let items = completions(JsonFamily::Json, text, offset, Some(&documents(schema)));
    assert_eq!(items[0].kind, CompletionKind::Property);
    assert_eq!(items[0].insert, "\"name\"");
    assert!(!items[0].insert.contains('$'));
  }

  #[test]
  fn completion_trigger_is_gated_on_json_structure_and_identifier_chars() {
    assert!(is_json_completion_trigger("\""));
    assert!(is_json_completion_trigger("{"));
    assert!(is_json_completion_trigger(","));
    assert!(is_json_completion_trigger(":"));
    assert!(is_json_completion_trigger("["));
    assert!(is_json_completion_trigger("n"));
    assert!(is_json_completion_trigger("_"));
    assert!(!is_json_completion_trigger(""));
    assert!(!is_json_completion_trigger(" "));
    assert!(!is_json_completion_trigger("\n"));
    assert!(!is_json_completion_trigger("}"));
    assert!(!is_json_completion_trigger("]"));
  }

  #[test]
  fn rope_prefix_stops_at_the_cursor_and_keeps_that_text() {
    let rope = Rope::from_str("{\"name\": true, \"extra\": 1}");
    let offset = "{\"name\": true".len();
    assert_eq!(rope_prefix(&rope, offset), "{\"name\": true");
    assert_eq!(rope_prefix(&rope, 0), "");
  }
}
