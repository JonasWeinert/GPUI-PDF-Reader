#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture_dir=${GPUI_PDF_READER_SCALE_FIXTURE_DIR:-$root}
document_count=${GPUI_PDF_READER_SCALE_DOCUMENTS:-20}
retention_modes=${GPUI_PDF_READER_SCALE_RETENTION_MODES:-"100 50 10"}
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
output_dir=${GPUI_PDF_READER_SCALE_OUTPUT_DIR:-"$target_dir/qa-resource-scale"}
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-scale.XXXXXX")
fixture_list="$tmp_dir/fixtures.txt"
app_pid=""
caffeine_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  if [ -n "$caffeine_pid" ]; then
    kill "$caffeine_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

fail() {
  printf 'E2E resource scale failed: %s\n' "$1" >&2
  if [ -n "${log:-}" ] && [ -f "$log" ]; then
    sed -n '1,360p' "$log" >&2 || true
  fi
  exit 1
}

[ "$(uname -s)" = Darwin ] || fail "macos_resource_scale.sh requires macOS"
case "$document_count" in
  ''|*[!0-9]*) fail "GPUI_PDF_READER_SCALE_DOCUMENTS must be a positive integer" ;;
esac
[ "$document_count" -gt 0 ] || fail "document count must be positive"

find "$fixture_dir" -maxdepth 1 -type f -name 'PUBMED_*.pdf' -print \
  | LC_ALL=C sort \
  | head -n "$document_count" >"$fixture_list"
fixture_count=$(wc -l <"$fixture_list" | tr -d ' ')
[ "$fixture_count" -eq "$document_count" ] || \
  fail "expected $document_count real PUBMED fixtures in $fixture_dir, found $fixture_count"

mkdir -p "$output_dir"
cd "$root"
cargo build --locked

printf 'Resource scale corpus: documents=%s fixture_dir=%s\n' "$document_count" "$fixture_dir"
printf 'retention,initial_footprint_mib,peak_footprint_mib,settled_footprint_mib,settled_delta_mib,peak_rss_mib,settled_rss_mib,tile_mib,hibernated_views,samples\n'

