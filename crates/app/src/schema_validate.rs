//! Map a JSON-family buffer onto schema diagnostics. No GPUI.

use openit_core::schema::{CompiledSchema, ParsedJson, SchemaDiagnostic, SyntaxError};

/// Validate an already-parsed document. A compile failure is not a diagnostic
/// (download and `$ref` failures stay off the editor).
pub(crate) fn collect_issues(
  parsed: Result<&ParsedJson, &SyntaxError>,
  compiled: Option<&CompiledSchema>,
) -> Vec<SchemaDiagnostic> {
  match parsed {
    Err(SyntaxError { range, message }) => vec![SchemaDiagnostic { range: *range, message: message.clone() }],
    Ok(parsed) => compiled.map_or_else(Vec::new, |compiled| compiled.validate(parsed)),
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use openit_core::schema::{CompiledSchema, JsonFamily, SchemaDocuments, parse};
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

  fn compiled(root: serde_json::Value) -> CompiledSchema {
    CompiledSchema::compile(&documents(root)).expect("compile")
  }

  #[test]
  fn a_syntax_error_is_an_issue_without_a_schema() {
    let parsed = parse(JsonFamily::Json, "{ // comment }");
    let issues = collect_issues(parsed.as_ref(), None);
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
    let parsed = parse(JsonFamily::Json, text).expect("parse");
    let compiled = compiled(schema);
    let extra = text.find("\"extra\"").unwrap();
    let issues = collect_issues(Ok(&parsed), Some(&compiled));
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
    let parsed = parse(JsonFamily::Json, text).expect("parse");
    let compiled = compiled(schema);
    let value = text.find("\"x\"").unwrap();
    let issues = collect_issues(Ok(&parsed), Some(&compiled));
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
    let parsed = parse(JsonFamily::Json, text).expect("parse");
    let compiled = CompiledSchema::get_or_compile(None, &documents).ok();
    assert!(collect_issues(Ok(&parsed), compiled.as_ref()).is_empty());
  }

  #[test]
  fn get_or_compile_reuses_the_validator_when_the_identity_is_unchanged() {
    let schema = json!({
      "type": "object",
      "properties": { "n": { "type": "number" } }
    });
    let documents = documents(schema);
    let first = CompiledSchema::compile(&documents).expect("compile");
    let identity = first.identity().clone();
    let reused = CompiledSchema::get_or_compile(Some(first), &documents).expect("reuse");
    assert_eq!(reused.identity(), &identity);
    let parsed = parse(JsonFamily::Json, "{\n  \"n\": 1\n}\n").expect("parse");
    assert!(collect_issues(Ok(&parsed), Some(&reused)).is_empty());
  }
}
