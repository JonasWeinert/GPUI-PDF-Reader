#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
first="$root/tests/fixtures/interaction.pdf"
second="$root/tests/fixtures/scientific-unlinked.pdf"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-multiwindow.XXXXXX")
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
  printf 'E2E multiwindow failed: %s\n' "$1" >&2
  sed -n '1,260p' "$log" >&2 || true
  exit 1
}

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_multiwindow.sh requires macOS" >&2
  exit 1
fi
for fixture in "$first" "$second"; do
  [ -f "$fixture" ] || fail "missing fixture: $fixture"
done

cd "$root"
cargo build --locked

GPUI_PDF_READER_QA_WINDOW_COUNT=2 \
GPUI_PDF_READER_QA_TIMEOUT_MS=25000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$first" "$second" >"$log" 2>&1 &
app_pid=$!

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

[ ! -f "$timeout_marker" ] || fail "native app exceeded the 50 second watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "app log contains a panic or GPU/Metal failure"
fi

window_count=$(grep -c '^GPUI_PDF_READER_QA_WINDOWS windows=2 pdf_views=2 settled=2$' "$log" || true)
[ "$window_count" -eq 1 ] || fail "did not observe two settled PDF workspace windows"
report_count=$(grep -c '^GPUI_PDF_READER_QA ' "$log" || true)
[ "$report_count" -eq 1 ] || fail "expected exactly one reader QA report, got $report_count"
report=$(grep '^GPUI_PDF_READER_QA ' "$log")
case "$report" in
  *"pending=0 "*"debouncing=0 "*"status=Ready") ;;
  *) fail "foreground reader did not reach a quiet Ready state" ;;
esac

printf 'E2E multiwindow: %s\n' "$(grep '^GPUI_PDF_READER_QA_WINDOWS ' "$log")"
