# OpenIt application architecture

Status: Approved
Date: 2026-09-08

This spec defines how OpenIt is built: its modules, the seams between them, the document lifecycle, the reader and editor surfaces, resource permissions, and the verification bar. `VISION.md` owns what the product is; this document owns how the parts fit. It is not an implementation plan.

## Scope

In scope: the desktop application on macOS, Windows, and Linux, the shared document library, the `openit` command, and the macOS Quick Look extension's contract with the library.

Out of scope: exact UI styling, benchmark thresholds (measured, not invented; see Performance), and the internals of the gpui-base changes this design requires (each gets its own upstream change with its own review).

## Approach

OpenIt is a fresh application organized around documents. It uses gpui-kit 0.6.1 presentation primitives (`EditorState`/`Editor` for text, `TextViewState`/`TextView` for Markdown) and focused helpers where they fit (bounded image decoding, local-asset grant logic). It does not import a repository model, mandatory autosave, another application's window shell, or another product's branding, and it does not introduce a library shared with another application.

Alternatives considered and rejected:

- Starting from a repository-session application and stripping it into a viewer: reuses more code up front, but every file-opening, persistence, and window path assumes a repository session and a single selected file, which contradicts one document per window and explicit saving.
- A shared viewer library with another application: centralizes fixes, but creates a coordination surface before OpenIt's needs are proven.

## Modules

| Module | Responsibility | Depends on |
| --- | --- | --- |
| Document library (Rust crate, no GPUI) | Loading and saving, draft recovery, format detection, schema validation, resource policy and resolver, settings model, PDF page rendering, text layer, and Markdown conversion | `ropey`, `jsonschema`, `jsonc-parser`, `psl`, `image`, `hayro`, `pdf-inspector` |
| Desktop application (GPUI) | Windows, document sessions, editor and reader surfaces, menus, shortcuts, clipboard, drag and drop, open-request routing, permission bar, update checks | Document library, gpui-kit |
| `openit` command | The app binary is the command: it turns arguments into open requests and hands them to the running instance or starts one | Desktop application |
| Quick Look extension (macOS) | Read-only static presentation of supported types inside Finder | Document library only |

The document library has no GPUI dependency and no window state. It is the test surface for most contracts.

### State ownership

- Each window owns exactly one document session: source identity, unsaved content, current mode, viewing and editing position, selected content type, and selected schema.
- Application-wide services own settings, permissions, recovery storage, background work, and open-request routing. There is no workspace model.
- While editing, gpui-kit's `EditorState` owns the live text buffer. Saving, recovery, validation, and preview consume revision-tagged snapshots; nothing maintains a second editable buffer.
- File operations and expensive processing run off the UI thread. Every result carries the document revision it was computed from; a result for an older revision is dropped, never applied.

## Toolchain and quality bar

OpenIt adopts the Rust quality bar from the That's Home project unchanged, with the MSRV raised to 1.98.1:

