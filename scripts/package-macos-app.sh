#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <standard|minimal> <reader-binary> [output-directory]" >&2
    exit 2
}

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
bundle=$1
binary=$2
output=${3:-target/dist/$bundle}

case "$bundle" in
    standard|minimal) ;;
    *) usage ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$binary" in
    /*) ;;
    *) binary="$root/$binary" ;;
esac
case "$output" in
    /*) ;;
    *) output="$root/$output" ;;
esac

[ -f "$binary" ] || { echo "reader binary not found: $binary" >&2; exit 1; }
[ -x "$binary" ] || { echo "reader binary is not executable: $binary" >&2; exit 1; }
pdfium="$root/vendor/pdfium/lib/libpdfium.dylib"
[ -f "$pdfium" ] || { echo "PDFium library not found: $pdfium" >&2; exit 1; }

app="$output/GPUI PDF Reader.app"
staging="$output/.GPUI PDF Reader.app.staging.$$"
trap 'rm -rf "$staging"' EXIT HUP INT TERM
rm -rf "$staging"
mkdir -p "$staging/Contents/MacOS" "$staging/Contents/Resources/assets"

cp "$binary" "$staging/Contents/MacOS/gpui-pdf-reader"
chmod 755 "$staging/Contents/MacOS/gpui-pdf-reader"
cp "$pdfium" "$staging/Contents/Resources/libpdfium.dylib"
cp "$root/packaging/macos/Info.plist" "$staging/Contents/Info.plist"
cp -R "$root/assets/themes" "$staging/Contents/Resources/assets/themes"
python3 "$root/scripts/generate-bundle-notices.py" \
    "$bundle" "$staging/Contents/Resources/Notices"

rm -rf "$app"
mv "$staging" "$app"
trap - EXIT HUP INT TERM
sh "$root/scripts/verify-macos-app.sh" "$app" "$bundle"
echo "Assembled $bundle app bundle at $app"
