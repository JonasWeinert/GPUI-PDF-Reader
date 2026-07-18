#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-open-dialog.XXXXXX")
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
  printf 'E2E open dialog failed: %s\n' "$1" >&2
  sed -n '1,260p' "$log" >&2 || true
  exit 1
}

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_open_dialog.sh requires macOS" >&2
  exit 1
fi
[ -f "$fixture" ] || fail "missing fixture: $fixture"

cd "$root"
cargo build --locked

GPUI_PDF_READER_QA_WINDOW_COUNT=1 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" >"$log" 2>&1 &
app_pid=$!

ready=0
for _ in $(jot 100); do
  if osascript -e 'tell application "System Events" to return exists process "gpui-pdf-reader"' 2>/dev/null | grep -q true; then
    ready=1
    break
  fi
  sleep 0.1
done
[ "$ready" -eq 1 ] || fail "app process did not become available to System Events"

osascript \
  -e 'tell application "System Events" to tell process "gpui-pdf-reader"' \
  -e 'set frontmost to true' \
  -e 'keystroke "o" using command down' \
  -e 'delay 1' \
  -e 'keystroke "g" using {command down, shift down}' \
  -e 'delay 0.4' \
  -e "keystroke \"$fixture\"" \
  -e 'key code 36' \
  -e 'delay 0.6' \
  -e 'key code 36' \
  -e 'end tell'

(
  sleep 50
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

[ ! -f "$timeout_marker" ] || fail "native picker flow exceeded the 50 second watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "app log contains a QA, panic, or GPU/Metal failure"
fi

window_count=$(grep -c '^GPUI_PDF_READER_QA_WINDOWS windows=1 pdf_views=1 settled=1$' "$log" || true)
[ "$window_count" -eq 1 ] || fail "picker selection did not settle the source PDF window"
report_count=$(grep -c '^GPUI_PDF_READER_QA ' "$log" || true)
[ "$report_count" -eq 1 ] || fail "expected exactly one reader QA report, got $report_count"
report=$(grep '^GPUI_PDF_READER_QA ' "$log")
case "$report" in
  *"pending=0 "*"debouncing=0 "*"status=Ready") ;;
  *) fail "picker-opened reader did not reach a quiet Ready state" ;;
esac

printf 'E2E open dialog: %s\n' "$(grep '^GPUI_PDF_READER_QA_WINDOWS ' "$log")"
