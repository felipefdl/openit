# Git diff

## Purpose

A text file with uncommitted git changes shows what changed without leaving OpenIt. A title-bar button switches the window to a read-only diff against `HEAD`, and the nearby-files list marks changed files.

## Scope

- In: text documents (`DocumentView`: Markdown, text, code, configuration); the nearby-files picker (`nearby_picker.rs`).
- Out: PDF and image windows, staging, committing, blame, other refs or branches, untracked files, directory rows in the picker, diffing unsaved buffer text.
- Later: changed-line markers in the editor gutter, only once gpui-kit's editor accepts gutter markers; no line tints or overlay in their place.

## Stack

- `gpui_kit::component::diff` (`Diff`, `DiffState`, `DiffFile::parse`, `DiffMode`): already in the tree through gpui-kit 0.8.1; parses a git patch and highlights by language.
- The `git` binary through `std::process::Command`: no new crate.

## Decisions

- Changes mean the file on disk against `HEAD` (`git diff HEAD -- <file>`), staged and unstaged together; untracked files have none: answers "what changed since the last commit".
- The diff reflects the saved file, not unsaved buffer text: keeps the patch git's own, with no blob reading or diff crate.
- `git` CLI over `gix`: no dependency tree added; the patch is exactly what `DiffFile::parse` reads.
- Git missing, no repository, or a git failure means no changes: `tracing::debug!`, no button, no icon, no user-facing error.
- Layout is a View > Diff Layout choice: Automatic (side by side at 1100 px wide or more, inline below, switching live on resize), Side by Side, or Inline; app-wide and persisted as `diff_layout`, like the Markdown preview width.
- The diff toggles over the current mode and returns to it; it is never the opening mode and never touches `markdown_mode`.
- Picker icons are markers only; picking a row opens the file as before.

## Executor may decide

Decide alone: the marker and button icons, tooltip wording, test placement, log wording, the `openit_core::git` function names and signatures. Stop and report: anything not listed above, any conflict with `AGENTS.md`, any new crate dependency, any change to how the Markdown mode persists, anything that delays opening a file or the picker listing.

## Items

### 1. Git queries in openit-core

- New `openit_core::git` module: the set of changed files in one directory (`git diff HEAD --name-only`, files relative to that directory), and the patch against `HEAD` for one file. Both are synchronous; callers run them on the background executor.
- Git missing, not a repository, or a non-zero exit returns no changes and logs at `debug`.
- Done when:
  - `just task openit-core`: green
  - tests on a temporary repository: a modified tracked file (staged, unstaged, or both) is in the changed set and returns a non-empty patch; a clean file, an untracked file, and a directory outside any repository report no changes

### 2. Diff button and diff view

- After: 1
- Button beside Edit/Preview in `render_title_row`, shown only while the patch is non-empty; it opens a unified `Diff` with `DiffFile::with_language` set from the document language.
- View > Show Changes and Cmd/Ctrl+Shift+D (bind `cmd-shift-d` and `ctrl-shift-d` like the existing pairs in `main.rs`) do the same.
- Turning the diff off returns to the previous mode; while the diff shows, the Edit/Preview button also leaves it. The status bar hides in the diff as it does in Markdown preview.
- The patch reloads on open, on save, and on the file-watch pump; when it becomes empty while the diff shows, the window returns to the previous mode and the button disappears.
- Add a dated line to `docs/specs/application-architecture.md`.
- Done when:
  - `just task openit`: green
  - tests: a modified file shows the button and a clean file does not; the button and Cmd/Ctrl+Shift+D enter the diff and return to the previous mode; an emptied patch while the diff shows returns the window
  - VM run: a modified file in a git repository shows the button, and a click shows a single-column diff with syntax colors

### 3. Changed-file marker in the nearby-files picker

- After: 1
- One status query per listing, on the background executor; rows render at once and file rows gain the marker when it returns. Directory rows never get one.
- Done when:
  - `just task openit`: green
  - tests: a listing with one modified file marks only that row; a directory outside any repository marks none
  - VM run: Cmd/Ctrl+P in a repository with one modified file shows the marker on that row

### 4. Side-by-side layout on wide windows

- After: 2
- The diff's mode follows the window width (Decisions); a resize across 1100 px switches it while the diff shows.
- Done when:
  - `just task openit`: green
  - tests: a window 1100 px wide or more shows `DiffMode::Split`, a narrower one `DiffMode::Unified`, and a resize across the line switches it
  - VM run: a wide window shows the two versions side by side; narrowing it returns to one column

### 5. Diff Layout menu

- After: 4
- View > Diff Layout submenu with Automatic, Side by Side, and Inline, checked from settings; a pick applies to every open diff at once.
- Done when:
  - `just check`: green
  - tests: Inline holds on a wide window, Side by Side holds on a narrow one, Automatic follows the width; the setting round-trips through the settings file
  - VM run: each menu choice switches an open diff

---

## History

| Date | Item | Event | Description |
|---|---|---|---|
| 2026-10-28 | - | Created | |
| 2026-10-28 | 1 | Dispatched | Inline on feat/git-diff, start 417c7a9 |
| 2026-10-28 | 2 | Dispatched | Inline on feat/git-diff, start 417c7a9 |
| 2026-10-28 | 3 | Dispatched | Inline on feat/git-diff, start 417c7a9 |
| 2026-10-28 | - | Updated | Layout follows window width instead of one column only; item 4 added. |
| 2026-10-28 | 4 | Dispatched | Inline on feat/git-diff, start 417c7a9 |
| 2026-10-28 | 1 | Executor decided | Git runs only when `.git` exists in the directory or an ancestor; fsmonitor off, no external diff or textconv, literal pathspecs, fixed `a/` `b/` prefixes; every configured filter driver is blanked, and git is skipped when one cannot be. |
| 2026-10-28 | 2 | Executor decided | Window activation re-reads the patch so a commit made elsewhere is seen; the per-file diff header is hidden; long lines wrap; embedded PDF documents never query git. |
| 2026-10-28 | 3 | Executor decided | One status query per listed directory; only the latest request's answer is kept. |
| 2026-10-28 | - | Updated | Editor gutter markers stay in Later until gpui-kit supports them; no workaround. |
| 2026-10-28 | 1 | Updated | Review: content filters could run and names were globbed; fixed in d4cb58f and f308beb. |
| 2026-10-28 | 2 | Updated | Review: focus taken from an open picker, stale diff language, git in embedded documents; fixed in d4cb58f and f308beb. |
| 2026-10-28 | 3 | Updated | Review: superseded status answers and a test that recomputed the marker; fixed in d4cb58f and f308beb. |
| 2026-10-28 | 1 | Completed | Merged into main |
| 2026-10-28 | 2 | Completed | Merged into main |
| 2026-10-28 | 3 | Completed | Merged into main |
| 2026-10-28 | 4 | Completed | Merged into main |
| 2026-10-28 | - | Updated | Layout becomes a View menu choice (Automatic, Side by Side, Inline); item 5 added. |
| 2026-10-28 | 5 | Dispatched | Inline on main, start 29d0352 |
| 2026-10-28 | 5 | Executor decided | `diff_layout` is an app-wide persisted setting mirroring `markdown_preview_width`, since menu checkmarks come from Settings; the submenu is named Diff Layout. |
| 2026-10-28 | 5 | Completed | Committed on main |
