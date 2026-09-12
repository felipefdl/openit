<br/>
<p align="center">
  <img src="assets/brand/openit-mark.png" width="200px" alt="OpenIt"></img>
</p>

# OpenIt

A half-bounce app: the window is up before the Dock icon finishes its first bounce. Open a file, change one line, save, close.

You want to flip a value in a config file or read a README. The choices are a Preview that cannot edit, or an IDE that loads a workspace, a file tree, and an extensions marketplace before it shows you the file. OpenIt is the native app for the in-between: a viewer first, an editor one keystroke away, and no wait either way.

One file per window. No project, no sidebar, no language server booting in the background.

## Fast where it counts

Cold start opens straight to the document. Scrolling and mode switches do not stutter. Pick a sibling file from the same folder (click the file name, or Cmd/Ctrl+P) and the window stays put while the document changes.

The `openit` command hands paths to the running app, or starts one, and returns at once. `oi` is the short name.

Rust on GPUI. Native on macOS, Windows, and Linux.

## Config files

Text and code open in the editor: syntax highlighting, find and replace, go to line, multicursor. JSON, JSONC, and JSON5 get schema errors inline, without a language server. Those errors do not block save.

Save is explicit. Autosave is opt-in. Quit asks Save, Discard, or Cancel. A crash returns the draft on relaunch and never writes over the original file.

## Markdown

Markdown opens in preview, rendered as soon as the window is. When a heading or a link is wrong, switch to the editor in the same window and fix it. Reading position and editor state survive the switch.

Remote images and schemas stay off until you allow the domain.

## Also opens

PDFs scroll, zoom, and search. Select and copy the text on the page. One click writes a Markdown version next to the file, figures included.

Images rotate, flip, and export. SVG too.

A type OpenIt cannot read still opens: the path goes to the system default instead of a window that only knows how to refuse.

## Install

Installers are on [GitHub Releases](https://github.com/felipefdl/openit/releases):

- macOS: `.dmg` (Apple Silicon and Intel), signed and notarized
- Linux x86_64: `.deb` and AppImage
- Windows x86_64: NSIS installer (`-setup.exe`)

The AppImage does not ship `openit` or `oi`. Symlink those by hand.

## Build

Requires Rust 1.98.1 or newer and [`just`](https://github.com/casey/just).

```sh
just check
cargo run -p openit -- path/to/file.md
just package
```

The binary is named `OpenIt`. `just package` builds a release installer; pass `--formats` to pick `dmg`, `deb`, `appimage`, or `nsis`.

## License

Apache-2.0. See [LICENSE](LICENSE). The OpenIt name and logo are not covered; see [NOTICE](NOTICE).
