#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-annotation-ownership.XXXXXX")
log="$tmp_dir/multi.log"
portable_log="$tmp_dir/portable.log"
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
  printf 'E2E annotation ownership failed: %s\n' "$1" >&2
  sed -n '1,320p' "$log" >&2 || true
  sed -n '1,180p' "$portable_log" >&2 || true
  exit 1
}

[ "$(uname -s)" = "Darwin" ] || fail "requires macOS"
[ -f "$fixture" ] || fail "missing fixture: $fixture"

paths=""
for index in 1 2 3; do
  path="$tmp_dir/document-$index.pdf"
  cp "$fixture" "$path"
  paths="$paths $path"
done

cd "$root"
cargo build --locked

GPUI_PDF_READER_QA_MULTI_ANNOTATION_SCENARIO=1 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" $paths >"$log" 2>&1 &
app_pid=$!

(
  sleep 70
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

[ ! -f "$timeout_marker" ] || fail "native multi-tab flow exceeded watchdog"
[ "$status" -eq 0 ] || fail "native app exited with status $status"
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  fail "native app reported a QA, panic, or GPU failure"
fi
report=$(grep '^GPUI_PDF_READER_QA_MULTI_ANNOTATIONS ' "$log" || true)
[ "$report" = "GPUI_PDF_READER_QA_MULTI_ANNOTATIONS tabs=3 unique_views=3 unique_paths=3 persisted=3" ] \
  || fail "multi-tab ownership report was incomplete: $report"

for index in 1 2 3; do
  sidecar="$tmp_dir/document-$index.pdf.gpui-pdf-reader.json"
  [ -f "$sidecar" ] || fail "tab $index did not create its own sidecar"
  [ "$(grep -c '"id":' "$sidecar")" -eq 1 ] \
    || fail "tab $index did not persist exactly one owned annotation"
  grep -q 'fluid note' "$sidecar" \
    || fail "tab $index did not persist its owned comment"
  grep -q '"schema_version": 2' "$sidecar" \
    || fail "tab $index did not persist content identity"
done

# Simulate copying/restoring the PDF after its schema 2 sidecar was written.
# The content is unchanged but the filesystem timestamp no longer matches.
portable_pdf="$tmp_dir/document-1.pdf"
touch -t 202001010101 "$portable_pdf"
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$portable_pdf" >"$portable_log" 2>&1
portable_report=$(grep '^GPUI_PDF_READER_QA ' "$portable_log" || true)
case "$portable_report" in
  *"annotations=1 "*"annotation_loading=0 "*"annotation_blocked=0 "*) ;;
  *) fail "content-identical fixture sidecar did not load: $portable_report" ;;
esac

printf 'E2E annotation ownership: %s\n' "$report"
