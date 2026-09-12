# OpenIt development recipes.
# Primary gate: `just check`

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
  @just --list

# Format check (does not write).
fmt-check:
  cargo fmt --all --check

# Format write.
fmt:
  cargo fmt --all

# Clippy with deny warnings for all targets and features.
clippy:
  cargo clippy --workspace --all-targets --all-features -- -D warnings

# Deterministic offline tests.
# --no-tests=pass: a fresh crate has zero tests; still exercise nextest.
test:
  cargo nextest run --workspace --no-tests=pass

# Lint and test one package (mid-change loop). Not the gate.
task pkg:
  cargo clippy -p {{pkg}} --all-targets -- -D warnings
  cargo nextest run -p {{pkg}} --no-tests=pass

# Filtered test run: `just t 'test(save)'`.
t expr:
  cargo nextest run --workspace -E '{{expr}}'

# Supply-chain and unused-deps checks.
deny:
  cargo deny check

audit:
  cargo audit

machete:
  cargo machete

# Verify every workspace member sets [lints] workspace = true.
lints-inherit:
  #!/usr/bin/env bash
  set -euo pipefail
  fail=0
  while IFS= read -r -d '' f; do
    if ! grep -qF '[lints]' "$f" || ! grep -qF 'workspace = true' "$f"; then
      echo "missing [lints] workspace = true: $f" >&2
      fail=1
    fi
  done < <(find crates -name Cargo.toml -print0)
  if [[ "$fail" -ne 0 ]]; then
    exit 1
  fi
  echo "all member crates inherit workspace lints"

# Run the app on one file: `just run README.md`.
run +paths:
  cargo run -p openit -- {{paths}}

# Render assets/brand and assets/app-icons from assets/brand/openit.svg.
brand *args:
  scripts/gen-brand.sh {{args}}

# Installers into target/release: `just package --formats dmg`.
package *args:
  umount /Volumes/OpenIt 2>/dev/null || true
  cargo packager --release -p openit {{args}}

# Bump the workspace version, commit, and tag. Does not push.
release version:
  sed -i '' 's/^version = "[^"]*"/version = "{{version}}"/' Cargo.toml
  sed -i '' 's/openit-core = { path = "crates\/core", version = "[^"]*"/openit-core = { path = "crates\/core", version = "{{version}}"/' Cargo.toml
  cargo update -w -p openit
  git add Cargo.toml Cargo.lock
  git commit -m "chore(release): v{{version}}"
  git tag "v{{version}}"
  @echo "Push with: git push origin HEAD --tags"

# Ordered cheapest first: a formatting failure reports in seconds instead of
# after the compile steps.
#
# Full quality gate (docs/rust-quality.md).
check: fmt-check lints-inherit deny audit machete clippy test
  @echo "quality gate passed"
