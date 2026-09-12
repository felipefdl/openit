# Rust quality bar

OpenIt holds a **maximum** quality bar for Rust. Prefer correct, tested, lint-clean code over speed. Read current official docs for a crate before adopting it. Do not relax gates to land work.

This document is the human-readable contract. Machine configuration lives in:

| File | Role |
|------|------|
| `rustfmt.toml` | Format, edition 2024, 120 columns, 2 spaces |
| `clippy.toml` | Clippy thresholds, test allows, disallowed methods, MSRV |
| `deny.toml` | Advisories, licenses, bans, sources |
| `.cargo/config.toml` | Cargo aliases |
| Root `Cargo.toml` `[workspace.lints]` | Rust and Clippy levels, added with the workspace |

## Toolchain

| Field | Value |
|-------|-------|
| Edition | 2024 |
| MSRV | 1.98.1 through `rust-version` in the workspace |
| Workspace | Virtual root `Cargo.toml`, resolver `"3"` |
| Layout | `crates/core` for the document library and `crates/app` for the GPUI application |
| Format | `cargo fmt --all` against `rustfmt.toml` |
| Clippy | Workspace lints and `clippy.toml`; CI and local checks use `-D warnings` |
| Tests | `cargo nextest run --workspace` |
| Supply chain | `cargo deny check`, `cargo audit`, `cargo machete` |

## Lint policy

Configured in root `Cargo.toml` `[workspace.lints]` and `clippy.toml`.

- `unsafe_code = forbid`
- Clippy groups `all`, `pedantic`, `nursery`, and `cargo` at **deny**, with a short allow-list for known noise without safety value
- The full Clippy `restriction` group is not enabled. Selected panic and silent-fail lints are denied individually, including:
  - `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`
  - `indexing_slicing`, `string_slice`, `as_conversions`
  - `print_stdout`, `print_stderr`, `dbg_macro`
  - `await_holding_lock`, `await_holding_refcell_ref`
  - `allow_attributes_without_reason`
- Tests can unwrap, expect, panic, print, and index through `allow-*-in-tests` in `clippy.toml`. Production code cannot.
- Prefer `#[expect(..., reason = "...")]` over bare `#[allow(...)]`.
- Public items require rustdoc through `missing_docs = deny`.
- Do not weaken workspace lints or turn off deny gates to land a change.

The owned workspace safety rule covers all OpenIt crates, including thin integration adapters. External sidecar processes and third-party dependencies sit outside the workspace `unsafe_code = forbid` boundary. Their unsafe code does not permit unsafe code in owned crates and remains subject to dependency, license, advisory, and operational review.

### Required `[workspace.lints]` shape

The root workspace `Cargo.toml` carries the full deny set documented in this section. Every member crate sets:

```toml
[lints]
workspace = true
```

Inheritance is opt-in per crate. Missing inheritance is a gate failure.

### Allowed Clippy noise

Keep these at `allow` unless a safety case requires a change:

- `module_name_repetitions`
- `missing_errors_doc`
- `missing_panics_doc`
- `must_use_candidate`
- `return_self_not_must_use`
- `double_must_use`
- `redundant_pub_crate`
- `multiple_crate_versions`
- `wildcard_dependencies`
- `cargo_common_metadata`
- `mod_module_files` and `self_named_module_files`
- `decimal_literal_representation`

Do not expand this allow-list casually.

## Conventions

- Public APIs: American English and rustdoc on public items.
- Errors: typed `Error` and `Result`, `?`, no stringly panics on I/O paths.
- Logging: `tracing` only. No `println!` or `eprintln!` in non-test code.
- Time: UTC for stored and wire times.
- Configuration: inject configuration into components. Do not call `std::env::set_var` or `remove_var`.
- Dependencies: established crates only. Check the latest compatible version and current docs before adoption.
- New crates: path under `crates/`, workspace member, and `[lints] workspace = true`.

## Profiles

Root `Cargo.toml` carries these profiles:

```toml
# Line tables keep panic and backtrace locations for workspace code and drop the
# type debuginfo that dominates test binary size and link time.
[profile.dev]
debug = "line-tables-only"
overflow-checks = true

# Dependencies rebuild only when the lockfile changes, so optimizing them costs
# one build and pays back on every test run. Workspace crates stay unoptimized.
# Debug assertions and overflow checks still apply everywhere.
[profile.dev.package."*"]
opt-level = 2
debug = false

# GPUI's layout and text pipeline is unusable at opt-level 0 even in dev.
[profile.dev.package.gpui-pre]
opt-level = 3

[profile.dev.package.gpui-pre-platform]
opt-level = 3

[profile.dev.package.gpui-pre-macros]
opt-level = 3

# Full debug info on demand: `cargo build --profile debugging`.
[profile.debugging]
inherits = "dev"
debug = true

[profile.release]
lto = true
codegen-units = 1
strip = true
overflow-checks = true
debug = false

# Build scripts and proc macros are host dylibs that rustc `dlopen`s during the
# build. Stripping one corrupts its load commands, and the macOS release build
# then fails with "mis-aligned LINKEDIT string pool". The shipped binary above
# stays stripped; only build-time artifacts opt out.
[profile.release.build-override]
strip = false
```

## Quality gate

Local completion claims and continuous integration run one recipe:

```text
just check
```

That recipe is the gate. It covers the format check, workspace lint inheritance, supply chain (`cargo deny check`, `cargo audit`, `cargo machete`), clippy with `-D warnings` on all targets and features, and `cargo nextest run --workspace`. The `justfile` holds the ordered steps; this document holds why the bar is what it is, so a step list here would only drift from it.

There is no `cargo test` fallback for the required nextest gate. An environment without nextest is not ready to run the full gate.

While a change is still in progress, verify the packages it touches with `just task <package>` (clippy plus nextest for one crate) or `just t '<nextest filter>'`. Neither replaces the gate: completion claims still require `just check`.

On macOS, the first execution of every freshly linked test binary is scanned by XProtect, which costs more than the compile itself on a large test suite and does not appear in cargo timings. Adding the terminal application under System Settings, Privacy and Security, Developer Tools exempts the binaries it spawns. That is a local machine trade-off between build speed and an operating system security feature, so it stays a per-developer choice and never a gate requirement. See the [cargo-nextest macOS notes](https://nexte.st/docs/installation/macos/).

Behavior changes also require:

- Tests in the same change for the new path or fixed defect.
- Documentation updates for altered contracts.
- An ADR when a durable architecture decision changes.

## Rules for agents and contributors

1. New public API or adapter contract requires tests in the same change.
2. A new dependency requires current docs review and green deny and audit checks.
3. Do not add `unwrap`, `expect`, or `panic!` in non-test code.
4. Do not disable or downgrade workspace lints to land a change.
5. An architecture fork with more than one valid option requires maintainer input.
6. Do not commit or push unless the user asks.
