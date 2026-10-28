# Schema validation

## Purpose

JSON, JSONC, and JSON5 documents complete from a JSON Schema: properties at the cursor and `enum` values. Unknown keys and type mismatches get a red squiggle. Saving is never blocked. Extends [Configuration validation](application-architecture.md#configuration-validation).

## Scope

- In: parse of `.json`, `.jsonc`, and `.json5`; SchemaStore catalog index; schema selection; `$ref` through the existing resource resolver; `DiagnosticSet` errors for syntax, unknown properties, and type mismatches; schema-driven completions; quiet status name; manual pick persisted on the draft and in settings.
- Out: YAML, TOML, XML, INI, and every other text kind; a language-server process; formatting or rewriting; Quick Look; status-bar error counts; a dialog or danger status on schema download failure.
- Later: other schema formats; live SchemaStore refresh; completion snippets.

## Stack

- `jsonc-parser` 0.34.0, MIT. Features `serde` and `serde_json`. `parse_to_ast` for source ranges.
- `jsonschema` 0.60.2, MIT. `default-features = false` (defaults pull reqwest and aws-lc-rs). Build with `.offline()` / an in-memory registry after OpenIt has the bytes.
- Existing `Fetcher`, `ResourceCache`, and `PermissionRequests` for remote schema documents. Not jsonschema's HTTP client.
- gpui-base 0.8.1 `EditorState::diagnostics_mut`, `DiagnosticSet::push`, `Lsp.completion_provider` (`CompletionProvider`), and the `ShowCompletions` action bound to Ctrl+Space in the editor. No language-server process.
- SchemaStore catalog index (`fileMatch` + `url`), bundled. Schema documents are not bundled; jsonschema already embeds draft meta-schemas.

## Decisions

- Completions are the reason the schema is loaded. Squiggles are secondary. Save never cares.
- `.json` is strict JSON (every `ParseOptions` flag false). `.jsonc` and `.json5` allow comments and trailing commas only.
- Selection order: manual pick (local file or URL), then the document's `$schema`, then an unambiguous catalog `fileMatch`. Ambiguous matches open the picker.
- Manual pick is stored on the draft and in settings keyed by canonical path. Untitled documents have no settings key; `$schema` and a manual URL still work.
- Catalog index only in the binary. Schema bodies come from cache, then the resolver. Mixed SchemaStore document licenses and size.
- `$ref` depth cap is 8. Each hop can be a fetch.
- Validation runs once on open, then 1 s after the last edit. Stale revisions are dropped.
- Status shows the schema name, or "No schema", muted like the language segment. Click opens the picker. Never a count, never danger or warning color.
- Fetch failure after allow: log only. Not a diagnostic, not a status color, not a bar. Unknown domain still uses the permission bar.
- Completions: properties at the cursor (including nested objects) and `enum` values from the loaded schema. `$ref` already in the registry is followed. No snippets.
- Diagnostics: `DiagnosticSet` Error for JSON syntax, unknown properties, and type mismatches. Download failure is not a diagnostic. Edits clear the set; the matching revision puts it back.
- JSON family only. Those files already open in the editor.

## Executor latitude

Decide alone: module and file names inside the stated crates (`schema`, `catalog`, `validate` are suggestions). Diagnostic message text from `jsonschema`. Completion item `label`/`detail`. Log line text. Catalog file path under `assets/`. Test placement. Whether validation shares `CHECKPOINT_DELAY` or a sibling 1 s timer.

Stop and report: `jsonschema` or `jsonc-parser` failing `cargo deny` or not building on a `deny.toml` target. `DiagnosticSet` or `CompletionProvider` missing or unusable on `EditorState` without a language-server process. Fetch-after-allow forcing a dialog or a danger status. Anything that conflicts with `AGENTS.md`. A gpui or gpui-kit limitation that blocks an item: row in `docs/blocked-features.md`, item stays open.

`just task` on the touched crate after each item. `just check` on the final tree only, never weakened.

## Items

### 1. Parse the JSON family with source ranges

- New GPUI-free module in `crates/core`.
- `.json`: every `jsonc_parser::ParseOptions` flag false.
- `.jsonc` and `.json5`: comments and trailing commas only.
- A successful parse returns a `serde_json::Value` and JSON-pointer to source-range maps: `ParsedJson.ranges` for instances (object members span name through value), and `ParsedJson.key_ranges` for member keys only. A syntax error returns a range covering the bad token.
- Does not rewrite the document.
- Done when:
  - `just task openit-core`: green
  - strict `.json` with a comment: syntax error
  - `.jsonc` and `.json5` with a comment and a trailing comma: ok
  - syntax error range covers the bad token
  - `/name` range covers that property

### 2. Bundled catalog index and filename match

- Bundle SchemaStore's catalog index (`fileMatch` + `url`), not schema documents. No network in this item.
- Match on the open document's file name. Only JSON-family documents consult the catalog.
- Result: one URL, several URLs (ambiguous), or none.
- Done when:
  - `just task openit-core`: green
  - `package.json`: one match
  - a name that hits two `fileMatch` entries: ambiguous
  - an unknown name: none

### 3. Validate against an in-memory schema registry

- After: 1
- `jsonschema` 0.56.0, `default-features = false`, `.offline()` / in-memory registry. No HTTP from this crate.
- Local schema documents enter the registry under their `file:` URL, so a path with spaces or other characters a URI cannot hold still compiles, and relative `$ref`s resolve against it. A schema that fails to compile logs a warning and validates nothing.
- Map `instance_path` through item 1's pointer map onto source ranges. A path with no range still yields a diagnostic at the document root. `additionalProperties` and `unevaluatedProperties` failures produce one diagnostic per offending key, covering that key only, with `Property "name" is not allowed`; a missing key range falls back to the instance range. The root `$schema` key is exempt from these two failures; a nested `$schema` is not.
- A `$ref` present in the registry validates. A missing `$ref` does not validate. Depth 9 does not validate.
- Compiled validators are reused until the schema identity (root URL or path plus digest of loaded documents) changes.
- Done when:
  - `just task openit-core`: green
  - a `minimum` failure's range covers the invalid value
  - `$ref` in the registry: validates
  - missing `$ref`: not validated
  - depth 9: not validated

### 4. Selection precedence

- After: 1, 2
- Order: manual pick (local file or URL), then `$schema` on the parsed value, then an unambiguous catalog match.
- Two catalog hits: ask for a choice. No match and no `$schema`: no schema.
- Untitled + relative local schema: denied (`NoLocalBase`). `$schema` that is an http(s) URL still selects.
- Done when:
  - `just task openit-core`: green
  - manual wins over `$schema`
  - `$schema` wins over an unambiguous catalog match
  - no `$schema` and one catalog hit: that URL
  - two catalog hits: ask
  - untitled + relative schema: denied

### 5. Load schema documents through the existing resource path

- After: 4
- Local files and remote URLs go through `resolve`, `Fetcher`, `ResourceCache`, and `PermissionRequests`, the same path Markdown images use.
- `$ref` uses that path with depth 8. Redirects still cannot leave the permitted family.
- GIVEN an unknown domain family, WHEN a schema URL is needed, THEN the permission bar asks and nothing is fetched until Allow this family or always-allow.
- GIVEN an allowed family and the server fails, WHEN validation runs, THEN one log line, no diagnostic, no status color, no bar.
- Untitled relative schema never fetches.
- Done when:
  - `just task openit`: green
  - allowed family: one fetch, second load is cache
  - unknown family: permission bar, zero fetches; Allow then retries
  - failed fetch after allow: log only, no diagnostic, no status color, no bar
  - local schema relative to the document loads
  - untitled relative schema never fetches

### 6. Background validation, DiagnosticSet, quiet status name

- After: 3, 5
- JSON family only. Runs once on open, then 1 s after the last edit. Drop results whose revision does not match.
- Push `DiagnosticSet` Error for JSON syntax, unknown properties, and type mismatches. Schema download failure is not a diagnostic.
- Status: schema name, or "No schema", muted, left of the language segment. Never a count, never danger or warning color.
- Save still writes.
- Done when:
  - `just task openit`: green
  - open JSON with `$schema` and an extra key or a type mismatch: after park, `DiagnosticSet` has an Error on that range
  - status shows the schema name (or "No schema"), muted, never a count, never danger
  - an edit clears the set; 1 s later the matching revision puts it back
  - a stale result is ignored
  - save still writes

### 7. Schema CompletionProvider

- After: 5
- Install `CompletionProvider` on the document's `EditorState`. No language-server process.
- Offers properties at the cursor, including nested object properties, and `enum` values, from the loaded schema. Follow `$ref` already in the registry. Property suggestions exclude sibling keys already present before or after the caret, but keep the key under or touching the caret; keys in nested objects do not exclude outer properties.
- No schema loaded: this provider returns no items.
- The menu opens while typing `"`, `:`, `_`, `$`, or an ASCII letter or digit, and on demand with Ctrl+Space (`ShowCompletions`). `{`, `,`, and `[` do not trigger it.
- No snippets. Property completion replaces the token with `"name": `, including a trailing space; if a colon already follows the replaced token, it inserts only `"name"`.
- Done when:
  - `just task openit`: green
  - with a loaded schema: the completion menu offers properties at the cursor and `enum` values
  - nested object properties appear inside that object
  - no schema loaded: this provider contributes nothing

### 8. Schema picker, draft and settings persistence

- After: 4, 6
- Status name click opens the schema picker in `DocumentView`'s existing `Overlay` slot (same as the language picker).
- The picker labels catalog entries by their first file pattern, stripping a leading `**/` and falling back to the URL file name when there is no pattern, plus a way to choose a local file or URL. Search ranks exact label, label prefix, label substring, other file patterns, then URL; a matching label span is bold. The current schema is preselected, scrolled into view, and marked "(current)". Automatic clears the manual pick. Escape closes even with a query typed.
- An ambiguous catalog match opens the picker.
- Persist the manual pick on the draft and in settings keyed by canonical path. Next open of that path uses it. A recovered draft restores it.
- Done when:
  - `just task openit`: green
  - status name click opens the picker
  - a pick writes settings by canonical path and is used on the next open
  - a recovered draft restores the pick
  - Automatic clears it
  - an ambiguous catalog match opens the picker

### 9. Point architecture at this spec

- After: 8
- `docs/specs/application-architecture.md` Configuration validation points here. Feedback, scheduling, and download-failure lines match this spec (quiet status, completions, no danger on fetch failure).
- Done when:
  - Architecture Configuration validation points here
  - History row exists
  - `just check`: green

---

## History

| Date | Item | Event | Description |
|---|---|---|---|
| 2026-09-11 | - | Created | Completions plus quiet schema errors. Fetch fail logs only. |
| 2026-09-11 | 1 | Dispatched | feat/schema-validation-1 |
| 2026-09-11 | 2 | Dispatched | feat/schema-validation-2 |
| 2026-09-11 | 1 | Shipped | feat/schema-validation-1 |
| 2026-09-11 | 2 | Shipped | feat/schema-validation-2 |
| 2026-09-11 | 3 | Dispatched | feat/schema-validation-3 |
| 2026-09-11 | 4 | Dispatched | feat/schema-validation-4 |
| 2026-09-11 | 3 | Shipped | feat/schema-validation-3 |
| 2026-09-11 | 4 | Shipped | feat/schema-validation-4 |
| 2026-09-11 | 5 | Dispatched | feat/schema-validation-5 |
| 2026-09-11 | 5 | Shipped | feat/schema-validation-5 |
| 2026-09-11 | 6 | Dispatched | feat/schema-validation-6 |
| 2026-09-11 | 7 | Dispatched | feat/schema-validation-7 |
| 2026-09-11 | 6 | Shipped | feat/schema-validation-6 |
| 2026-09-11 | 7 | Shipped | feat/schema-validation-7 |
| 2026-09-11 | 8 | Dispatched | feat/schema-validation-8 |
| 2026-09-11 | 8 | Shipped | feat/schema-validation-8 |
| 2026-09-11 | 9 | Dispatched | feat/schema-validation-9 |
| 2026-09-11 | 9 | Shipped | feat/schema-validation-9 |
