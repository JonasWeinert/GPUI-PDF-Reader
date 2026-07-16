#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-toc.XXXXXX")
log="$tmp_dir/toc.log"
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
  printf '%s\n' "macos_toc.sh requires macOS" >&2
  exit 1
fi

cd "$root"
cargo build --locked
GPUI_PDF_READER_QA_FLUID_VIEW=1 \
GPUI_PDF_READER_QA_THEME="Catppuccin Latte" \
GPUI_PDF_READER_QA_TOC_HOVER=1 \
GPUI_PDF_READER_QA_TOC_NAVIGATE=2 \
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
  printf '%s\n' "TOC E2E failed to exit cleanly" >&2
  exit 1
fi
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  sed -n '1,260p' "$log" >&2
  printf '%s\n' "TOC E2E logged a panic or rendering failure" >&2
  exit 1
fi

report=$(grep '^GPUI_PDF_READER_QA ' "$log")
if [ "$(printf '%s\n' "$report" | wc -l | tr -d ' ')" -ne 1 ]; then
  printf '%s\n' "TOC E2E did not produce exactly one report" >&2
  exit 1
fi
case "$report" in
  *"view=Fluid "*"toc=4 "*"toc_hover=2 "*"toc_hover_strength=1.000 "*"toc_text_matches=1 "*"pending=0 "*"debouncing=0 "*"status=Ready") ;;
  *)
    printf 'TOC E2E did not settle with the hover card and title-matched navigation: %s\n' "$report" >&2
    exit 1
    ;;
esac
scroll_y=$(printf '%s\n' "$report" | sed -n 's/.*scroll=([^,]*,\([^)]*\)).*/\1/p')
awk -v scroll_y="$scroll_y" 'BEGIN { if (scroll_y <= 100) exit 1 }' || {
  printf 'TOC E2E did not animate away from the first page: %s\n' "$report" >&2
  exit 1
}

printf 'E2E toc: %s\n' "$report"
