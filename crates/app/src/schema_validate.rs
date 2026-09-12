//! Map a JSON-family buffer onto schema diagnostics. No GPUI.

use openit_core::schema::{self, CompiledSchema, JsonFamily, SchemaDiagnostic, SchemaDocuments, SyntaxError};

/// Parse `text` and, when `documents` are ready, validate. A compile failure is
/// not a diagnostic (download and `$ref` failures stay off the editor).
pub(crate) fn collect_issues(
  family: JsonFamily,
  text: &str,
  documents: Option<&SchemaDocuments>,
) -> Vec<SchemaDiagnostic> {
  match schema::parse(family, text) {
    Err(SyntaxError { range, message }) => vec![SchemaDiagnostic { range, message }],
    Ok(parsed) => {
      let Some(documents) = documents else {
        return Vec::new();
      };
      CompiledSchema::compile(documents).map_or_else(|_| Vec::new(), |compiled| compiled.validate(&parsed))
    },
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use openit_core::schema::{JsonFamily, SchemaDocuments};
  use serde_json::json;

  use super::collect_issues;

  fn documents(root: serde_json::Value) -> SchemaDocuments {
    let mut documents = BTreeMap::new();
    documents.insert("https://openit.test/schema.json".to_owned(), root);
    SchemaDocuments {
      root: "https://openit.test/schema.json".to_owned(),
      documents,
    }
  }

  #[test]
  fn a_syntax_error_is_an_issue_without_a_schema() {
    let issues = collect_issues(JsonFamily::Json, "{ // comment }", None);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].range.start < issues[0].range.end);
  }

  #[test]
  fn an_unknown_property_covers_that_key() {
    let text = "{\n  \"extra\": true\n}\n";
    let schema = json!({
      "type": "object",
      "properties": {},
      "additionalProperties": false
    });
    let issues = collect_issues(JsonFamily::Json, text, Some(&documents(schema)));
    let extra = text.find("\"extra\"").unwrap();
    assert!(
      issues.iter().any(|issue| issue.range.start <= extra && extra < issue.range.end),
      "{issues:?}"
    );
  }

  #[test]
  fn a_type_mismatch_covers_the_value() {
    let text = "{\n  \"n\": \"x\"\n}\n";
    let schema = json!({
      "type": "object",
      "properties": { "n": { "type": "number" } }
    });
    let issues = collect_issues(JsonFamily::Json, text, Some(&documents(schema)));
    let value = text.find("\"x\"").unwrap();
    assert!(
      issues.iter().any(|issue| issue.range.start <= value && value < issue.range.end),
      "{issues:?}"
    );
  }

  #[test]
  fn a_missing_schema_document_is_not_an_issue() {
    let text = "{\n  \"extra\": true\n}\n";
    let mut documents = BTreeMap::new();
    documents.insert(
      "https://openit.test/schema.json".to_owned(),
      json!({ "$ref": "https://openit.test/missing.json" }),
    );
    let documents = SchemaDocuments {
      root: "https://openit.test/schema.json".to_owned(),
      documents,
    };
    assert!(collect_issues(JsonFamily::Json, text, Some(&documents)).is_empty());
  }
}
