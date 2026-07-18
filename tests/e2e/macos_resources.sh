#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
real_first="$root/tests/fixtures/referenceslinkedindocsuperscr.pdf"
real_second="$root/tests/fixtures/referenceslinkedindocsquarebr.pdf"
fallback_first="$root/tests/fixtures/interaction.pdf"
fallback_second="$root/tests/fixtures/scientific-unlinked.pdf"
if [ -f "$real_first" ] && [ -f "$real_second" ]; then
  default_first=$real_first
  default_second=$real_second
else
  default_first=$fallback_first
  default_second=$fallback_second
fi
first=${GPUI_PDF_READER_RESOURCE_FIXTURE_A:-$default_first}
second=${GPUI_PDF_READER_RESOURCE_FIXTURE_B:-$default_second}
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-resources.XXXXXX")
log=${GPUI_PDF_READER_RESOURCE_LOG:-"$tmp_dir/app.log"}
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
  printf 'E2E resources failed: %s\n' "$1" >&2
  sed -n '1,320p' "$log" >&2 || true
  exit 1
}

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_resources.sh requires macOS" >&2
  exit 1
fi
for fixture in "$first" "$second"; do
  [ -f "$fixture" ] || fail "missing fixture: $fixture"
done

cd "$root"
cargo build --locked
: >"$log"

GPUI_PDF_READER_QA_WINDOW_COUNT=2 \
GPUI_PDF_READER_QA_RESOURCE_TRACE=1 \
GPUI_PDF_READER_QA_RESOURCE_SAMPLE_MS=50 \
GPUI_PDF_READER_QA_RESOURCE_STRESS=1 \
GPUI_PDF_READER_QA_INTERVAL_MS=35 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$first" "$second" >"$log" 2>&1 &
app_pid=$!

(
  sleep 60
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

[ ! -f "$timeout_marker" ] || fail "resource stress exceeded the 60 second watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "app log contains a QA, panic, or GPU/Metal failure"
fi

trace_count=$(grep -c '^GPUI_PDF_READER_QA_RESOURCE ' "$log" || true)
[ "$trace_count" -ge 10 ] || fail "expected a resource timeline, got $trace_count samples"
grep -q '^GPUI_PDF_READER_QA_RESOURCE .*operation=timeline phase=sample ' "$log" || \
  fail "periodic resource samples are missing"
settled=$(grep '^GPUI_PDF_READER_QA_RESOURCE .*operation=resource-stress phase=settled ' "$log" | tail -1)
[ -n "$settled" ] || fail "resource stress settled marker is missing"

case "$settled" in *" views=2 "*) ;; *) fail "final trace does not contain two PDF views" ;; esac
case "$settled" in *" interactive=1 "*) ;; *) fail "final trace has no interactive view" ;; esac
case "$settled" in *" warm=1 "*) ;; *) fail "final trace has no warm view" ;; esac
resident=$(printf '%s\n' "$settled" | sed -E 's/.* resident=([0-9]+) .*/\1/')
resident_peak=$(printf '%s\n' "$settled" | sed -E 's/.* resident_peak=([0-9]+) .*/\1/')
alloc_calls=$(printf '%s\n' "$settled" | sed -E 's/.* alloc_calls=([0-9]+) .*/\1/')
alloc_live=$(printf '%s\n' "$settled" | sed -E 's/.* alloc_live=([0-9]+) .*/\1/')
alloc_peak=$(printf '%s\n' "$settled" | sed -E 's/.* alloc_peak=([0-9]+) .*/\1/')
tile_limit=$(printf '%s\n' "$settled" | sed -E 's/.* tile_limit=([0-9]+) .*/\1/')
tile_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* tile_bytes=([0-9]+) .*/\1/')
[ "$resident" -gt 0 ] || fail "process resident memory was not reported"
[ "$alloc_calls" -gt 0 ] || fail "allocator calls were not reported"
[ "$tile_limit" -le 100663296 ] || \
  fail "two-view raw tile limit exceeded 96 MiB: $tile_limit"
[ "$tile_bytes" -le 134217728 ] || \
  fail "protected visible tiles exceeded the 128 MiB stress ceiling: $tile_bytes"

printf 'E2E resources: samples=%s resident=%s resident_peak=%s alloc_live=%s alloc_peak=%s tile_bytes=%s tile_limit=%s fixtures=%s,%s\n' \
  "$trace_count" "$resident" "$resident_peak" "$alloc_live" "$alloc_peak" \
  "$tile_bytes" "$tile_limit" \
  "$(basename "$first")" "$(basename "$second")"
