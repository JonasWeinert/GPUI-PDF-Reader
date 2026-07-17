#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-link-preview.XXXXXX")
log="$tmp_dir/link-preview.log"
timeout_file="$tmp_dir/timeout"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_link_previews.sh requires macOS" >&2
  exit 1
fi

cd "$root"
cargo build --locked
GPUI_PDF_READER_QA_LINK_HOVER=1 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$fixture" >"$log" 2>&1 &
app_pid=$!
case_app_pid=$app_pid

(
  sleep 60
  : >"$timeout_file"
  kill "$case_app_pid" 2>/dev/null || true
  sleep 2
  kill -9 "$case_app_pid" 2>/dev/null || true
) &
watchdog_pid=$!

set +e
wait "$case_app_pid"
status=$?
set -e
app_pid=""
kill "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true

if [ -f "$timeout_file" ] || [ "$status" -ne 0 ]; then
  sed -n '1,260p' "$log" >&2
  printf '%s\n' "Link preview E2E failed to exit cleanly" >&2
  exit 1
fi
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  sed -n '1,260p' "$log" >&2
  printf '%s\n' "Link preview E2E logged a panic or rendering failure" >&2
  exit 1
fi

report=$(grep '^GPUI_PDF_READER_QA ' "$log")
if [ "$(printf '%s\n' "$report" | wc -l | tr -d ' ')" -ne 1 ]; then
  printf '%s\n' "Link preview E2E did not produce exactly one report" >&2
  exit 1
fi
case "$report" in
  *"links=2 "*"link_navigations=0 "*"link_preview=2 "*"link_preview_state=internal-matched "*"pending=0 "*"debouncing=0 "*"status=Ready") ;;
  *)
    printf 'Link preview E2E did not settle with destination text: %s\n' "$report" >&2
    exit 1
    ;;
esac

printf 'E2E link preview: %s\n' "$report"
