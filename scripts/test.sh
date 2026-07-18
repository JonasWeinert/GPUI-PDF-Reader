#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# Wasmtime and GPUI produce a very large incremental graph. CI never reuses it,
# and a full local quality run is more reliable when it cannot exhaust the
# disk midway through linking. Callers can still opt in explicitly.
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}

cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --no-deps -- -D warnings
cargo check --locked -p gpui-pdf-reader --no-default-features
sh scripts/audit-boundaries.sh
cargo test --locked \
  --manifest-path vendor/pdfium-render-tile/Cargo.toml \
  test_tiled_rendering_matches_full_rendering -- --test-threads=1
sh scripts/audit-licenses.sh
