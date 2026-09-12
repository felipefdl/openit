# OpenIt

## Vision

**Make reading a file and making a small correction immediate, without opening a workspace.**

OpenIt is an open-source native desktop viewer first and a focused single-file editor when needed. It brings Preview-style reading and useful text editing into one application, with the document receiving more attention than the interface around it.

OpenIt is licensed under Apache-2.0.

This document defines the intended product and its boundaries. It is the product source of truth, not a list of shipped features, an implementation spec, or a delivery plan.

## Who it serves and why

The starting use case is everyday file work: reading Markdown, inspecting images, reading PDFs, and making small changes to text, code, and configuration files. OpenIt serves people with that workflow, without asking them to open a project or manage a workspace.

The desired change is fewer application switches and less setup for a task that concerns one file. Replacing macOS Preview means replacing those viewing workflows, not copying every Preview feature. The same product matters equally on Windows and Linux.

## Product principles

- **Speed is a premise.** Opening a file, scrolling, switching modes, and making edits should feel immediate. Startup cost and resource use matter alongside rendering speed. Extra capability must justify its cost.
- **The document comes first.** Chrome stays compact and visually subordinate to the content.
- **One file per window.** Multiple windows are supported; a workspace is not the unit of interaction. Picking a nearby file replaces the document in the same window.
- **Useful editing, limited scope.** Small corrections and configuration changes belong here; a full development environment does not.
- **User control over changes and connections.** Saving a document, preserving a draft, and allowing network access are separate decisions.

## Product commitments

### Read in the appropriate mode

| Content | Opening experience |
| --- | --- |
| Markdown | Opens in the last selected preview/edit mode, with preview as the initial default. A compact button and keyboard shortcut switch modes while preserving reading position and editor state; the choice is remembered across files and launches. |
| Other supported text, code, and configuration files | Open directly in the editor. |
| PDF | A document reader with continuous scrolling, zoom, search that is fast and forgiving (ligatures, hyphenation, accents), page navigation, and selection/copying of available text. One click generates a Markdown version beside the file, figures included, and the same window switches between the pages, the Markdown preview, and the Markdown editor. |
| Images | Image viewing with useful metadata such as dimensions and format, quick rotation and flipping, a background switch for transparent content, and export to common formats at a chosen size. |

Common Markdown features include headings, lists, tables, task lists, highlighted code blocks, links, and images.

### Edit deliberately and keep unfinished work

The editor provides syntax highlighting, find/replace, undo/redo, go-to-line, multicursor editing, automatic bracket and quote closing, and smart indentation. Lightweight configuration validation starts with JSON Schema errors. It recognizes common configuration filenames and declared schemas automatically, with manual schema selection for other files. Schema errors are advisory and do not prevent saving. This does not require language servers.

Saving is explicit by default through Cmd/Ctrl+S; autosave is an opt-in setting. Quitting with unsaved edits asks per document: Save, Discard, or Cancel. Unsaved edits and clipboard-created documents are checkpointed as recoverable drafts, so a crash returns them on relaunch; a discarded document does not come back. Draft recovery does not write changes to the original files.

### Open from where the content already is

New from Clipboard, available through Cmd/Ctrl+N, opens images, text, and copied files in new windows. Images and text become unsaved documents; copied file references open their original files.

OpenIt recognizes clear Markdown or JSON in clipboard text, falls back to plain text, and always permits a manual type override. The selected type follows the same viewing/editing defaults as a file of that type.

Dragging files or content onto the app icon or a window opens new windows, one file or item per window, rather than replacing an existing document. An empty window offers a quiet placeholder with guidance such as "Drop a file here" and "Paste from clipboard".

Files OpenIt has no reader for still open: the path goes to the operating system's default application for its type, rather than to an OpenIt window that reports a refusal. Choosing OpenIt to open something never leaves the person without a way to read it.

### Keep the interface out of the way

Use one compact title/toolbar row, without large headers or stacked headers. Show tools relevant to the current content; place secondary actions in menus and shortcuts.

