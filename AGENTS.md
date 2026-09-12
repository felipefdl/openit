# OpenIt (`openit`)

## Sources of truth

- Read `VISION.md` before product, UI, architecture, specification, or planning work.
- Keep confirmed product decisions in `VISION.md`; keep detailed specifications and implementation plans separate.

## Scope and assumptions

- Get the project owner's approval before turning an assumption or open product question into a requirement.
- Reused implementations must match `VISION.md`; they are not a second behavior contract.

## Documents

- Keep implementation plans outside the repository.
- Refer to maintainers by role, not personal name, in project documentation.
- Keep machine-specific filesystem locations out of project documentation.
- Make contributor instructions self-contained, using repository files or public documentation as references.

## Code and local rules

- Use Rust edition 2024 and MSRV 1.98.1 (`rust-version` in the root `Cargo.toml`).
- Quality bar: `docs/rust-quality.md`. Machine configuration: root `[workspace.lints]`, `rustfmt.toml`, `clippy.toml`, `deny.toml`. Every crate sets `[lints] workspace = true`.
- `just check` is the only completion gate. Do not weaken lints or deny gates to land a change.
- No `unwrap`, `expect`, or `panic!` outside tests. `tracing` only; no `println!` in non-test code.
- Before editing code, read every `AGENTS.md` from the repository root through the target directory.
- When creating a package or crate, add a focused child `AGENTS.md` with only package-specific obligations.
- Child rules narrow or override root rules explicitly; do not duplicate root rules.

## Packaging and release

- Packager config: `[package.metadata.packager]` in `crates/app/Cargo.toml`. Formats from the CLI (`dmg`, `deb`, `appimage`, `nsis`); rpm is not built. `just package` is `cargo packager --release -p openit`.
- Brand and app icons: `just brand` renders `assets/brand/openit-glyph.svg`, `assets/brand/openit-mark.png`, and `assets/app-icons/` from `assets/brand/openit.svg`; never hand-edit the derived files.
- Publish: `.github/workflows/publish.yml` on `v*` tags. Native runners, cargo-packager 0.11.8, Apple signing and notarization (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_CONTENT`), minisign via `PACKAGER_SIGN_PRIVATE_KEY` and `PACKAGER_SIGN_PRIVATE_KEY_PASSWORD`. Uploads a draft GitHub release.
- Release procedure: `just release <version>` bumps `[workspace.package].version`, commits `chore(release): v<version>`, tags `v<version>`. Push the tag (`git push origin HEAD --tags`), review the draft release, then publish it.
