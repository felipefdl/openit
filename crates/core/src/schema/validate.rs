//! In-memory JSON Schema validation. No HTTP.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jsonschema::Registry;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::{ParsedJson, SourceRange};

/// Maximum `$ref` document hops. The ninth hop does not compile.
pub const MAX_REF_DEPTH: u32 = 8;

/// Schema documents already loaded into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDocuments {
  /// Root schema URL or path. Must be a key in [`Self::documents`].
  pub root: String,
  /// URI or path to schema JSON. Includes the root document.
  pub documents: BTreeMap<String, Value>,
}

impl SchemaDocuments {
  /// Identity of this set: root plus a digest of every loaded document.
  pub fn identity(&self) -> SchemaIdentity {
    SchemaIdentity {
      root: self.root.clone(),
      digest: digest(&self.root, &self.documents),
    }
  }
}

/// Root URL or path plus a digest of the loaded documents.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaIdentity {
  /// Root schema URL or path.
  pub root: String,
  /// SHA-256 of the root key and every loaded document.
  pub digest: [u8; 32],
}

/// Why a schema could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
  /// A `$ref` is not among the loaded documents.
  #[error("schema $ref {uri} is not in the registry")]
  MissingRef {
    /// The unresolved reference.
    uri: String,
  },
  /// `$ref` hops exceeded [`MAX_REF_DEPTH`].
  #[error("schema $ref depth exceeds {MAX_REF_DEPTH}")]
  RefTooDeep,
  /// jsonschema rejected the schema.
  #[error("{message}")]
  Invalid {
    /// Validator construction message.
    message: String,
  },
}

/// A compiled schema, reused until [`SchemaIdentity`] changes.
#[derive(Debug)]
pub struct CompiledSchema {
  identity: SchemaIdentity,
  validator: jsonschema::Validator,
}

impl CompiledSchema {
  /// Compile `documents` unless `existing` still matches their identity.
  pub fn get_or_compile(existing: Option<Self>, documents: &SchemaDocuments) -> Result<Self, CompileError> {
    let identity = documents.identity();
    if let Some(existing) = existing
      && existing.identity == identity
    {
      return Ok(existing);
    }
    Self::from_documents(identity, documents)
  }

  /// Compile against an in-memory registry. External retrieval is disabled.
  pub fn compile(documents: &SchemaDocuments) -> Result<Self, CompileError> {
    Self::from_documents(documents.identity(), documents)
  }

  fn from_documents(identity: SchemaIdentity, documents: &SchemaDocuments) -> Result<Self, CompileError> {
    check_refs(documents)?;
    let root_schema = documents.documents.get(&documents.root).ok_or_else(|| CompileError::Invalid {
      message: format!("root schema {} is not in the registry", documents.root),
    })?;
    let registry = build_registry(documents)?;
    let validator = jsonschema::options()
      .with_registry(&registry)
      .offline()
      .with_base_uri(&documents.root)
      .build(root_schema)
      .map_err(|error| CompileError::Invalid { message: error.to_string() })?;
    Ok(Self { identity, validator })
  }

  /// Identity this validator was compiled for.
  pub const fn identity(&self) -> &SchemaIdentity {
    &self.identity
  }

  /// Validate `parsed` and map `instance_path` onto source ranges.
  ///
  /// A path with no range uses the document root.
  pub fn validate(&self, parsed: &ParsedJson) -> Vec<SchemaDiagnostic> {
    let root = parsed.range("").unwrap_or(SourceRange { start: 0, end: 0 });
    self
      .validator
      .iter_errors(&parsed.value)
      .map(|error| {
        let pointer = error.instance_path().as_str();
        SchemaDiagnostic {
          range: parsed.range(pointer).unwrap_or(root),
          message: error.to_string(),
        }
      })
      .collect()
  }
}

/// One schema failure mapped onto the instance source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDiagnostic {
  /// Source span covering the invalid value, or the document root.
  pub range: SourceRange,
  /// Message from jsonschema.
  pub message: String,
}

