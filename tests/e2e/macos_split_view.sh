#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
first="$root/tests/fixtures/interaction.pdf"
second="$root/tests/fixtures/scientific-unlinked.pdf"
binary="${CARGO_TARGET_DIR:-$root/target}/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-split.XXXXXX")
log="$tmp_dir/app.log"
timeout_marker="$tmp_dir/timeout"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

fail() {
  printf 'E2E split view failed: %s\n' "$1" >&2
  sed -n '1,360p' "$log" >&2 || true
  exit 1
}

[ "$(uname -s)" = "Darwin" ] || {
  printf '%s\n' "macos_split_view.sh requires macOS" >&2
  exit 1
}
for fixture in "$first" "$second"; do
  [ -f "$fixture" ] || fail "missing fixture: $fixture"
done

cd "$root"
cargo build --locked

GPUI_PDF_READER_QA_TAB_COUNT=2 \
GPUI_PDF_READER_QA_SPLIT_VIEW_SCENARIO=1 \
GPUI_PDF_READER_QA_TIMEOUT_MS=35000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$first" "$second" >"$log" 2>&1 &
app_pid=$!

(
  sleep 65
  : >"$timeout_marker"
  kill "$app_pid" 2>/dev/null || true
  sleep 2
  kill -9 "$app_pid" 2>/dev/null || true
) &
watchdog_pid=$!

set +e
wait "$app_pid"
status=$?
set -e
app_pid=""
kill "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true

[ ! -f "$timeout_marker" ] || fail "native split flow exceeded the 65 second watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "app log contains a QA, panic, or GPU/Metal failure"
fi

expected='GPUI_PDF_READER_QA_SPLIT opened=2 visible=2 switched=1 bar_owner=1 resized=1 swapped=1 separated=1 closed=1'
actual=$(grep '^GPUI_PDF_READER_QA_SPLIT ' "$log" || true)
[ "$actual" = "$expected" ] || fail "split lifecycle report was missing or incomplete"

printf 'E2E split view: %s\n' "$actual"
