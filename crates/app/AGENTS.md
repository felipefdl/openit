# crates/app

GPUI desktop application. One document per window: `DocumentView` for text and Markdown, `ImageView` for images and SVG, `PdfView` for PDFs, and `EmptyView` when nothing is open yet.

## Conventions

- Views own presentation state only. Document lifecycle, persistence, and disk policy live in `session.rs`; the view delegates and renders.
- The editor buffer belongs to gpui-kit's `EditorState`; never keep a second copy of the text in a view.
- Long work goes through `cx.background_spawn` or `cx.spawn` and carries the `Revision` it was started from; drop results whose revision is stale.

## Commands

```bash
just run path/to/file.md
just task openit
```

## Rules (from actual mistakes in git history)

1. `EditorState::set_value` does not emit `InputEvent::Change`; drive tests through `simulate_input`/`simulate_keystrokes`, never `set_value`, when the test needs the revision to move.
2. Every `cx.observe`/`cx.subscribe` handle is stored in a struct field; a dropped `Subscription` is a silent no-op.
3. `EditorState::set_value` emits no `Change` (never use it where dirtiness matters); `replace_all` emits one, so `replace_buffer` suppresses that single event and bumps the revision itself.
4. Timers in views are `Option<Task<()>>` fields; dropping the field is the cancel.
5. Work that must outlive the window (draft removal on close) runs in a detached task; a view-owned task dies with the window's entity.
6. Background results (reload, save, checkpoint) apply only if the captured revision and disk fingerprint still match; otherwise they flag a conflict.
7. Document resources (images, later schemas) load only through `DocumentImageCache`/the fetcher seam; never through GPUI's default asset loader or `svg().external_path`.
8. No literal colors in production code outside theme.rs; paint with cx.theme() tokens or ActivePalette fields.
9. The theme is application-global: the appearance source is `cx.window_appearance()`, and one `AppSettings` observer at app level reloads and applies it. A view never rebuilds the theme catalog.
10. One overlay at a time: dialogs over a document (theme picker, language picker, go-to-line) go through `DocumentView`'s `Overlay` slot with their subscription in `overlay_subscription`; closing one refocuses the editor or the view.
11. `just task openit` does not run rustfmt; run `cargo fmt --all -- --check` before committing.
12. Image documents open in `ImageView`; text and Markdown in `DocumentView`. Every pixel operation (decode, transform, resize, encode) lives in `openit_core::raster`, and SVG rasterization in `svg.rs`; a view never decodes inline.
13. GPUI paints BGRA: any buffer handed to `RenderImage` goes through `image_decode::fit`, which caps the edge and swaps the channels. Skipping it renders the image with red and blue exchanged.
14. A window is decoded before it opens (`window.rs` passes the decoded image in), because `cx.notify()` from a background task does not schedule a frame for a root view. Async work that must appear on screen refreshes the window explicitly.
15. A file no reader accepts leaves through `handoff.rs`, never `cx.open_with_system` at the call site: GPUI's test platform panics on `open_with_system`, so tests replace the `SystemOpener` in the `Handoff` global.
16. PDF pixels come from `openit_core::pdf::render_page` on the background executor and go through `image_decode::fit` like every other bitmap; a render result is applied only when its generation still matches the view's.
17. A converted document's figures are pinned in its `DocumentImageCache` by the reference string the Markdown writes; Save As writes them beside the file and swaps the cache to that base directory. Nothing is written before the user saves.
18. Title bar icons go through `title_bar::toolbar_button`, which carries the tooltip on a wrapper element. `Button::tooltip` compiles but never shows: it looks its overlay up through gpui-component's `Root`, and OpenIt windows do not use `Root`.