for retention in $retention_modes; do
  case "$retention" in
    ''|*[!0-9]*) fail "retention values must be integers" ;;
  esac
  [ "$retention" -ge 1 ] && [ "$retention" -le 100 ] || \
    fail "retention must be between 1 and 100"

  log="$output_dir/retention-$retention.log"
  timeout_marker="$tmp_dir/timeout-$retention"
  set -- "$binary"
  while IFS= read -r fixture; do
    set -- "$@" "$fixture"
  done <"$fixture_list"

  : >"$log"
  GPUI_PDF_READER_QA_PROGRESSIVE_OPEN=1 \
  GPUI_PDF_READER_QA_WINDOW_COUNT="$document_count" \
  GPUI_PDF_READER_QA_RESOURCE_TRACE=1 \
  GPUI_PDF_READER_QA_RESOURCE_SAMPLE_MS=50 \
  GPUI_PDF_READER_QA_RESOURCE_CHAOS=1 \
  GPUI_PDF_READER_QA_CHAOS_ROUNDS=${GPUI_PDF_READER_QA_CHAOS_ROUNDS:-1} \
  GPUI_PDF_READER_QA_CHAOS_ACTIONS_PER_WINDOW=${GPUI_PDF_READER_QA_CHAOS_ACTIONS_PER_WINDOW:-4} \
  GPUI_PDF_READER_QA_CHAOS_SEED=${GPUI_PDF_READER_QA_CHAOS_SEED:-7737383632611182113} \
  GPUI_PDF_READER_QA_RENDER_DWELL_MS=${GPUI_PDF_READER_QA_RENDER_DWELL_MS:-320} \
  GPUI_PDF_READER_QA_CACHE_RETENTION_PERCENT="$retention" \
  GPUI_PDF_READER_QA_CACHE_TRIM_MS=${GPUI_PDF_READER_QA_CACHE_TRIM_MS:-900} \
  GPUI_PDF_READER_QA_TIMEOUT_MS=45000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
    "$@" >"$log" 2>&1 &
  app_pid=$!
  /usr/bin/caffeinate -dimsu -w "$app_pid" &
  caffeine_pid=$!

  (
    sleep ${GPUI_PDF_READER_SCALE_WATCHDOG_SECONDS:-900}
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
  kill "$caffeine_pid" 2>/dev/null || true
  wait "$caffeine_pid" 2>/dev/null || true
  caffeine_pid=""
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true

  [ ! -f "$timeout_marker" ] || fail "retention $retention exceeded the watchdog"
  [ "$status" -eq 0 ] || fail "retention $retention exited with status $status"
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
    fail "retention $retention log contains a QA, panic, or GPU/Metal failure"
  fi

  initial=$(grep '^GPUI_PDF_READER_QA_RESOURCE .*operation=progressive-open-1 phase=settled ' "$log" | tail -1)
  settled=$(grep '^GPUI_PDF_READER_QA_RESOURCE .*operation=resource-chaos phase=settled ' "$log" | tail -1)
  [ -n "$initial" ] || fail "retention $retention has no initial checkpoint"
  [ -n "$settled" ] || fail "retention $retention has no settled checkpoint"
  open_checkpoints=$(grep -c '^GPUI_PDF_READER_QA_RESOURCE .*operation=progressive-open-[0-9][0-9]* phase=settled ' "$log" || true)
  [ "$open_checkpoints" -eq "$document_count" ] || \
    fail "retention $retention recorded $open_checkpoints/$document_count open checkpoints"

  initial_bytes=$(printf '%s\n' "$initial" | sed -E 's/.* footprint=([0-9]+) .*/\1/')
  peak_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* footprint_peak=([0-9]+) .*/\1/')
  settled_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* footprint=([0-9]+) .*/\1/')
  peak_rss_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* resident_peak=([0-9]+) .*/\1/')
  settled_rss_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* resident=([0-9]+) .*/\1/')
  tile_bytes=$(printf '%s\n' "$settled" | sed -E 's/.* tile_bytes=([0-9]+) .*/\1/')
  hibernated=$(printf '%s\n' "$settled" | sed -E 's/.* hibernated_views=([0-9]+) .*/\1/')
  samples=$(grep -c '^GPUI_PDF_READER_QA_RESOURCE ' "$log" || true)
  minimum_hibernated=$((document_count > 4 ? document_count - 4 : 0))
  [ "$hibernated" -ge "$minimum_hibernated" ] || \
    fail "retention $retention hibernated only $hibernated/$minimum_hibernated expected views"

  initial_mib=$(awk -v bytes="$initial_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  peak_mib=$(awk -v bytes="$peak_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  settled_mib=$(awk -v bytes="$settled_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  delta_mib=$(awk -v end="$settled_bytes" -v start="$initial_bytes" 'BEGIN { printf "%.1f", (end - start) / 1048576 }')
  peak_rss_mib=$(awk -v bytes="$peak_rss_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  settled_rss_mib=$(awk -v bytes="$settled_rss_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  tile_mib=$(awk -v bytes="$tile_bytes" 'BEGIN { printf "%.1f", bytes / 1048576 }')
  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
    "$retention" "$initial_mib" "$peak_mib" "$settled_mib" "$delta_mib" \
    "$peak_rss_mib" "$settled_rss_mib" "$tile_mib" "$hibernated" "$samples"

  grep '^GPUI_PDF_READER_QA_RESOURCE .*operation=progressive-open-[0-9][0-9]* phase=settled ' "$log" \
    | sed -E 's/.*operation=progressive-open-([0-9]+).* resident=([0-9]+).* tile_bytes=([0-9]+).* hibernated_views=([0-9]+).*/checkpoint retention='"$retention"' documents=\1 resident_bytes=\2 tile_bytes=\3 hibernated=\4/' \
    >"$output_dir/retention-$retention-checkpoints.txt"
done

printf 'Full traces and per-open checkpoints: %s\n' "$output_dir"
