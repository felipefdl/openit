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
    let rope = text.clone();
    cx.background_spawn(async move {
      let source = rope.to_string();
      let items = schema::completions(family, &source, offset, documents.as_ref())
        .into_iter()
        .map(|item| to_item(&rope, item))
        .collect();
      Ok(CompletionResponse::Array(items))
    })
  }

  fn is_completion_trigger(&self, _offset: usize, _new_text: &str, _cx: &mut App) -> bool {
    true
  }
}

/// Attach this provider when the document is JSON-family.
pub(crate) fn install(state: &mut EditorState, cache: Entity<DocumentSchemaCache>, family: JsonFamily) {
  state.lsp_mut().completion_provider = Some(SchemaCompletionProvider::new(cache, family));
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
  use serde_json::json;

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
}
