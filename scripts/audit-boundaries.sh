#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

audit() {
    package=$1
    forbidden=$2
    # Audit declared, direct implementation ownership. GPUI itself has broad
    # transitive dependencies (including an HTTP client); that does not mean a
    # component crate imports or exposes those facilities.
    tree=$(cargo tree --locked --offline -p "$package" --depth 1 --edges normal,build --prefix none --format '{p}')
    violations=$(printf '%s\n' "$tree" | awk -v forbidden="$forbidden" '
        BEGIN { count = split(forbidden, denied, " ") }
        {
            for (i = 1; i <= count; i++) {
                if ($1 == denied[i]) {
                    print
                    break
                }
            }
        }
    ')
    if [ -n "$violations" ]; then
        echo "$package crosses a forbidden dependency boundary:" >&2
        printf '%s\n' "$violations" >&2
        exit 1
    fi
}

# Runtime-neutral contracts and domain crates cannot acquire platform,
# rendering-engine, network, or executable-extension implementations.
neutral_forbidden="gpui gpui-component pdfium-render reqwest zed-reqwest wasmtime key-extension-wasm key-extension-gpui key-pdfium key-pdf-gpui key-safe-http"
audit key-editor-core "$neutral_forbidden"
audit key-pdf-core "$neutral_forbidden"
audit key-pdf-runtime "$neutral_forbidden"
audit key-extension-api "$neutral_forbidden key-pdf-core key-pdf-runtime key-pdf-extension-api"
audit key-extension-host "$neutral_forbidden key-pdf-core key-pdf-runtime key-pdf-extension-api"
audit key-pdf-extension-api "gpui gpui-component pdfium-render reqwest zed-reqwest wasmtime key-extension-wasm key-extension-gpui key-pdfium key-pdf-gpui key-safe-http"

# Reusable GPUI components may know their semantic dependencies, but never the
# concrete PDF engine, application network stack, or executable runtime.
audit key-editor-gpui "pdfium-render reqwest zed-reqwest wasmtime key-pdfium key-safe-http gpui-pdf-reader"
audit key-pdf-gpui "pdfium-render reqwest zed-reqwest wasmtime key-pdfium key-safe-http gpui-pdf-reader"
audit key-extension-gpui "pdfium-render reqwest zed-reqwest wasmtime key-pdfium key-safe-http gpui-pdf-reader"
audit key-ui-gpui "pdfium-render reqwest zed-reqwest wasmtime key-pdfium key-safe-http gpui-pdf-reader"

echo "Workspace dependency boundaries passed"
