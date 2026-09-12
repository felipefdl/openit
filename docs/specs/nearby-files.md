# Nearby files

## Purpose

Reading one file in a folder and wanting the next one should not mean leaving OpenIt. A click on the title bar file name, or Cmd/Ctrl+P, opens a fuzzy file picker rooted at the open document's directory; a pick replaces the document in the same window. It is a transient list, not a file explorer: no sidebar, no tree, no folder state kept between uses. Extends [Windows, opening, and the shell](application-architecture.md#windows-opening-and-the-shell).

## Scope

- In: a fuzzy picker over one directory at a time, typed path navigation (`../`, `sub/`, absolute paths, `~`), same-window document replacement across every document kind, the title bar entry point, the shortcut, and the macOS `File` menu item.
- Out: recursive listing, a persistent panel, recent files, bookmarks, file operations (rename, delete, create), and any setting. Unsupported files are not listed; the [system handoff](system-handoff.md) covers files handed to OpenIt directly.

## Stack

- `nucleo-matcher` 0.3 (`Config::DEFAULT.match_paths()`, `AtomKind::Fuzzy`, `CaseMatching::Ignore`, `Normalization::Smart`, `Atom::indices` for match positions): fuzzy path ranking; MPL-2.0, already allowed in `deny.toml`.
- gpui-kit `component::command::{Command, CommandGroup, CommandItem, CommandState}`: the palette `theme_picker.rs` already runs without `Root`.
- `std::fs::read_dir` for the listing. No `walkdir`, no `ignore`.
- `Window::replace_root` (gpui 0.2.2) for the same-window swap.

## Decisions

- Transient palette, not a sidebar or tree: `VISION.md` forbids sidebars and a workspace model; a list that closes on pick is navigation, not a workspace.
- The picked file replaces the document in the same window: the goal is moving between siblings, and opening a window per glance leaves a pile behind. Dirty documents get the existing Save/Discard/Cancel prompt first.
- Title bar file name is the entry point, with Cmd/Ctrl+P and a `File` menu item: native menus render on macOS only in gpui 0.2.2 (Linux and Windows store them and draw nothing), so a menu cannot be the only door.
- One directory per listing, no recursion: one `read_dir` is fast on any folder, and recursion is the first step toward the explorer this feature is not.
- Unsupported files are hidden, not listed: a row that hands off to another application is a surprise inside a picker whose purpose is reading here.
- Match on the entry name, not the path: the root is fixed per listing, so path characters would only add noise to the score.
- Same-window swap rebuilds the root view per kind: `open_document_window` already chooses the view by kind, and a picker that lists a PDF beside a Markdown file and then refuses it is worse than the rebuild.

## Executor latitude

Decide alone: module and type names in core and app (`browse` is a suggestion), the chevron icon, the muted root prefix styling, row height, palette width, the debounce constant, how match positions become bold spans in a `CommandItem`, error wording, and test placement.

Stop and report: a second entry point, listing more than one directory level, showing unsupported files, a persisted setting, a dependency beyond `nucleo-matcher`, opening in a new window instead of replacing, or anything that conflicts with `AGENTS.md`. A gpui-kit or gpui limitation that blocks an item gets a row in `docs/blocked-features.md` and the item stays open.

`just check` runs on the final tree only and is never weakened. `#[expect]` with a reason where the codebase already does it; no `allow`, no `unwrap` outside tests, `tracing` for the unreadable-directory cause.

## Items

### 1. Directory listing and query parsing in the document library

- New module in `crates/core` with two pure entry points:
  - `parse(input, root) -> (dir, needle)`: the input splits at its last `/`. The left side resolves against `root`; it may contain `..`, start with `~` (home directory), or be absolute. The right side is the needle. A trailing `/` yields an empty needle. No filesystem access during parsing.
  - `list(dir) -> Result<Vec<Entry>, Error>`: one `read_dir`, non-recursive. An entry is a name, whether it is a directory, and its `DocumentKind` from `kind::detect` (filename only, no file reads). Files whose kind is `Unsupported` are dropped. Symbolic links are followed for the directory and kind checks only.
  - Dotfiles and dot-directories are excluded unless the needle starts with `.`; `list` takes that flag from the caller.
  - Order: directories first, then files, each group by name case-insensitively.
  - An unreadable directory is an `Error` carrying the path and the `io::Error`, never `Ok(vec![])`.