fn digest(root: &str, documents: &BTreeMap<String, Value>) -> [u8; 32] {
  let mut hasher = Sha256::new();
  hasher.update(root.as_bytes());
  hasher.update([0]);
  for (uri, value) in documents {
    hasher.update(uri.as_bytes());
    hasher.update([0]);
    match serde_json::to_vec(value) {
      Ok(bytes) => hasher.update(bytes),
      Err(_) => hasher.update(value.to_string().as_bytes()),
    }
    hasher.update([0]);
  }
  hasher.finalize().into()
}

fn build_registry(documents: &SchemaDocuments) -> Result<jsonschema::Registry<'static>, CompileError> {
  let mut builder = Registry::new();
  for (uri, value) in &documents.documents {
    builder = builder
      .add(uri.as_str(), value.clone())
      .map_err(|error| CompileError::Invalid { message: error.to_string() })?;
  }
  builder
    .prepare()
    .map_err(|error| CompileError::Invalid { message: error.to_string() })
}

fn check_refs(documents: &SchemaDocuments) -> Result<(), CompileError> {
  if !documents.documents.contains_key(&documents.root) {
    return Err(CompileError::Invalid {
      message: format!("root schema {} is not in the registry", documents.root),
    });
  }
  let mut scan = RefScan {
    documents,
    visited: BTreeSet::from([documents.root.clone()]),
    queue: VecDeque::from([(documents.root.clone(), 0)]),
  };
  while let Some((key, hop)) = scan.queue.pop_front() {
    let Some(schema) = documents.documents.get(&key) else {
      return Err(CompileError::MissingRef { uri: key });
    };
    scan.collect(schema, &key, hop)?;
  }
  Ok(())
}

struct RefScan<'a> {
  documents: &'a SchemaDocuments,
  visited: BTreeSet<String>,
  queue: VecDeque<(String, u32)>,
}

impl RefScan<'_> {
  fn collect(&mut self, value: &Value, base: &str, hop: u32) -> Result<(), CompileError> {
    match value {
      Value::Object(map) => {
        if let Some(Value::String(reference)) = map.get("$ref") {
          self.enqueue(base, reference, hop)?;
        }
        for child in map.values() {
          self.collect(child, base, hop)?;
        }
      },
      Value::Array(items) => {
        for child in items {
          self.collect(child, base, hop)?;
        }
      },
      _ => {},
    }
    Ok(())
  }

  fn enqueue(&mut self, base: &str, reference: &str, hop: u32) -> Result<(), CompileError> {
    let resolved = resolve_ref(base, reference)?;
    let target = document_uri(&resolved);
    if target == document_uri(base) {
      return Ok(());
    }
    let next = hop.saturating_add(1);
    if next > MAX_REF_DEPTH {
      return Err(CompileError::RefTooDeep);
    }
    let Some(key) = self.documents.documents.keys().find(|key| document_uri(key) == target).cloned() else {
      return Err(CompileError::MissingRef { uri: resolved });
    };
    if self.visited.insert(key.clone()) {
      self.queue.push_back((key, next));
    }
    Ok(())
  }
}

pub(super) fn resolve_ref(base: &str, reference: &str) -> Result<String, CompileError> {
  if let Ok(url) = Url::parse(reference) {
    return Ok(url.to_string());
  }
  Url::parse(base).map_or_else(
    |_| Ok(reference.to_owned()),
    |base_url| {
      base_url
        .join(reference)
        .map(|url| url.to_string())
        .map_err(|error| CompileError::Invalid { message: error.to_string() })
    },
  )
}

