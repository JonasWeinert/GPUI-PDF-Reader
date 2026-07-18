#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings
cargo test --locked \
  --manifest-path vendor/pdfium-render-tile/Cargo.toml \
  test_tiled_rendering_matches_full_rendering -- --test-threads=1
sh scripts/audit-licenses.sh