The View menu offers Readable, Wide, and Full Width Markdown preview presets. The selected width is remembered across files and launches; editor wrapping is unchanged.

A subdued contextual status bar provides useful information: line/column and language for text, dimensions and format for images, and page position for PDFs. Reading gets the quieter frame: Markdown preview hides the bar, editing keeps it, and a menu item pins it visible everywhere for people who want it there. A source warning or an error brings it back on its own. There are no sidebars: PDF search shows its results in a list under the find bar, not in a panel. A click on the title bar file name, or Cmd/Ctrl+P, opens a transient list of files in the open document's directory; a pick replaces the document in the same window.

### Make remote access a visible choice

Document rendering and validation use local or cached resources by default, with an editable starter allowlist for common remote sources such as GitHub. Starter entries can be removed in settings.

When a remote image or schema needs permission, show a nonblocking bar rather than interrupting reading or editing. The choices are:

- Allow the resource's domain family and persist that entry in the allowlist. A stored domain covers itself and its subdomains, rather than one exact origin.
- Always allow remote content across the application, until the user disables that setting.

Allowlisting grants network access; it does not establish that hosted content is safe. The exact starter entries and domain-family matching rules belong in the security and resource-loading specs.

## Platform and reuse commitments

macOS, Windows, and Linux are equal product targets, with no Mac-first sequence. The standalone application uses Rust and GPUI. Reusable code and rendering work from other projects is allowed when it fits this contract. Inheriting another application's interface, feature limits, or architecture is not a requirement.

The command line executable is `openit`.

On macOS, a bundled Quick Look extension supplies Finder's Space-to-preview integration for supported registered types. It can share Rust document logic while using a presentation layer different from the GPUI application. Reusing the exact GPUI renderer inside the extension is not a requirement.

Quick Look integration is not a promise to replace every system preview handler. Its type routing and sandboxed access to supporting files require validation. The platform-specific extension does not change the equal standing of the standalone application on other operating systems.

## Boundaries and conditional additions

OpenIt is not a replacement for a full IDE. General code formatting, completions, diagnostics from language servers, and project/workspace tooling are outside the agreed editor scope. Lightweight schema validation is the deliberate exception for configuration files.

Mermaid diagrams and mathematical notation are conditional additions, not baseline Markdown requirements. Ready-made native libraries are acceptable if integration stays small. These features do not justify a custom rendering engine or an additional browser/JavaScript runtime.

PDF editing is outside the agreed scope. PDF to Markdown is faithful for text-heavy documents (papers, manuals, reports); tables are best-effort, slides come through readable rather than faithful, and scanned pages produce nothing because there is no OCR. Image editing is limited to rotation, flipping, and export; crop, resize in place, annotations, and signatures are an explicitly discussed expansion, not a delivery commitment. Other lightweight schema formats are candidates, not promised support.

## What success looks like

For the supported workflows, OpenIt removes the need to open Preview for viewing or a full editor for a small correction. Opening another file does not disrupt the document already being read. Choosing a nearby file replaces the document in the same window. Switching Markdown modes does not lose the user's place. Unfinished work survives without silently overwriting files.

Performance must be demonstrated on each target platform, not inferred from the choice of Rust or GPUI. Specs establish representative workloads and measurable budgets for opening, interaction latency, and resource use; this vision does not invent benchmark results or thresholds.

## Unresolved decisions and evidence

Detailed specs must settle supported format coverage, performance budgets, remote-resource matching and starter entries, and the feasibility of conditional Markdown features. Quick Look API/source research establishes an integration route, not a tested OpenIt extension or measured performance.

## References

- [Roman Pichler: product vision guidance](https://www.romanpichler.com/blog/tips-for-writing-compelling-product-vision/) informs the distinction between purpose, product direction, and an implementation plan. OpenIt's commitments come from the product discussion, not the template.
- [Apple: Quick Look UI](https://developer.apple.com/documentation/quicklookui) documents the extension integration route and supported presentation approaches.