- Done when:
  - On a temp dir holding `.hidden`, `notes.md`, `report.docx`, `sub/`: the listing is `sub/`, `notes.md`; with the dot flag, `.hidden` joins it; `report.docx` never appears.
  - `parse("../sub/rep", root)` gives `<parent of root>/sub` and `rep` (`.` and `..` collapse lexically, so the picker's root prefix reads `/tmp/` rather than `/tmp/dir/../`); `parse("/tmp/", root)` gives `/tmp` and an empty needle; `parse("~/x", root)` gives `<home>` and `x`.
  - `list` on a directory without read permission returns the error variant.

### 2. Fuzzy ranking

- `rank(entries, needle) -> Vec<Ranked>`, in the same core module. Empty needle: the listing order, no positions. Otherwise nucleo scores each entry name (directories and files in one pool); ties break by name; the result is capped at 100 rows and carries the matched character positions for rendering. Directory rows keep their trailing `/` in the displayed name.
- After: 1
- Done when:
  - 300 entries and any needle give at most 100 rows.
  - Needle `rdm` places `README.md` above `readme-draft.md`, with positions at `R`, `d`, `m`.
  - An empty needle returns the listing order untouched.

### 3. Same-window document replacement

- `open_document_window` in `crates/app/src/window.rs` keeps building new windows. A new entry point replaces the document in an existing window: the path goes through the same identity and kind routing, and `Window::replace_root` installs the view for the new kind (text, image, or PDF). The window title updates.
- Rules, in order:
  - Picked path equal to the current document's path: nothing happens.
  - Picked path open in another window: that window is focused; this one keeps its document.
  - Current document dirty, including a PDF window with unsaved generated Markdown: the existing Save/Discard/Cancel prompt runs first. Cancel leaves the document and buffer intact. Discard removes that document's recovery data, as closing does.
  - Otherwise the outgoing session ends without writing a draft, and the new document opens in this window with the usual mode, position, and recovery behavior a fresh window would have.
- After: 1
- Done when (integration harness, per the architecture spec's Testing section):
  - A Markdown window replaced with a PNG has an image view as its root, the new title, and no recovery entry for the outgoing session.
  - Replacing with the current path changes nothing observable.
  - Replacing with a path open in window B focuses B; window A still holds its document.
  - A dirty document gets the prompt; Cancel keeps the buffer.

### 4. The picker

- A palette on gpui-kit `Command`, without content search, recents, or the `:line` suffix. It opens rooted at the document's directory; an untitled or empty window roots at the home directory. The input shows the current root as a muted prefix.
- Enter on a file row calls item 3 and closes the picker. Enter on a directory row rewrites the input to `<name>/`, which moves the root and clears the needle. Typing `../`, `sub/`, `~/`, or an absolute path moves the root the same way. Escape closes.
- Listing and ranking run off the UI thread after a 100 ms debounce. Every result carries the query it was computed for; a result for a stale query is dropped.
- The unreadable-directory error is one row in the list naming the directory and the cause; the input stays editable so `../` recovers.
- After: 2, 3
- Done when (integration harness):
  - Opening the picker on a Markdown window and typing `../` changes the root prefix and the rows.
  - Enter on a directory row leaves the input reading `<name>/` and the rows are that directory's.
  - Two queries issued back to back, with the first listing landing second, show only the second query's rows.
  - An unreadable directory shows the error row and `../` restores a listing.

### 5. Entry points

- One shared file name element in `crates/app/src/title_bar.rs`, with the dirty marker and a chevron, replaces the three per-view title elements in `document_view.rs`, `image_view.rs`, and `pdf_view.rs`. Click opens the picker; hover follows `toolbar_button`'s tint.
- Cmd/Ctrl+P is bound without a context so it works in every view, including a focused editor. `File > Go to File...` carries it on the native menu.
- After: 4
- Done when (smoke test, no screenshot test):
  - All three views show the chevron beside the file name; click and Cmd+P open the picker.
  - `File > Go to File...` is present on macOS with the shortcut, and `menus::build` lists it.
  - A focused text editor receives Cmd+P as the picker, not as text input.

### 6. Documents

- `VISION.md`: line 62 loses "and there is no file explorer" and gains one sentence describing the transient nearby-files list; the one-file-per-window principle (line 23) and the success paragraph (line 95) each gain the same-window replacement rule.
- `docs/specs/application-architecture.md`: a "Nearby files" subsection under Windows, opening, and the shell pointing here, `Go to File... Cmd/Ctrl+P` in the `File` menu line, and a History row.
- After: 5
- Done when:
  - `VISION.md` no longer contains "there is no file explorer".
  - The architecture spec has the subsection, the menu entry, and the History row.
  - `just check` passes on the final tree.

---

## History

| Date | Item | Event | Description |
|---|---|---|---|
| 2026-09-11 | - | Created | |
| 2026-09-11 | 1 | Shipped | Core `browse` module with `parse`, `list`, `Entry`, and `Error::Browse`. |
| 2026-09-11 | 2 | Shipped | Core `rank` and `Ranked` in `browse`, with nucleo-matcher. |
| 2026-09-11 | 3 | Shipped | App `replace_document` same-window swap via `Window::replace_root`. |
| 2026-09-11 | 4 | Shipped | App `NearbyPicker` palette with debounce, stale-drop, and `NearbyPicker::new` opener. |
| 2026-09-11 | 5 | Shipped | Title bar `file_name`, `GoToFile` action, Cmd/Ctrl+P, File menu item. |
| 2026-09-11 | 1 | Updated | `parse` collapses `.` and `..` lexically after the smoke test showed `/tmp/dir/../` in the root prefix. |
| 2026-09-11 | 6 | Shipped | VISION.md and the architecture spec record the transient nearby-files list and same-window replacement. |
