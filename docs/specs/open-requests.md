# Open requests

## Purpose

Every way a file reaches OpenIt ends in the same place: the `openit` and `oi` commands, a drop on a window or on the app icon, Finder or Explorer "Open with", the Dock, and an empty window waiting for any of them. Today each command invocation is a separate process and a launch with no paths shows nothing. This spec makes one running instance own every request and gives the empty launch a window. Extends [Windows, opening, and the shell](application-architecture.md#windows-opening-and-the-shell).

## Scope

- In: a per-user local socket and its protocol; the app binary acting as the command line client before it becomes the app; the `openit` and `oi` commands and their install modal on macOS; the empty window; drop of external paths on any window; OS open events (`on_open_urls`, `on_reopen`); file associations in packaging; command installation through the Linux and Windows packages.
- Out: clipboard Markdown/JSON detection and the manual type override (clipboard content, not an open request); creating a file that does not exist; a Linux or Windows door for the install modal (their packages install the commands); AppImage command installation (done by hand).

## Stack

- `interprocess` 2.4.4, default features: Unix domain socket on macOS and Linux, named pipe on Windows, one API. std has no named pipes. License `0BSD OR Apache-2.0`, both in `deny.toml`.
- `serde_json` (already a dependency) for the one-line request.
- gpui-pre 0.3.4 `ExternalPaths` with `on_drop`, `App::on_open_urls`, `App::on_reopen`. The test platform panics on the last two, so both sit behind a seam the tests drive, like `handoff::SystemOpener`.
- gpui 0.2.2 `Window::replace_root` for filling the empty window in place, as `replace_document` already does.
- cargo-packager 0.11.8 `[[package.metadata.packager.file-associations]]` for the macOS document types, the Windows "Open with" list, and the Linux `.desktop` MIME entries.
- Installer logic for command-line symlinks, elevation (`osascript ... with administrator privileges`, `pkexec`), and a Windows shim plus user PATH entry.

## Decisions

- The commands are symlinks to the app binary, not a launcher script and not a URL scheme: the binary is named `OpenIt`, so no lowercase `openit` exists anywhere today; a binary that checks the socket first forwards its own arguments, which avoids a separate launcher script.
- Client before GPUI: the process tries the socket before initializing the application, so a forward costs one connect and exits in well under a second with no second Dock icon.
- One routing rule for every entry point: an open request fills an empty window when one exists (first path) and opens a new window per remaining path; a path already open focuses its window; a refused path goes to the [system handoff](system-handoff.md). `openit` with no paths is a request for one empty window.
- The install modal is one view of installed state with an Apply button, not an Install/Uninstall pair: the checkboxes show what is installed, and Apply makes the filesystem match them.
- The modal is its own small window: `OpenIt > Install Command Line Tools...` must work on macOS with no document window open, and hosting it in four root views is four copies.
- The modal and its menu item exist on macOS only: gpui draws native menus on macOS alone, and the `.deb` and NSIS installers already have an install step, so those packages ship the commands.
- File associations declare exactly the extensions `kind.rs` accepts, `Editor` role for text and images (OpenIt writes both) and `Viewer` for PDF; the Dock accepts drops only for declared types.
- Nonexistent paths are refused by the client with a message, not created: creating files is a product decision this spec does not make.
- Socket and protocol live in `openit_core::ipc`, GPUI-free, so the tests use real sockets; installation lives in the app crate, because it is shell and process logic, not document logic.

## Executor latitude

Decide alone: module and file names inside the stated crates (`ipc`, `cli`, `cli_install`, `empty_view` are suggestions), JSON field names beyond `paths`, thread and channel wiring, pixel sizes and spacing in the modal and the empty window as long as they match the described layout, the drag-over ring color token, log line text, error wording on stderr, and test placement.

Stop and report: `interprocess` failing `cargo deny` or not building on one of the five `deny.toml` targets; the elevation route unable to create a link in `/usr/local/bin` on the current macOS; filling the empty window forcing a close-and-reopen instead of `replace_root`; the socket check adding more than about 50 ms to a cold launch; an extension needed by an association that `kind.rs` does not accept; cargo-packager's NSIS target unable to add a directory to the user PATH; anything that conflicts with `AGENTS.md`. A gpui or gpui-kit limitation that blocks an item gets a row in `docs/blocked-features.md` and the item stays open.

`just task` per item, `just check` on the final tree only, never weakened.

## Items

### 1. Socket and protocol in the document library

- New module in `crates/core`, no GPUI:
  - Name: `dirs::runtime_dir()/openit.sock` when XDG defines it, else `<data_local_dir>/openit/openit.sock`; on Windows `\\.\pipe\openit-<username>`.
  - Protocol: one connection per invocation. The client writes one JSON line `{"paths": ["/abs/a.md", ...]}` (an empty list asks for an empty window). The server dispatches and replies `ok` followed by a newline. Nothing else is ever written in either direction.
  - `send(paths) -> Result<(), NoInstance>`: connect, write, read the reply, with at most 200 ms for connect and 2 s for the reply. No socket, connection refused, timeout, or a reply that is not `ok` all mean "no running instance". The short connect timeout keeps a stale socket from delaying a cold start.
  - `listen() -> Result<Listener, Error>`: bind; when the address is in use and a connect attempt fails, remove the stale file and bind once more. The listener yields one request per accepted connection and answers `ok` after the caller has taken the paths.
- Done when:
  - A test starts a listener at a temp name, sends two paths, receives them in order, and the client gets `ok`.
  - A test with no listener returns `NoInstance` within the timeout.
  - A test that leaves a stale socket file behind binds over it.
  - `just task openit-core` is green.

### 2. Command entry and the listener thread

- `main` parses arguments by hand before anything else: `openit [path ...]`, `--help`, `--version`, `--` ends flags. Unknown flag: usage on stderr, exit 2. `--help` and `--version` print and exit 0.
- Each path is made absolute against the current directory. A path that does not exist or is a directory prints `openit: <path>: no such file` or `openit: <path>: is a directory` to stderr and is dropped. When paths were given and none survived, exit 1 without launching.
- The surviving list (possibly empty) goes to item 1's `send`. `Ok`: exit 0 at once, without waiting for a window. `NoInstance`: continue as the application with that list; an empty list opens the empty window (item 3).
- The running application starts item 1's listener on a dedicated `std::thread` (a blocking loop must not pin a `background_spawn` pool thread) and forwards each request through `async_channel` to a `cx.spawn` loop, the pattern `start_watch` uses in `session.rs`. Each request activates the application and goes through the routing rule in Decisions. Requests that arrive before the startup windows settle count as in flight for the handoff exit rule.
- After: 1
- Done when:
  - Unit tests cover the parser: paths, `--`, `--help`, `--version`, unknown flag.
  - A GPUI test feeds a request through the channel and a window for that path opens; a second request for the same path focuses it instead.
  - Manual: with OpenIt running, `target/release/OpenIt README.md` from another terminal opens the file in the running app, the second process exits 0 in under a second, and the Dock shows one icon. With no instance, the same command starts the app. `OpenIt --version` prints the crate version.

### 3. The empty window and reopen

- A fourth root view beside `DocumentView`, `ImageView`, and `PdfView`; the window helpers dispatch on it. A launch with no paths and no drafts opens one. Layout, centered in the window: `assets/brand/openit-glyph.svg` at 80 px in `ActivePalette.mark` at 0.6 opacity (already embedded), a ghost `Button` labeled "Select a file" that dispatches `OpenFile`, and one muted 12 px line under it reading "or drag and drop a file". No status bar. Title "OpenIt", no dirty marker. Everything else in the window stays quiet.
- Filling: the first path of any open request replaces this window's root through `Window::replace_root` (the `replace_document` path), keeping the window handle; remaining paths open new windows. Cmd/Ctrl+N with clipboard content fills it the same way.
- `on_reopen` (behind the seam) with zero windows opens an empty window; with windows open it does nothing. Close Window closes it; it never writes a draft.
- After: 2
- Done when:
  - GPUI tests: a launch with no paths and no drafts yields one empty window; an open request fills it (same window handle, root is now the document view) and a second request opens a new window; the button dispatches `OpenFile`; the reopen seam with zero windows opens one.
  - Manual: the window shows the mark, the ghost button, the line under it, and no status bar.

### 4. Drop on a window

- Every root view, the empty one included, accepts `on_drop::<ExternalPaths>`. Dropped paths become one open request under the routing rule: an empty window is filled by the first path; a document window is never replaced, each path opens a new window.
- While external paths are dragged over a window, a thin accent ring inside the window edge and nothing else; the empty window additionally brightens its mark and text. The ring goes away on leave and on drop.
- After: 3
- Done when:
  - GPUI test: two paths dropped on a document window open two new windows and leave the original untouched; dropped on an empty window they fill it and open one more.
  - Manual: the ring appears during drag-over on a text, an image, a PDF, and an empty window.

### 5. OS open events and file associations

- `on_open_urls` (behind the seam): `file://` URLs are percent-decoded to paths and become one open request; any other scheme is logged at info and ignored. Events before the startup windows settle count as in flight for the handoff exit rule.
- `crates/app/Cargo.toml` gains `[[package.metadata.packager.file-associations]]` entries built from the `kind.rs` table: Markdown (`md`, `markdown`, `mdx`), every text and code extension, the image set, `svg` and `svgz`, `pdf`. Nothing from the Unsupported arm. Role `Editor` for text and images, `Viewer` for PDF. A Linux `mime_type` per group.
- After: 2
- Done when:
  - GPUI test: the seam receives `file:///tmp/a%20b.md` and an `https://` URL; one open request with `/tmp/a b.md` results, the other is logged.
  - Core test: every extension the app declares resolves through `detect` to a non-Unsupported kind, and every Markdown, text, image, SVG, and PDF extension in `kind.rs` appears in the declared list.
  - Manual on macOS: `open -a OpenIt README.md` reaches a running instance; a `.md` dropped on the Dock icon opens; `open -a OpenIt paper.pdf` hands off and quits.

### 6. Command installation

- macOS: `OpenIt > Install Command Line Tools...` opens a dedicated window, about 420 x 260, not resizable, with two checkbox rows, each followed by its example in a mono muted line (`openit file.md`, `oi file.md`), a status line, Cancel, and Apply. The boxes open showing installed state; when neither is installed both are checked. Apply is disabled until a box differs from the installed state. Apply creates or removes `/usr/local/bin/openit` and `/usr/local/bin/oi` to match: directly when the directory is writable, else through the administrator prompt. Success: rows show the new state, status line "Open a new terminal to use them." A failure names the cause in the same line; a cancelled prompt shows nothing. Escape and Cancel close the window. The menu item and the window are registered on macOS only.
- Link target: `std::env::current_exe()` resolved through symlinks. The same resolver prefers `$APPIMAGE` when set, so the shared install code never links into an AppImage mount.
- Linux `.deb`: ships `openit` and `oi` in `/usr/bin` as two-line `exec /usr/bin/OpenIt "$@"` scripts through the packager `files` map. Windows NSIS: `[package.metadata.packager.nsis] preinstall-section` is injected NSIS (not a replacement of `installer.nsi`). It contains an install `Section` and an `un.` uninstall `Section`. Install creates `$INSTDIR\bin` (`$LOCALAPPDATA\OpenIt\bin` in current-user mode), writes `openit.cmd` and `oi.cmd` that invoke `"%~dp0..\${MAINBINARYNAME}.exe" %*`, appends that directory to `HKCU\Environment` Path when missing, and broadcasts `WM_SETTINGCHANGE`. Uninstall deletes the two cmd files, removes the directory from Path, and broadcasts again.
- After: 2
- Done when:
  - Unit tests: status from a temp directory (none, one, both links); the plan from checkbox state to create and remove sets; target selection with and without `$APPIMAGE`.
  - Manual on macOS: the menu item opens the window; Apply with both checked prompts for the password; `which openit oi` prints both under `/usr/local/bin`; `oi README.md` opens the file in the running app; reopening the window shows both checked; unchecking `oi` and applying removes only that link.
  - `just package` on Linux produces a `.deb` whose file list includes `usr/bin/openit` and `usr/bin/oi`.
  - The nsis `preinstall-section` in `crates/app/Cargo.toml` creates both cmd names and writes Path under `HKCU\Environment`.

### 7. Documents

- `docs/specs/application-architecture.md`: "Command line" and "Empty window" point here; the Modules table row for the `openit` command says the binary is the command; the `OpenIt` menu line gains "Install Command Line Tools... (macOS)"; a History row.
- `README.md`: the AppImage note that the commands are symlinked by hand.
- After: 6
- Done when:
  - Both spec sections link here and the History row exists.
  - `just check` passes on the final tree.

---

## History

| Date | Item | Event | Description |
|---|---|---|---|
| 2026-09-11 | - | Created | |
| 2026-09-11 | 1 | Shipped | Core `ipc` module with the per-user socket, JSON line protocol, `send`, and `listen`. |
| 2026-09-11 | 2 | Shipped | Command parser, client-first send, and a listener thread that routes requests to open or focused windows. |
| 2026-09-11 | 3 | Shipped | Empty window, fill-in-place routing, and the reopen seam. |
| 2026-09-11 | 5 | Shipped | OS `on_open_urls` seam, `file://` decoding, and packager file associations. |
| 2026-09-11 | 4 | Shipped | Drop of external paths on every root view, with the accent drag-over ring. |
| 2026-09-11 | 6 | Stopped | cargo-packager 0.11.8 NsisConfig has no PATH helper. Adding a directory to the user PATH needs a custom NSIS template, which the stop rule forbids. macOS install window, Linux `.deb` wrappers, and unit-testable Windows shim/PATH helpers are in the tree. |
| 2026-09-11 | 6 | Updated | Windows PATH uses nsis `preinstall-section` with matching install and uninstall sections. |
| 2026-09-11 | 6 | Shipped | NSIS preinstall-section writes the cmd shims and HKCU Environment Path. |
| 2026-09-11 | 7 | Shipped | Architecture spec and README updated: Command line and Empty window point here, the binary is the command, and AppImage commands are symlinked by hand. |
