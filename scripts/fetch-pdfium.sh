#!/bin/sh
set -eu

version=7763
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "$(uname -s)" != Darwin ]; then
    echo "GPUI PDF Reader currently supports macOS only; no unaudited binary was downloaded." >&2
    exit 1
fi
os=mac

case "$(uname -m)" in
    arm64|aarch64) arch=arm64 ;;
    x86_64|amd64) arch=x64 ;;
    *)
        echo "Unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

package="pdfium-$os-$arch.tgz"
case "$os-$arch" in
    mac-arm64) expected=9acf49e46c68992cd40810e88264b1ad171805d02fd41c4cca336aad6653b333 ;;
    mac-x64) expected=f455e0868ef7e5174a315de8789ee2b7a5544638d0ac7a3312ea7b68ebbc99cb ;;
    *)
        echo "No verified PDFium package for $os-$arch" >&2
        exit 1
        ;;
esac

temporary=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-pdfium.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
archive="$temporary/$package"
url="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$version/$package"

echo "Downloading PDFium Chromium $version ($os-$arch)…"
curl -L --fail --retry 3 --output "$archive" "$url"

if command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | awk '{print $1}')
else
    echo "Neither shasum nor sha256sum is available" >&2
    exit 1
fi

if [ "$actual" != "$expected" ]; then
    echo "PDFium checksum mismatch" >&2
    echo "expected: $expected" >&2
    echo "actual:   $actual" >&2
    exit 1
fi

destination="$root/vendor/pdfium"
mkdir -p "$destination"
tar -xzf "$archive" -C "$destination"
printf '%s\n' "$version" > "$destination/PINNED_BUILD"

echo "Installed and verified $destination/lib"
echo "The upstream LICENSE and licenses/ notices were retained for distribution."