- Edition 2024, `rust-version = "1.98.1"`, virtual workspace root with resolver `"3"`, crates under `crates/`.
- Root `Cargo.toml` `[workspace.lints]`: `unsafe_code = "forbid"`, `missing_docs = "deny"`, clippy `all`, `pedantic`, `nursery`, and `cargo` at deny, the documented allow-list for known noise, and individually denied panic and silent-fail lints (`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `indexing_slicing`, `string_slice`, `as_conversions`, `print_stdout`, `print_stderr`, `dbg_macro`, `await_holding_lock`, `await_holding_refcell_ref`, `allow_attributes_without_reason`). Every member crate sets `[lints] workspace = true`.
- `rustfmt.toml` (120 columns, 2 spaces), `clippy.toml` (thresholds, test allows, `msrv = "1.98.1"`), `deny.toml` (advisories, license allow-list, bans, crates.io only), `audit.toml`, `.cargo/config.toml` aliases, `.config/nextest.toml`.
- Dev profile with `line-tables-only` debug info and optimized dependencies; release profile with LTO, one codegen unit, strip, and overflow checks; `build-override` keeps build scripts unstripped.
- One gate, `just check`: format check, lint inheritance check, `cargo deny check`, `cargo audit`, `cargo machete`, clippy with `-D warnings` on all targets and features, and `cargo nextest run --workspace`. There is no `cargo test` fallback.
- `tracing` only for logging; typed errors with `?`; no `unwrap`, `expect`, or `panic!` outside tests; `#[expect(..., reason = "...")]` over bare `allow`.

GPUI contains unsafe code inside its own crates; `unsafe_code = "forbid"` applies to OpenIt's crates only. Both PDF crates forbid unsafe code themselves.

The human-readable contract lives in `docs/rust-quality.md`, copied from That's Home and edited for this workspace's gate list.

## Document lifecycle

### Identity

Each document has a stable session identity independent of its path. Clipboard text and images are untitled until saved. Copied file references open the original files.

### Snapshots and revisions

The session increments a revision on every `InputEvent::Change`. A snapshot is the O(1) shared `Rope` clone from `EditorState::text()` paired with that revision. The library receives snapshots at operation boundaries (save, checkpoint, validate, preview refresh) and materializes bytes only there.

### Saving

- Cmd/Ctrl+S saves to the current path or opens Save As for an untitled document.
- A save captures one revision. Edits made during the write stay unsaved and mark the document dirty again.
- Writes go to a temporary file in the target directory followed by an atomic rename. The original file is never truncated in place.
- Writes to the same path are serialized across windows.
- Autosave is an opt-in setting using the same save path, only for documents with an established path. It never picks a destination for untitled content.
- Schema errors never block saving.

### Recovery

- Background checkpoints preserve unsaved content, source path, selected content type, selected schema, and position, keyed by session identity, in the platform data directory.
- Relaunch restores every document a crash or forced termination left unfinished, each in its own window.
- Recovery never writes to the original file.
- A failed checkpoint flush stops normal quit and reports the failure instead of silently losing work.

### Close and quit

- Quit asks each dirty document in turn: Save, Discard, or Cancel. Cancel stops the quit and keeps every window. Discard deletes that document's recovery data, so nothing returns on relaunch. A pending save-then-close whose save fails keeps its window and stops the quit.
- Explicitly closing a dirty document offers the same Save, Discard, and Cancel.
- Closing the last window quits on Linux and Windows through the same quit path as Cmd/Ctrl+Q, unless an open is still in flight. On macOS the app stays resident with no windows so Dock reopen can show an empty one.

### External changes

- A clean document reloads when its file changes on disk.
- A dirty document keeps its buffer and shows a conflict notice. Saving over a detected external change requires explicit confirmation.

### Failures

A failed save or checkpoint keeps the buffer and shows an actionable error in the window it belongs to.

## Text and Markdown

### Modes

- Markdown opens in the last explicitly selected Preview/Edit mode, persisted app-wide; Preview is the initial default. Preview does not create an editor until needed. Once created, mode switches keep the same `EditorState`, so buffer, undo history, caret, and selection survive. Recovered Markdown drafts use the same opening preference and retain their saved text and cursor.
- Other supported text formats open directly in the editor.
- A title bar button and Cmd/Ctrl+Shift+E toggle modes and save the choice for subsequent Markdown windows and launches. Existing windows retain their own mode. The button shows the mode a click moves to: a pencil while previewing, an eye while editing.
- Preview refreshes only when the revision changes, not on hidden-preview keystrokes.
- Preview layout: `View > Markdown Preview Width` offers Readable (700 px, default), Wide (960 px), and Full Width. Fixed-width columns are centered and shrink to fit smaller windows; all presets retain at least 24 px side padding. Width changes apply to open previews and persist app-wide without changing editor wrapping. Body text is 16 px with 1.6 line height. Links, inline code, code blocks, and tables take their colors from the active theme palette. Tables use gpui-base's measured layout (word wrapping; a table wider than the column scrolls sideways). The `TextView` owns the scroll so it stays virtualized (a 24,000-line file opens in about 0.6 s and scrolls smoothly; the non-virtualized layout could not open it at all). Scrollbar placement and heading-color limitations are tracked in [Blocked features](../blocked-features.md).
- Text selection in the preview comes from gpui-base's window-level `TextSelectionLayer`, which the document view mounts itself (OpenIt windows do not use gpui-component's `Root`).

### Reading position

Switching modes keeps the retained editor state (buffer, undo history, caret, selection) and the preview's own scroll position. Following the same passage across modes by source location is not supported; a mode switch lands at the top of the target surface. The upstream requirements and prototype findings are tracked in [Blocked features](../blocked-features.md).

### Editing tools

gpui-kit supplies syntax highlighting, the built-in find/replace panel, undo/redo, multicursor editing, automatic bracket and quote closing, and smart indentation. OpenIt uses these built-in editing capabilities and adds a small go-to-line prompt. Language changes update the retained editor's highlighter and wrapping without replacing its buffer, selection, or undo history. No second editing engine, no language server, no formatter. JSON-family schema completions are a `CompletionProvider` on `EditorState`; details in [Schema validation](schema-validation.md).

### Diagnostics

Schema results are pushed through the public `DiagnosticSet` on `EditorState` as Error for JSON syntax, unknown properties, and type mismatches. Schema download failure is not a diagnostic. Edits reset the set, so OpenIt reissues diagnostics for the matching revision after validation completes.

## PDF and images

### PDF

#### Engine

- Rendering: `hayro`, a pure-Rust PDF interpreter and rasterizer (CPU only, `#![forbid(unsafe_code)]`), with its `embed-fonts` feature so the fourteen standard fonts render without system lookups. No PDFium, no native library, nothing bundled per platform: the same build renders on macOS, Windows, and Linux.
- Text: `pdf-inspector`, a pure-Rust extractor (default feature set, no OCR) that reports every text run with its box, font, size, style flags, and page, and converts documents to Markdown. Its text items are the text layer for search, selection, and copy; the same crate produces the Markdown conversion below.
- Both crates read the same bytes independently. A `Pdf` handle is `Send + Sync`; the document library exposes plain synchronous functions and the application chooses the executor, as for every other kind.
- Coverage: hayro renders the common feature set and skips knockout groups, some blend and isolation cases, and non-embedded CID fonts; a page with such content renders with those elements wrong or missing, never blank. Text without a `ToUnicode` map (some Type 3 and symbolic fonts) has no text layer on that run; the page still renders.

#### Opening

- `.pdf` opens in the PDF reader; a file whose bytes do not start with `%PDF-` shows the unsupported-format explanation. Files over 256 MiB are refused with the size explanation.
- Password-protected documents prompt for the password in the window; it is held in memory for that window only and never persisted. A wrong password re-prompts; Cancel shows the explanation in place of the pages.
- Page count and every page's size come from the document structure before any page renders, so the layout is stable from the first frame.