pub(super) fn document_uri(uri: &str) -> String {
  Url::parse(uri).map_or_else(
    |_| uri.split_once('#').map_or(uri, |(head, _)| head).to_owned(),
    |mut url| {
      url.set_fragment(None);
      url.to_string()
    },
  )
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use serde_json::{Value, json};

  use super::{CompileError, CompiledSchema, SchemaDocuments, SourceRange};
  use crate::schema::{JsonFamily, ParsedJson, parse};

  const ROOT: &str = "https://openit.test/root.json";

  fn documents(root: Value, extras: &[(&str, Value)]) -> SchemaDocuments {
    let mut documents = BTreeMap::new();
    documents.insert(ROOT.to_owned(), root);
    for (uri, value) in extras {
      documents.insert((*uri).to_owned(), value.clone());
    }
    SchemaDocuments { root: ROOT.to_owned(), documents }
  }

  fn compile(root: Value, extras: &[(&str, Value)]) -> CompiledSchema {
    CompiledSchema::compile(&documents(root, extras)).expect("compile")
  }

  fn chain(depth: u32) -> SchemaDocuments {
    let mut docs = BTreeMap::new();
    docs.insert(ROOT.to_owned(), json!({ "$ref": "https://openit.test/1.json" }));
    for index in 1..=depth {
      let uri = format!("https://openit.test/{index}.json");
      let next = if index == depth {
        json!({ "type": "number" })
      } else {
        json!({ "$ref": format!("https://openit.test/{}.json", index + 1) })
      };
      docs.insert(uri, next);
    }
    SchemaDocuments { root: ROOT.to_owned(), documents: docs }
  }

  #[test]
  fn minimum_failure_range_covers_the_invalid_value() {
    let text = "{ \"n\": 1 }";
    let parsed = parse(JsonFamily::Json, text).expect("json");
    let compiled = compile(json!({ "type": "object", "properties": { "n": { "minimum": 5 } } }), &[]);
    let issues = compiled.validate(&parsed);
    let issue = issues.iter().find(|issue| issue.message.contains("minimum")).expect("minimum");
    let covered = text.get(issue.range.start..issue.range.end).expect("range");
    assert!(covered.contains('1'), "{covered:?} does not cover the invalid value");
  }

  #[test]
  fn ref_in_the_registry_validates() {
    let parsed = parse(JsonFamily::Json, "{ \"n\": 8 }").expect("json");
    let compiled = compile(
      json!({ "properties": { "n": { "$ref": "https://openit.test/number.json" } } }),
      &[("https://openit.test/number.json", json!({ "type": "number", "minimum": 5 }))],
    );
    assert!(compiled.validate(&parsed).is_empty());
  }

  #[test]
  fn missing_ref_does_not_validate() {
    let error = CompiledSchema::compile(&documents(json!({ "$ref": "https://openit.test/missing.json" }), &[]))
      .expect_err("missing $ref");
    assert!(matches!(error, CompileError::MissingRef { .. }));
  }

  #[test]
  fn depth_nine_does_not_validate() {
    let error = CompiledSchema::compile(&chain(9)).expect_err("depth 9");
    assert!(matches!(error, CompileError::RefTooDeep));
  }

  #[test]
  fn depth_eight_compiles() {
    CompiledSchema::compile(&chain(8)).expect("depth 8");
  }

  #[test]
  fn missing_pointer_range_uses_the_document_root() {
    let parsed = ParsedJson {
      value: json!({ "n": 1 }),
      ranges: BTreeMap::from([(String::new(), SourceRange { start: 0, end: 9 })]),
    };
    let compiled = compile(json!({ "type": "object", "properties": { "n": { "minimum": 5 } } }), &[]);
    let issues = compiled.validate(&parsed);
    let issue = issues.first().expect("issue");
    assert_eq!(issue.range, SourceRange { start: 0, end: 9 });
  }

  #[test]
  fn compiled_identity_changes_with_loaded_documents() {
    let first = documents(json!({ "type": "number" }), &[]);
    let compiled = CompiledSchema::compile(&first).expect("compile");
    let reused = CompiledSchema::get_or_compile(Some(compiled), &first).expect("reuse");
    assert_eq!(reused.identity(), &first.identity());
    let mut second = first.clone();
    second
      .documents
      .insert("https://openit.test/extra.json".to_owned(), json!({ "type": "string" }));
    let compiled = CompiledSchema::get_or_compile(None, &second).expect("recompile");
    assert_ne!(compiled.identity(), &first.identity());
  }
}
