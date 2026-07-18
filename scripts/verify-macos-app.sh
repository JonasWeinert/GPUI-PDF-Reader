#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <app-bundle> <standard|minimal>" >&2
    exit 2
fi

app=$1
bundle=$2
case "$bundle" in
    standard|minimal) ;;
    *) echo "unknown bundle: $bundle" >&2; exit 2 ;;
esac

contents="$app/Contents"
executable="$contents/MacOS/gpui-pdf-reader"
resources="$contents/Resources"
pdfium="$resources/libpdfium.dylib"
notices="$resources/Notices"

test -x "$executable"
test -f "$contents/Info.plist"
plutil -lint "$contents/Info.plist" >/dev/null
test -f "$pdfium"
test -f "$resources/assets/themes/gpui-component.json"
test -f "$resources/assets/themes/LICENSE-APACHE"
test -f "$notices/RUST_DEPENDENCIES.tsv"
test -f "$notices/THIRD_PARTY_NOTICES.md"
test -f "$notices/PROJECT_LICENSE"
test -f "$notices/PDFium/LICENSE"
test -f "$notices/PDFium/licenses/pdfium.txt"
test -f "$notices/Themes/LICENSE-APACHE"

# This is the exact executable-relative candidate used by the PDFium loader.
(cd "$contents/MacOS" && test -f ../Resources/libpdfium.dylib)

file "$executable" | grep -q 'Mach-O'
file "$pdfium" | grep -q 'Mach-O.*dynamically linked shared library'
binary_arches=$(lipo -archs "$executable")
pdfium_arches=$(lipo -archs "$pdfium")
compatible=false
for architecture in $binary_arches; do
    for pdfium_architecture in $pdfium_arches; do
        if [ "$architecture" = "$pdfium_architecture" ]; then
            compatible=true
        fi
    done
done
[ "$compatible" = true ] || {
    echo "binary architectures ($binary_arches) do not match PDFium ($pdfium_arches)" >&2
    exit 1
}

# PDFium may identify itself by a relative install name because it is opened by
# an explicit absolute bundle path. Its dependencies must remain system/rpath
# libraries rather than build-machine paths.
unexpected=$(otool -L "$pdfium" | tail -n +3 | awk '{ print $1 }' | \
    grep -Ev '^(/System/Library/|/usr/lib/|@rpath/|@loader_path/|@executable_path/)' || true)
[ -z "$unexpected" ] || {
    echo "PDFium has unexpected dynamic dependencies:" >&2
    echo "$unexpected" >&2
    exit 1
}

if [ "$bundle" = minimal ]; then
    if grep -q '^wasmtime[[:space:]]' "$notices/RUST_DEPENDENCIES.tsv" || \
       grep -q '^key-reference[[:space:]]' "$notices/RUST_DEPENDENCIES.tsv"; then
        echo "minimal notice inventory contains standard-only dependencies" >&2
        exit 1
    fi
else
    grep -q '^wasmtime[[:space:]]' "$notices/RUST_DEPENDENCIES.tsv"
    grep -q '^key-reference[[:space:]]' "$notices/RUST_DEPENDENCIES.tsv"
fi

echo "Verified $bundle macOS app bundle: $app"