#### Viewing

- Continuous vertical scrolling with a fixed gap between pages, centered. Opens at fit-to-width. Zoom in and out (Cmd/Ctrl+Plus, Cmd/Ctrl+Minus, scroll wheel with Cmd/Ctrl, pinch), fit to width (Cmd/Ctrl+0), actual size (Cmd/Ctrl+1, one PDF point per logical pixel). Zoom keeps the point under the cursor fixed.
- Page navigation: Option+Cmd/Ctrl+G (Alt+Ctrl+G on Windows and Linux) or a click on the status bar page readout opens a go-to-page prompt (`page`, prefilled, "Page X of N"); Page Up and Page Down move one page; Home and End move to the first and last page.
- Rendering runs off the UI thread at the device scale factor times the zoom. Visible pages render first, then one page above and below. A page not yet rendered at the current scale shows its previous bitmap scaled while the new one is produced, or the page background when there is none. The bitmap cache is bounded to the visible pages plus prefetch and at most 256 MiB per window; older entries are evicted first. The longest rendered edge is capped at 4096 px, the same texture cap images use; beyond that the page is rendered at the cap and scaled up on screen.
- Every render result carries the request generation it was computed for; a result for a stale scale or a replaced document is dropped.
- The status bar shows `Page X of N` on the left (click for go-to-page) and the zoom percentage on the right; the file type shows `PDF`. Pages are painted on the theme background with a page shadow from the theme palette; the paper itself is the PDF's own white.
- A vertical scrollbar sits at the right edge: it shows the position in the whole document, its thumb drags, and it follows the theme's scrollbar behavior like every other scrolling surface. Its gutter never starts a text selection.
- No thumbnails, no sidebar, no outline panel.

#### Text layer, search, selection

- The text layer is built in the background as soon as the document opens: every text item per page with its box in page coordinates. The status bar shows `Indexing…` while it runs on a long document and search is answered for pages already indexed.
- Boxes map to the rendered page through the page's visible box (`CropBox` intersected with `MediaBox`) and rotation, so highlights and selection sit on the glyphs they name.
- Search: `Edit > Find…` (Cmd/Ctrl+F) opens a find bar at the top of the pages. Matching is incremental as the query changes and case-insensitive; ligatures (`ﬁ`, `ﬂ`, and the rest of the Alphabetic Presentation Forms block) are folded, diacritics are folded, whitespace runs collapse, and a hyphen at a line end joins the split word so `configuration` matches `configu-` + `ration`. Every match on every visible page is highlighted; the current match is distinct. The bar shows `X of N` (`No matches` when none). Cmd/Ctrl+G and Shift+Cmd/Ctrl+G step forward and back, as do Enter and Shift+Enter in the find bar, scrolling the current match into view. Cmd/Ctrl+G with no matches opens the find bar. Escape closes the bar, clears the highlights, and keeps the scroll position. A results list under the bar shows one row per match with its page number and a line of context; clicking a row makes it the current match.
- Selection: click and drag selects text in reading order across page boundaries; Shift+click extends; double-click selects a word; Cmd/Ctrl+A selects the whole document. Cmd/Ctrl+C copies the selected text with one line break per text line and one blank line per page boundary. The selection paints as a translucent accent over the glyph boxes.
- Search and selection operate on the text layer only. Scanned pages stay viewable and have no text; OCR is out of scope.

#### PDF to Markdown

One PDF window holds three views of the same document: the pages, the generated Markdown as a preview, and that Markdown in the editor. Two title bar icons move between them:

- **PDF** returns to the pages.
- **Markdown** generates the Markdown the first time it is used, then moves between preview and editor: a pencil while previewing, an eye while editing. `File > Convert to Markdown` (Cmd/Ctrl+Shift+M) does the same thing as the icon; `View > PDF Pages` (Cmd/Ctrl+Shift+P) returns to the pages; Cmd/Ctrl+Shift+E toggles preview and editor once the Markdown exists.

The Markdown icon is disabled until the document has loaded and shows a working state while a generation runs; a second click cancels.

#### Generating

