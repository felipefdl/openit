# crates/core

Document library shared by the desktop app and the Quick Look extension. No GPUI types, no window state.

## Conventions

- Public functions take paths and bytes/strings and return typed `Result<T, Error>`; they never touch UI.
- Anything that can be slow (disk, parsing) is a plain synchronous function. The app decides which executor runs it.
- Results that depend on a document revision carry that `Revision` in their return type.
- Every file written by core goes through `save::write_atomic`; no direct `fs::write` in non-test code.
- Every persistent input (document, draft, settings) is read through one open handle with a regular-file check and a size cap; never `fs::read`/`read_to_string` on a user path.
- Initial resolution and authorization of any reference happen in `resource.rs`; callers apply the result and may only re-check redirect targets and bounded local reads through the documented helpers.
