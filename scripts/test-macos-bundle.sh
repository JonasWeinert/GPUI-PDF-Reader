#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-bundle.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

printf 'fn main() {}\n' > "$temporary/smoke.rs"
rustc "$temporary/smoke.rs" -o "$temporary/gpui-pdf-reader"

sh "$root/scripts/package-macos-app.sh" \
    standard "$temporary/gpui-pdf-reader" "$temporary/standard"
sh "$root/scripts/package-macos-app.sh" \
    minimal "$temporary/gpui-pdf-reader" "$temporary/minimal"

standard_inventory="$temporary/standard/GPUI PDF Reader.app/Contents/Resources/Notices/RUST_DEPENDENCIES.tsv"
minimal_inventory="$temporary/minimal/GPUI PDF Reader.app/Contents/Resources/Notices/RUST_DEPENDENCIES.tsv"
standard_count=$(wc -l < "$standard_inventory" | tr -d ' ')
minimal_count=$(wc -l < "$minimal_inventory" | tr -d ' ')
[ "$standard_count" -gt "$minimal_count" ] || {
    echo "standard inventory should contain more dependencies than minimal" >&2
    exit 1
}

echo "macOS bundle assembly smoke test passed"
