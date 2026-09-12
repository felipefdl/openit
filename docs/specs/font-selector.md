# Font selector

## Purpose

OpenIt uses whatever UI and monospace families gpui-kit picked. A Color Theme-style palette lists the fonts installed on the machine so those two families can be chosen and kept across theme changes.

Extends [Theme](application-architecture.md#theme) and [Settings](application-architecture.md#windows-opening-and-the-shell).

## Scope

- In: two application-wide families, UI Font and Code Font; a `Command` palette per family listing `all_font_names()`; live preview; persist in `settings.toml`; apply on every color-theme switch; `View > Font` submenu; `Cmd/Ctrl+K` chords.
- Out: font size, ligatures, loading a font file, per-window fonts, PDF and SVG fonts, a shipped catalog, comma-separated fallback strings, filtering the list to monospace, title bar buttons.
- Later: font size, ligatures, loading a font file, a monospace-only Code list, title bar buttons.

## Stack

- `cx.text_system().all_font_names()`: the public GPUI list Zed and gpui-kit already use. No extra crate.
- gpui-kit `Command` palette: `theme_picker.rs` already runs without `Root`.
- gpui-component `Theme.font_family` and `Theme.mono_font_family`: the two slots the toolkit already paints from. Size fields are not written.

## Decisions

- Two families, not one: a Code-only picker leaves chrome on the toolkit default.
- System fonts through `all_font_names()`, not a catalog: same public API as Zed's `FontFamilyCache` and gpui-kit `mono_font.rs`.
- `[font]` table with `ui` and `code`, not Zed's `buffer_font_family`: matches `[theme]`, and this app has no buffer jargon.
- Re-apply fonts inside `apply_theme`: color configs today leave font fields default, so a theme switch would wipe a pick.
- Same full list in both pickers: a name heuristic hides Menlo, SF Mono, and Consolas.
- `View > Font` submenu, not two top-level items: View is already busy; `Appearance` is the same pattern.
- Chords as well as the menu: native menus draw on macOS only in this gpui, so a menu cannot be the only door.
- No title bar buttons: chrome stays compact, and Color Theme already took that slot.
- Live preview, Enter writes, Escape restores: same contract as Color Theme.
- Two openers, one overlay type: each opener writes one key.
- No `VISION.md` edit: this is a settings control, not a product-principle change.

## Executor latitude

Decide alone: module and type names (`font_picker.rs` next to `theme_picker.rs` is a suggestion), whether UI and Code share one type parameterized by slot, how `all_font_names()` is cached, debounce, placeholder copy, row height, palette width, whether fonts go through `ThemeConfig` or onto `Theme` after `apply_config`, test placement, and error wording.

Stop and report: font size, ligatures, loading a font file, per-window fonts, PDF or SVG fonts, fallback strings, a shipped catalog, filtering the list to mono, a title bar button, a new dependency, editing `VISION.md`, or anything that conflicts with `AGENTS.md`. A gpui-kit or gpui limit that blocks applying a family gets a row in `docs/blocked-features.md` and the item stays open.

`just check` runs on the final tree only and is never weakened.

## Items

### 1. Persist `[font]` in settings

- New `[font]` table on the core `Settings` type, sibling of `[theme]`, with string keys `ui` and `code`.
- Omit a key (or the whole table) to keep the gpui-kit default for that slot.
- An unknown family name is kept as written, same as an unknown theme id.
- Done when:
  - round-trip TOML with both keys set: both values come back
  - a file that omits `[font]`: toolkit defaults
  - an unknown family name: still present on the loaded struct

### 2. Apply families in `apply_theme`

- After: 1
- Every `apply_theme` path sets `Theme.font_family` from `font.ui` and `Theme.mono_font_family` from `font.code`. Size fields are not touched.
- A name missing from `all_font_names()` uses that slot's toolkit default; the settings file is left as written.
- `.SystemUIFont` is a valid `ui` value and is applied even when it is absent from `all_font_names()`.
- Done when:
  - after `apply_theme`: `Theme.font_family` and `Theme.mono_font_family` match the saved pair
  - switching color theme: those two families unchanged
  - a name missing from `all_font_names()`: no panic, toolkit default for that slot, file unchanged

### 3. Font picker overlay

- After: 2
- One `Command` overlay, two openers. Both lists are `all_font_names()`. UI Enter writes `font.ui` only. Code Enter writes `font.code` only.
- Moving the highlight applies the matching `Theme` field (debounced). Enter commits. Escape restores the saved pair and closes.
- Done when:
  - UI Font opener: list is `all_font_names()`; Enter writes `font.ui` only
  - Code Font opener: same names; Enter writes `font.code` only
  - moving the highlight: matching `Theme` field changes; Escape restores the saved pair and closes

### 4. Entry points

- After: 3
- `View > Font > UI Font...` and `View > Font > Code Font...`.
- `Cmd/Ctrl+K Cmd/Ctrl+U` opens UI Font. `Cmd/Ctrl+K Cmd/Ctrl+C` opens Code Font.
- Every view that already hosts Color Theme, including the empty window. A focused text editor receives the chord, not as text input.
- Done when:
  - `menus::build`: `View > Font > UI Font...` and `Code Font...` present
  - both chords: matching picker opens from the empty window and from a focused text editor

### 5. Architecture spec

- After: 4
- `docs/specs/application-architecture.md` Theme and Settings sections name the two families, the submenu, and the chords, plus a History row.
- `VISION.md` is not edited.
- Done when:
  - architecture Theme and Settings sections name the two families, the submenu, and the chords
  - a History row is present
  - `VISION.md` is unchanged
  - `just check` on the final tree: pass

---

## History

| Date | Item | Event | Description |
|---|---|---|---|
| 2026-09-12 | - | Created | |
| 2026-09-12 | 1 | Dispatched | |
