#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="${CARGO_TARGET_DIR:-$root/target}/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-control-bar.XXXXXX")
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
  printf 'E2E control bar failed: %s\n' "$1" >&2
  sed -n '1,300p' "$log" >&2 || true
  exit 1
}

[ "$(uname -s)" = "Darwin" ] || fail "requires macOS"
[ -f "$fixture" ] || fail "missing fixture: $fixture"

cd "$root"
cargo build --locked

GPUI_PDF_READER_QA_CONTROL_BAR_SCENARIO=1 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$fixture" >"$log" 2>&1 &
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

[ ! -f "$timeout_marker" ] || fail "native flow exceeded the 50 second watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "app log contains a QA, panic, or GPU/Metal failure"
fi

report=$(grep '^GPUI_PDF_READER_QA_CONTROL_BAR ' "$log" || true)
[ -n "$report" ] || fail "typed control-bar report is missing"
case "$report" in
  *"results="*" reset=1") ;;
  *) fail "control-bar lifecycle report was incomplete: $report" ;;
esac
expanded=$(printf '%s\n' "$report" | sed -E 's/.* expanded_height=([0-9.]+).*/\1/')
collapsed=$(printf '%s\n' "$report" | sed -E 's/.* collapsed_height=([0-9.]+).*/\1/')
if ! awk -v expanded="$expanded" -v collapsed="$collapsed" 'BEGIN { exit !(expanded >= 137.9 && expanded <= 138.1 && collapsed == 44.0) }'; then
  fail "animated heights were outside their exact settled bounds: expanded=$expanded collapsed=$collapsed"
fi

printf 'E2E control bar: %s\n' "$report"
