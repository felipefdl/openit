<br/>
<p align="center">
  <img src="assets/brand/openit-mark.png" width="200px" alt="OpenIt"></img>
</p>

# OpenIt

Read a file and make a small correction immediately, without opening a workspace.

OpenIt is a native desktop viewer first and a focused single-file editor when needed: Markdown preview with an editor one keystroke away, text and code editing with syntax highlighting, PDF and image viewing, one window per file. Built in Rust on GPUI.

## Status

Early development. The product contract is `VISION.md`; the design is `docs/specs/application-architecture.md`.

Working today: Markdown preview and editing, text and code editing with syntax highlighting, JSON Schema validation for JSON, JSONC, and JSON5, image and SVG viewing with rotation and export, PDF reading with search, selection, and one-click Markdown generation, files with no reader handed to the system's default application, a save-or-discard prompt on quit with drafts that survive a crash, external-change detection, opt-in autosave, image permissions for Markdown, bundled and user color themes (Cmd+K Cmd+T), an application-drawn title bar, status-bar pickers for language, schema, and go-to-line, and the `openit` command, which hands paths to the running instance or starts one and returns at once. Quick Look is still ahead.

## Downloads

Installers are on [GitHub Releases](https://github.com/felipefdl/openit/releases):

- macOS: `.dmg` (Apple Silicon and Intel), signed and notarized
- Linux x86_64: `.deb` and AppImage
- Windows x86_64: NSIS installer (`-setup.exe`)

The AppImage does not ship `openit` or `oi`; those commands are symlinked by hand.

## Build

Requires Rust 1.98.1 or newer and [`just`](https://github.com/casey/just).

```sh
just check   # format, lint, tests, dependency audit
cargo run -p openit -- path/to/file.md   # binary is named OpenIt
just package # cargo packager --release (pass --formats to pick dmg, deb, appimage, nsis)
```

Cut a tagged release with `just release <version>` (bumps the workspace version, commits, tags; does not push). Push the tag, then review the draft GitHub release.

## License

Apache-2.0. See [LICENSE](LICENSE). The OpenIt name and logo are not covered; see [NOTICE](NOTICE).
