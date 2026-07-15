#!/bin/sh
set -eu

target=${1:-$(rustc -vV | sed -n 's/^host: //p')}

if [ -z "$target" ]; then
    echo "Could not determine the Rust host target" >&2
    exit 1
fi

tree=$(cargo tree \
    --locked \
    --offline \
    --target "$target" \
    --edges normal,build \
    --prefix none \
    --format '{p}@@@{l}')
tree=$(printf '%s\n' "$tree" | sed 's/ (\*)$//')

missing=$(printf '%s\n' "$tree" | awk -F '@@@' 'NF < 2 || $2 == "" { print }')
forbidden=$(printf '%s\n' "$tree" | awk -F '@@@' '
    toupper($2) ~ /(^|[^A-Z])(AGPL|LGPL|GPL|MPL|EPL|CDDL|SSPL)(-|[^A-Z]|$)/ { print }
')

if [ -n "$missing" ]; then
    echo "Dependencies with missing license metadata:" >&2
    printf '%s\n' "$missing" >&2
    exit 1
fi

if [ -n "$forbidden" ]; then
    echo "Prohibited license-family identifier in the active graph:" >&2
    printf '%s\n' "$forbidden" >&2
    exit 1
fi

packages=$(printf '%s\n' "$tree" | sort -u | wc -l | tr -d ' ')
echo "License guard passed for $packages packages on $target"
echo "Declared license expressions:"
printf '%s\n' "$tree" | awk -F '@@@' '{ print $2 }' | sort -u