- Generation runs off the UI thread through `pdf-inspector` with headings, lists, code blocks, bold, italic, links, tables, header and footer stripping, hyphenation repair, and page-number removal enabled. The status bar reads `Generating…` during text extraction and `Rendering figures… X of N` while figures are produced. Cancel takes effect between figures; a cancelled result is dropped.
- The Markdown is written immediately, beside the PDF, with the same name and a `.md` extension (`paper.pdf` gives `paper.md`). A name already taken by a file OpenIt did not write in this session gets a number (`paper-2.md`), so nothing of the user's is overwritten. Regenerating replaces the file OpenIt wrote.
- Figures: every image placeholder the converter emits is replaced by a rendered crop of that region of the page (through hayro at twice the page's point size, capped at 2048 px on the longest edge, with 4 pt of slack so labels beside the artwork come along), so raster and vector figures both come through. The crops are written to a sibling folder named after the file (`paper-images/p003-01.png`: page, figure index) and linked relatively.
- A folder that cannot be written (a read-only disk, a document opened from a mounted image) sends the file and its figures to `<temp>/openit/<stem>.md` instead, and the status bar names the location.
- The status bar reports where the file landed (`Saved paper.md`), and adds `N pages skipped (no text)` when the document had pages the converter could not read.

#### The Markdown views

- The generated file is an ordinary document from that moment on: the preview and the editor are the same surfaces every Markdown document uses, with syntax highlighting, find and replace, undo, and the go-to-line prompt, and Cmd/Ctrl+S saves it. Autosave, draft recovery, and external-change detection apply to it exactly as they do to a Markdown window.
- The window's title bar shows the PDF's name in the PDF view and the Markdown file's name, with the dirty marker, in the other two.
- The status bar belongs to whichever view is showing: page position and zoom for the pages, line and column and the language for the editor.
- Closing the window with unsaved Markdown edits offers Save, Discard, and Cancel, the same as closing any dirty document. The PDF itself is never modified.
- Relaunching after a quit with unsaved edits restores the Markdown document in its own window, because it is a file of its own; the PDF window restores separately.

#### Failures

Failures stay in the PDF window's status bar and open no view. A document the classifier calls scanned or image-only: `This PDF has no text to convert.` An encrypted document reuses the password entered for viewing; when the converter still cannot decrypt, the status bar says so. A file that cannot be written anywhere, including the temporary folder, names the cause.

#### Quality contract

Faithful for text-heavy documents (papers, manuals, reports). Tables are best-effort: ruled tables are reconstructed, unruled ones become aligned lines. Slides come through readable, not faithful. Scanned pages produce nothing; there is no OCR. Predefined CJK CMaps are read by `pdf-inspector` from a directory named by `PDF_INSPECTOR_BCMAPS_DIR`; without it, CJK text encoded through a predefined CMap (not `Identity-H`) is skipped. Distribution sets that variable to a bundled copy.

### Images

#### Formats

- Raster, decoded by the `image` crate: PNG (including APNG), JPEG, GIF, WebP, BMP, ICO and CUR, TIFF, TGA, PNM. GIF, WebP, and APNG animations play. EXIF orientation is applied on decode.
- Vector: SVG rasterized by `resvg`, static subset only (no scripts, events, or animation). Fonts come from the system font database. `href` references resolve through the document resource resolver against the document's own directory; `data:` URLs embedded in the file are allowed; remote references render a placeholder for that element and the rest of the document still renders. Standalone SVG documents do not depend on GPUI's SVG renderer, so the Markdown-embedded SVG limitation does not apply to them.
- Every other type, including HEIC, AVIF, JPEG XL, JPEG 2000, PSD, OpenEXR, camera RAW, and GPU texture containers, is **not supported** and shows the unsupported-format explanation.

#### Viewing

- Open fitted to the window. Zoom in and out (Cmd/Ctrl+Plus, Cmd/Ctrl+Minus, scroll wheel with Cmd/Ctrl, pinch), fit (Cmd/Ctrl+0), actual size (Cmd/Ctrl+1), pan by dragging when the image exceeds the viewport.
- Dimensions and format come from the original file header, not the decoded texture. The status bar shows `W x H · FORMAT · size`, the frame count for animations, and the zoom percentage on the right.
- Decoding runs off the UI thread under the existing byte and pixel limits. The texture is capped at 4096 px on the longest edge; larger images are downsampled for display and actual size is exact only up to that cap. The original dimensions are still reported from the header.
- Background: white, black, or checkerboard. Default: checkerboard when the image has an alpha channel or is an SVG, otherwise the theme background. The choice is per window and not persisted. The checkerboard is painted in screen space (8 px cells in two theme-derived greys) so it stays sharp at every zoom. A swatch icon in the title bar cycles it; `View > Image Background` holds the same states as radios.
- Panning stops at the image: one that fits the window stays centered, and a larger one stops when its edge reaches the window edge.

#### Editing

- Rotate left and right by quarter turns (Cmd/Ctrl+L, Cmd/Ctrl+R) and flip horizontally and vertically, from `Tools` or the title bar icons. There is no crop, resize in place, or annotation.
- A turn reaches the file on its own: the write waits five seconds so a burst of turns is one write, and closing the window or quitting flushes it first. The status bar says `Saved` when it lands, and a draft covers the gap so a crash in between loses nothing. The document is never left for the user to save, and closing asks nothing. Cmd/Ctrl+Z steps back through the turns and writes again; Cmd/Ctrl+Shift+Z steps forward.
- Every write encodes from the bytes the file had, never from the previous write, so repeated turns cannot stack re-encodings and a turn back to the original orientation restores the original file byte for byte. A JPEG therefore pays one re-encode at most, whatever the user does.
- Writes go through the atomic save path for the formats `image` can encode (PNG, JPEG, BMP, TIFF, TGA, GIF including animation). ICO, CUR, PNM, and SVG cannot be written in place: the turn stays in the window, and the status bar names Export as the way out.
- Clipboard images are untitled PNG documents: they are the one image the window holds unsaved, so they checkpoint as drafts and Cmd/Ctrl+S opens Save As with a `.png` default. Closing one offers Save, Discard, and Cancel.
- Recovery checkpoints store the clipboard bytes and the transform under the draft size limit. A clipboard image over the limit is not checkpointed and the status bar says so.
- A file changed by another application replaces the window's bytes and clears the turn history; the file on disk is the truth.

#### Export

`File > Export...` (Cmd/Ctrl+Shift+S) opens one dialog and never changes the open document:

- Format: PNG, JPEG (with quality), WebP lossless, BMP, TIFF, GIF.
- Size: width and height with aspect lock, presets Original, 50%, 1080p (fit inside 1920 x 1080), and Custom. SVG rasterizes at the requested size directly; raster images resample with Lanczos3.
- Background: transparent (formats with alpha), white, black.
- Advanced (collapsed by default): sizing rule `Fit inside` (default) or `Exact size`; metadata `Strip` (default) or `Keep ICC profile`.
- The dialog shows the output pixel size, then the platform save prompt. Encoding runs off the UI thread; the window reports success or failure in place.

### Failure behavior

Corrupt or unsupported documents show an explanation in place of the viewer. A failed background job never replaces the current document with stale content.

## Configuration validation

JSON, JSONC, and JSON5 documents complete from a JSON Schema and show quiet schema errors. Details in [Schema validation](schema-validation.md).

- Coverage: `.json` (strict JSON), `.jsonc` and `.json5` (comments and trailing commas). Other formats are editable without schema validation.
- Parser: `jsonc-parser` supplies source ranges so diagnostics point into the original text. Validation never rewrites or reformats the document.
- Validator: `jsonschema` with default retrieval features disabled. All `$ref` resolution goes through the resource resolver with a depth cap of 8.
- Schema selection precedence: manual selection (local file or URL), then the document's `$schema`, then an unambiguous filename match from a bundled SchemaStore-derived catalog index. Ambiguous matches open the picker. Manual selection persists on the draft and in settings keyed by canonical path.
- Catalog: index only in the binary. Schema bodies come from cache, then the resolver.
- Scheduling: validation runs once on open, then 1 s after the last edit. Stale revisions are dropped. Compiled validators are reused until the schema identity changes.
- Completions: properties at the cursor, including nested object properties, and `enum` values from the loaded schema. No snippets. With no schema loaded, this provider contributes nothing.
- Feedback: `DiagnosticSet` Error for JSON syntax, unknown properties, and type mismatches. Status shows the schema name, or "No schema", muted like the language segment. Click opens the picker. Never a count, never danger or warning color.
- Download failure after allow: one log line. Not a diagnostic, not a status color, not a bar. Unknown domain still uses the permission bar.
- All results are advisory. Save stays available.

## Theme

One theme is in effect for the whole application: one theme for every window, applied from the OS appearance the application reports. It supplies every color: window chrome, editor, syntax highlighting, Markdown preview, status bar, prompts, and the permission bar. Two application-wide families persist across color-theme switches: UI Font for chrome and Code Font for monospace. Themes are Zed-format theme families: one JSON file holding one or more themes, each marked `dark` or `light`.

### Sources

- Bundled families ship with the application: One (One Dark, One Light), Ayu, Gruvbox, Warm Burnout. Ids are the lowercased theme name with spaces as hyphens (`one-dark`).
- User families are read from `<config_dir>/openit/themes/*.json` at startup, when the theme settings change, and when the settings file changes on disk. The rescan runs once for the application, not once per window. A bundled id wins over a user file with the same id. A file over 1 MiB or that fails to parse is skipped with a logged warning.

### Selection

- `mode = "system"` (default) follows the window appearance from the OS: dark appearances apply the `dark` theme id, light appearances the `light` id. The switch is live, without restart.
- `mode = "light"` or `"dark"` pins that kind regardless of the OS.
- Defaults: `light = "warm-burnout-light"`, `dark = "warm-burnout-dark"`. An unknown id falls back to the default of its kind; the settings file is left as written.
- The OS accent color is not used. GPUI exposes no accent API, and a theme needs a whole palette to keep contrast, so one foreign color would not compose with it.

### Menu

View > Appearance holds System, Light, Dark (radio). Choosing one writes `mode` to the settings file.

View > Color Theme..., Cmd/Ctrl+K Cmd/Ctrl+T, and the palette button in the title bar open the theme picker: a command palette listing the dark and light themes (the OS kind first) with live preview while moving; Enter writes the pick as the `light` or `dark` id and, when the pick is of the other kind than the one shown, pins `mode` to that kind; Escape restores the configured theme.

View > Font is the submenu for UI Font and Code Font. View > Font > UI Font... and Cmd/Ctrl+K Cmd/Ctrl+U open the UI Font picker; View > Font > Code Font... and Cmd/Ctrl+K Cmd/Ctrl+C open the Code Font picker. Each picker lists the fonts installed on the machine with live preview while moving; Enter writes that family; Escape restores the saved pair.

## Resource permissions

One resolver in the document library decides every document resource: local paths, remote images, and schema references.

### Local resources

- A document may load files relative to its own location and absolute paths on the same machine. Symbolic links resolve before the check.
- Untitled documents have no local base; relative references are denied.

### Remote resources

- Only `https://` and `http://`. Other schemes, embedded credentials, and `data:` are denied.
- Redirects that leave the permitted domain family are denied.

### Domain families

- Matching uses the registrable domain from the Public Suffix List (`psl`), updated with the application. An allowlist entry covers its registrable domain and every subdomain.
- Starter entries include `github.com`, `githubusercontent.com`, and `schemastore.org` as separate entries; any of them can be removed in settings.
- The application-wide "always allow" setting bypasses the allowlist until disabled.

### Decision order

Local file, then cache, then allowlist. Anything else shows the nonblocking permission bar with two choices: allow this domain family (persisted), or always allow remote content (setting). Denied resources are never fetched or cached. Multiple resources from one family collapse into one prompt.

### Markdown images

Images load through a per-document `ImageCache` installed as an ancestor of the `TextView`. GPUI consults it before its shared default loader, so the cache resolves the original URL against the document base and authorizes it before any fetch. Markdown source is never rewritten, which preserves source positions.

### SVG

SVG images from documents show a placeholder. The resolver security limitation and required upstream control are tracked in [Blocked features](../blocked-features.md).

### Schemas

`$ref` follows the same resolver with a depth cap of 8. Fetch failure after allow is a log line only: not a diagnostic, not a status color, not a bar. Unknown domain still uses the permission bar.

## Windows, opening, and the shell

### Open requests

Every entry point produces an open request: Finder or Explorer open, `openit <path>...`, drag onto icon or window, Cmd/Ctrl+O, Cmd/Ctrl+N from clipboard, and relaunch recovery. A path already open focuses its window instead of opening a duplicate. A path the readers refuse goes to the system's default application instead of a window, per [System handoff](system-handoff.md). Every other request ends in its own window.

### Command line

The app binary is the `openit` command: it hands requests to the running instance over a per-user local socket and starts one when none is running. A failed handoff starts a new instance rather than dropping the request. The command returns as soon as the request is handed over, the way `open` does on macOS: a new instance starts detached under the product name, with no terminal attached. Details in [Open requests](open-requests.md).

### Empty window

The placeholder is a document session in its empty state: the app mark, a muted Select a file button, and "or drag and drop a file, or paste". A drop or paste fills it; a second drop opens a new window. Details in [Open requests](open-requests.md).

### Nearby files

The title bar file name and Cmd/Ctrl+P open a fuzzy picker over the open document's directory; a pick replaces the document in the same window. Details in [Nearby files](nearby-files.md).

### Chrome

One title bar drawn by the application beside the traffic lights (gpui-component `TitleBar`, transparent system title bar): file name with the dirty marker, then the content actions on the right as bare icons (mode toggle for Markdown, rotate left and background cycle for images, PDF and Markdown views for PDFs). Clicks on those icons stop there, so the bar's own double-click does not zoom the window. The theme picker is reached from the View menu and Cmd/Ctrl+K Cmd/Ctrl+T, not from an icon. The icons carry no frame at rest; a rounded fill appears on hover and deepens while pressed, and each one shows a tooltip naming what a click does with its shortcut. The tooltips are mounted by the window, because gpui-component routes its own through the `Root` OpenIt does not use. The bar is filled flat with the theme's title bar color, overriding the toolkit's default gradient, so it reads the same in every theme. No second toolbar row. Menus and shortcuts hold everything else.

The status bar shows metadata on the left and the file type on the right. For text: line/column while editing (click opens the go-to-line prompt: `line[:column]`, prefilled with the current position, showing "Current Line: X of N"), absent in preview, which has no caret and the language name (click opens the language palette listing every compiled-in grammar; the pick changes highlighting for the session and decides whether the document offers Markdown preview). JSON-family documents show the schema name, or "No schema", muted left of the language name; a click opens the picker. Never a count, never danger or warning color. Images show dimensions, format, file size, and frame count on the left and the zoom percentage on the right. PDFs show `Page X of N` on the left (click opens the go-to-page prompt), indexing and generation progress beside it, and the zoom percentage on the right; a PDF window showing its generated Markdown shows that document's own readouts instead.

The bar floats over the text while editing, where a translucent strip reads well against a scrolling buffer. In every other view it takes its own opaque row, so an image, a page, or a rendered document is never covered by it. Markdown preview hides the status bar and editing shows it. `View > Always Show Status Bar` (`always_show_status_bar`, default `false`) pins it visible in both. A source warning, a save error, or a settings error shows it regardless of mode or setting.

Native menus: `OpenIt` (Install Command Line Tools... (macOS), Check for Updates..., Quit), `File` (New from Clipboard Cmd/Ctrl+N, Open... Cmd/Ctrl+O, Go to File... Cmd/Ctrl+P, Close Window, Save, Export... Cmd/Ctrl+Shift+S for images, Convert to Markdown Cmd/Ctrl+Shift+M for PDFs), `Edit` (Undo, Redo, Cut, Copy, Paste, Select All as OS actions; Find... Cmd/Ctrl+F for PDFs), `View` (Toggle Preview / Edit, Always Show Status Bar, Markdown Preview Width, Image Background, Zoom In, Zoom Out, Fit, Actual Size, Go to Page... Option+Cmd/Ctrl+G and PDF Pages Cmd/Ctrl+Shift+P for PDFs, Appearance, Color Theme...), `Tools` (Rotate Left, Rotate Right, Flip Horizontal, Flip Vertical for images). The binary is named `OpenIt` so the platform shows that name in the application menu.

### Settings

One TOML file in the platform config directory: autosave, `auto_check_updates` (default `false`), `always_show_status_bar` (default `false`), `allow_remote` (default `false`), `allowed_domains` (default `github.com`, `githubusercontent.com`, `schemastore.org`), `[theme]`: `mode` (`system` | `light` | `dark`), `light`, `dark` theme ids, `[font]`: `ui` (UI Font) and `code` (Code Font), manual schema choices, and the session window list used for recovery. Changes apply to open windows without restart. View > Font > UI Font... and Cmd/Ctrl+K Cmd/Ctrl+U write `ui`; View > Font > Code Font... and Cmd/Ctrl+K Cmd/Ctrl+C write `code`.

Markdown preferences use `markdown_preview_width` (`readable` | `wide` | `full_width`, default `readable`) and `markdown_mode` (`preview` | `edit`, default `preview`). Width changes apply to open previews; the mode preference only controls newly opened or restored Markdown windows. Opening other text formats does not change the remembered Markdown mode.

### Updates

Packaged builds check GitHub Releases through cargo-packager-updater. The manifest is `https://github.com/felipefdl/openit/releases/latest/download/latest.json`. Bundles are minisign-verified with the packager public key. Debug builds never check on their own.

`OpenIt > Check for Updates...` opens the Updates modal in the current window and checks immediately. The modal shows the running version, whether a newer package exists, download progress, and a Check automatically checkbox. Install downloads, verifies, and relaunches.

`auto_check_updates` is the opt-in. When it is on, a packaged app checks once a few seconds after launch. An available update opens the same modal. Up to date and failed automatic checks stay silent aside from a log line. Checking never runs until the user asks or opts in.

Publish writes `latest.json` onto the GitHub release. Platform keys are `macos-aarch64`, `macos-x86_64`, `linux-x86_64`, and `windows-x86_64`. Each entry has `signature`, `url`, and `format` (`app` / `appimage` / `nsis`). dmg and deb signatures are release assets only.

### Quick Look

The extension links the document library only, renders Markdown to a static presentation, and reads relative local images only within the sandbox's granted scope. It never writes drafts or prompts for network access.

## Errors

Every failure surfaces in the window it belongs to: unreadable file, decode failure, blocked resource, failed save. The message names the file and the cause and offers the next action (retry, save as, allow domain). No pane is ever left blank. A file with no reader in OpenIt never reaches a window; it goes to the system's default application ([System handoff](system-handoff.md)). Background failures are logged and affect the view only when they belong to the current revision.

## Performance

Budgets are measured on each platform and recorded in a dedicated spec once a baseline exists. The workload set is fixed here:

- Cold open to first paint: 100 KB Markdown, 5 MB PDF.
- Mode toggle latency on the 100 KB Markdown document.
- Scroll frame time on a 500-page PDF and a 24-megapixel JPEG; zoom and pan latency on the same JPEG and on a 2 MB SVG.
- Memory after opening ten windows.

Thresholds come from the first measured baseline, not from assumptions about Rust or GPUI.

## Testing

- Document library: unit tests for save and recovery semantics, external-change conflicts, format detection, schema precedence, resolver domain-family and path rules, JSONC diagnostic positions, PDF page geometry and text-box mapping, search normalization, and Markdown figure substitution.
- Desktop application: a small integration harness for mode switching (position and editor state retained), open-request routing, the permission bar flow, the PDF reader (page layout, go-to-page, find stepping, selection copy, conversion opening its window), and update checks. No screenshot tests.

## Prototype gates

The feasibility checks and upstream limitations are tracked in [Blocked features](../blocked-features.md), including reading-position alignment, SVG resource control, next-occurrence selection, and the PDF engine's rendering gaps.

## Dependencies and licensing

OpenIt is Apache-2.0. gpui-kit, gpui-component, gpui-base, and gpui-pre are Apache-2.0. `image` is MIT OR Apache-2.0. `resvg`, `usvg`, `tiny-skia`, and `fontdb` are MIT OR Apache-2.0. `jsonschema` and `jsonc-parser` are MIT. `psl` is MIT OR Apache-2.0. `hayro` and its crates are MIT OR Apache-2.0 (its embedded standard-font substitutes and CMaps carry their own permissive notices inside the crate). `pdf-inspector` is MIT and `lopdf` is MIT. `cargo-packager-updater` is Apache-2.0 OR MIT. Directly copied Apache-2.0 code carries its notice. Other products' names, logos, and bundled icon assets are excluded.

## References

- gpui-component `EditorState`: https://docs.rs/gpui-component/0.6.1/gpui_component/input/type.EditorState.html
- gpui-component `TextViewState`: https://docs.rs/gpui-component/0.6.1/gpui_component/text/struct.TextViewState.html
- hayro: https://docs.rs/hayro/latest/hayro/
- hayro-syntax `Page`: https://docs.rs/hayro-syntax/latest/hayro_syntax/page/struct.Page.html
- pdf-inspector Rust API: https://github.com/firecrawl/pdf-inspector/blob/main/docs/rust-api.md
- jsonschema: https://docs.rs/jsonschema/latest/jsonschema/
- jsonc-parser: https://docs.rs/jsonc-parser/latest/jsonc_parser/
- Public Suffix List: https://publicsuffix.org/learn/
- SchemaStore catalog: https://www.schemastore.org/
- resvg: https://docs.rs/resvg/0.48.1/resvg/
- image codecs: https://docs.rs/image/0.25.10/image/codecs/index.html

## History

- 2026-09-08: Draft written from the approved design discussion. Approved the same day; toolchain and quality bar recorded from the That's Home project with MSRV 1.98.1.
- 2026-09-08: Foundation implemented: workspace, quality gate, document kinds, loading, atomic save, one window per path, Markdown preview with lazy editor, explicit save.
- 2026-09-08: Persistence implemented: session identity, recovery drafts with restore on launch, close confirmation and quit flush, external-change detection with overwrite confirmation, opt-in autosave from settings.toml.
- 2026-09-08: Cross-mode reading position deferred to an upstream gpui-kit release; no fork or vendored copy.
- 2026-09-08: Resource permissions implemented: resolver and domain families in core, remote-access settings, per-document image cache with a permission bar, family-checked redirects, on-disk resource cache, SVG placeholder.
- 2026-09-08: Theme section added: bundled Zed-format families, system light/dark follow, no OS accent.
- 2026-09-08: Theme implemented: bundled and user Zed-format families, system light/dark follow per window, View > Appearance, settings hot reload.
- 2026-09-08: Status bar hidden in Markdown preview, with `View > Always Show Status Bar` to pin it and warnings or errors overriding both; the language pick now drives preview availability and rebuilds the editor so highlighting returns at once.
- 2026-09-08: Chrome tune-in: application-drawn title bar, native File/Edit/View menus with Open and New from Clipboard, theme picker, Warm Burnout defaults, status bar pickers (language, go-to-line), centered Markdown preview column with theme-driven styling and measured tables, preview text selection, quit no longer refused by a vanished window, application activated on launch.
- 2026-09-09: Approved gpui-kit 0.6.1's built-in multicursor editing, automatic bracket and quote closing, and smart indentation. Language changes retain editor state through the upstream highlighter refresh API.
- 2026-09-09: Recorded next-occurrence selection (Cmd/Ctrl+D) as blocked on upstream gpui-kit, alongside cross-mode reading position. Built-in cursor addition and rectangular selection remain supported.
- 2026-09-09: Added persistent Readable, Wide, and Full Width Markdown preview presets and remembered the last explicit Preview/Edit choice for subsequent Markdown windows and launches.
- 2026-09-09: Images section rewritten from the approved design: `image` raster set plus `resvg` SVG, everything else not supported; viewing with zoom, pan, backgrounds; rotate and flip with in-place save; export dialog with format, size, background, and advanced sizing and metadata options. The former "no permanent resolution cap" line replaced by the stated 4096 px display cap.
- 2026-09-09: Images implemented: the `image` raster set plus `resvg` SVG, a viewer with fit, zoom, pan, actual size and white/black/checkerboard backgrounds, rotate and flip with in-place save, clipboard images and copied files, drafts that keep an unsaved rotation, and an export dialog for format, size, background, and metadata.
- 2026-09-09: System handoff specified and implemented: a file the readers refuse goes to the operating system's default application, guarded against a shell that hands it straight back, and a handoff that leaves nothing on screen exits the process. Details in [System handoff](system-handoff.md).
- 2026-09-09: PDF section rewritten from the approved design: `hayro` renders, `pdf-inspector` supplies the text layer and the Markdown conversion; no PDFium and nothing bundled per platform. Continuous reader with fit-to-width, zoom, go-to-page; incremental normalized search with a results list; cross-page selection and copy; one-click PDF to Markdown with rendered figures. Thumbnails dropped.
- 2026-09-09: PDF to Markdown reshaped: one window holds the pages, the generated Markdown preview, and its editor, reached by two title bar icons. Generation writes the file and its figures beside the PDF at once (temporary folder when that fails), so the result is an ordinary document with the usual save, autosave, and recovery instead of an unsaved window.
- 2026-09-09: PDF implemented: hayro reader with fit-to-width, zoom, go-to-page, and a password prompt; pdf-inspector text layer with folded incremental search, a results list, cross-page selection and copy; Markdown generated into the document's own folder and shown in the same window as a preview or in the editor, reached by two title bar icons. Local images in the Markdown preview are blocked upstream (see Blocked features).
- 2026-09-11: Nearby files: a transient picker over the open document's directory replaces the document in the same window; the title bar file name, Cmd/Ctrl+P, and File > Go to File... open it. Details in [Nearby files](nearby-files.md).
- 2026-09-11: Open requests: the app binary is the `openit` command, one running instance owns every request, and an empty launch shows a window. Details in [Open requests](open-requests.md).
- 2026-09-11: Configuration validation points at [Schema validation](schema-validation.md): quiet status name, schema completions, and fetch failure as a log line only.
- 2026-09-12: Closing the last window quits on Linux and Windows through the existing quit path, unless an open is still in flight. macOS stays resident with no windows so Dock reopen can show an empty one.
- 2026-09-12: Font selector: UI Font and Code Font persist across color-theme switches; View > Font, Cmd/Ctrl+K Cmd/Ctrl+U, and Cmd/Ctrl+K Cmd/Ctrl+C open the pickers. Details in [Font selector](font-selector.md).
- 2026-09-14: Updates: `OpenIt > Check for Updates...` opens a modal that checks GitHub Releases; `auto_check_updates` is opt-in and does not run on launch until enabled. Publish writes `latest.json`.
- 2026-09-15: PDF Cmd/Ctrl+G is Find Next (Shift for previous); Go to Page moves to Option+Cmd/Ctrl+G. Cmd/Ctrl+V on the empty window opens the clipboard the same way as New from Clipboard. The empty-window line reads "or drag and drop a file, or paste".
