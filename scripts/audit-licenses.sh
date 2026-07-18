#!/bin/sh
set -eu

target=${1:-$(rustc -vV | sed -n 's/^host: //p')}

if [ -z "$target" ]; then
    echo "Could not determine the Rust host target" >&2
    exit 1
fi

dependency_tree() {
    cargo tree \
        --locked \
        --target "$1" \
        --edges normal,build,dev \
        --prefix none \
        --format '{p}@@@{l}' | sed 's/ (\*)$//'
}

# The host graph is what this machine can ship today. The all-target graph is
# checked separately so a dormant Windows/Linux/build dependency cannot quietly
# violate the repository-wide source policy.
if ! host_tree=$(dependency_tree "$target"); then
    echo "Could not inspect the host dependency graph" >&2
    exit 1
fi
if ! all_tree=$(dependency_tree all); then
    echo "Could not inspect the cross-target dependency graph" >&2
    echo "Run cargo fetch for all locked targets before an offline audit." >&2
    exit 1
fi
tree=$(printf '%s\n%s\n' "$host_tree" "$all_tree" | sort -u)

missing=$(printf '%s\n' "$tree" | awk -F '@@@' 'NF < 2 || $2 == "" { print }')
unknown=$(printf '%s\n' "$tree" | awk -F '@@@' '
    function allowed(token) {
        return token == "MIT" || token == "MIT-0" || token == "Apache-2.0" ||
            token == "LLVM-exception" || token == "0BSD" ||
            token == "BSD-2-Clause" || token == "BSD-3-Clause" ||
            token == "ISC" || token == "Zlib" || token == "CC0-1.0" ||
            token == "Unlicense" || token == "BSL-1.0" ||
            token == "Unicode-3.0" || token == "NCSA"
    }
    function operator(token) {
        return token == "" || token == "AND" || token == "OR" || token == "WITH"
    }
    {
        expression = $2
        gsub(/[()\/]/, " ", expression)
        count = split(expression, tokens, /[[:space:]]+/)
        has_allowed_choice = 0
        has_and = 0
        for (i = 1; i <= count; i++) {
            has_allowed_choice = has_allowed_choice || allowed(tokens[i])
            has_and = has_and || tokens[i] == "AND"
        }
        for (i = 1; i <= count; i++) {
            token = tokens[i]
            if (operator(token) || allowed(token)) {
                continue
            }
            # A dependency offered under `MIT OR GPL`, for example, is used
            # under its permissive branch. Unknown/disallowed alternatives are
            # accepted only for a pure OR expression with an explicit allowed
            # choice. Any AND expression stays conservative and must be wholly
            # allowlisted.
            if (has_allowed_choice && !has_and) {
                continue
            }
            print $0 "@@@unexpected-token=" token
            break
        }
    }
')

if [ -n "$missing" ]; then
    echo "Dependencies with missing license metadata:" >&2
    printf '%s\n' "$missing" >&2
    exit 1
fi

if [ -n "$unknown" ]; then
    echo "Dependency outside the explicit permissive SPDX allowlist:" >&2
    printf '%s\n' "$unknown" >&2
    exit 1
fi

packages=$(printf '%s\n' "$tree" | sort -u | wc -l | tr -d ' ')
echo "License guard passed for $packages host and cross-target package records"
echo "Declared license expressions:"
printf '%s\n' "$tree" | awk -F '@@@' '{ print $2 }' | sort -u

if [ ! -f vendor/pdfium/LICENSE ] || [ ! -d vendor/pdfium/licenses ]; then
    echo "Bundled PDFium license bundle is incomplete" >&2
    exit 1
fi
