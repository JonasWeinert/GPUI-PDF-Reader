#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
first="$root/tests/fixtures/interaction.pdf"
second="$root/tests/fixtures/scientific-unlinked.pdf"
binary="${CARGO_TARGET_DIR:-$root/target}/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-tab-drag.XXXXXX")
third="$tmp_dir/interaction-copy.pdf"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

fail() {
  name=$1
  message=$2
  printf 'E2E tab drag (%s) failed: %s\n' "$name" "$message" >&2
  sed -n '1,320p' "$tmp_dir/$name.log" >&2 || true
  exit 1
}

run_case() {
  name=$1
  shift
  log="$tmp_dir/$name.log"
  timeout_marker="$tmp_dir/$name.timeout"
  "$@" >"$log" 2>&1 &
  app_pid=$!
  (
    sleep 55
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
  [ ! -f "$timeout_marker" ] || fail "$name" "exceeded watchdog"
  [ "$status" -eq 0 ] || fail "$name" "app exited with status $status"
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
    fail "$name" "app log contains a QA, panic, or GPU/Metal failure"
  fi
}

[ "$(uname -s)" = "Darwin" ] || {
  printf '%s\n' "macos_tab_drag.sh requires macOS" >&2
  exit 1
}
for fixture in "$first" "$second"; do
  [ -f "$fixture" ] || fail setup "missing fixture: $fixture"
done
cp "$first" "$third"

cd "$root"
cargo build --locked

run_case reorder env \
  GPUI_PDF_READER_QA_TAB_COUNT=3 \
  GPUI_PDF_READER_QA_TAB_DRAG_SCENARIO=reorder \
  GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$first" "$second" "$third"
grep -q '^GPUI_PDF_READER_QA_TAB_DRAG scenario=reorder tabs=3$' "$tmp_dir/reorder.log" \
  || fail reorder "did not preserve all tabs in the expected order"

run_case transfer env \
  GPUI_PDF_READER_QA_WINDOW_COUNT=2 \
  GPUI_PDF_READER_QA_SEPARATE_WINDOWS=1 \
  GPUI_PDF_READER_QA_TAB_DRAG_SCENARIO=transfer \
  GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$first" "$second"
grep -q '^GPUI_PDF_READER_QA_TAB_DRAG scenario=transfer tabs=2 windows=1 settled=1$' "$tmp_dir/transfer.log" \
  || fail transfer "cross-window transfer did not settle in one two-tab window"

printf '%s\n' "E2E tab drag: reorder and cross-window transfer passed"
