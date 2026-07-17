#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/scientific-unlinked.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-scientific.XXXXXX")
log="$tmp_dir/scientific.log"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_scientific.sh requires macOS" >&2
  exit 1
fi

cd "$root"
cargo build --locked
GPUI_PDF_READER_QA_LINK_HOVER=0 \
GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
GPUI_PDF_READER_QA_REPORT=1 \
GPUI_PDF_READER_QA_EXIT=1 \
  "$binary" "$fixture" >"$log" 2>&1 &
app_pid=$!

set +e
wait "$app_pid"
status=$?
set -e
app_pid=""

if [ "$status" -ne 0 ]; then
  sed -n '1,260p' "$log" >&2
  printf '%s\n' "Scientific-document E2E failed to exit cleanly" >&2
  exit 1
fi
if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
  sed -n '1,260p' "$log" >&2
  printf '%s\n' "Scientific-document E2E logged a panic or rendering failure" >&2
  exit 1
fi

report=$(grep '^GPUI_PDF_READER_QA ' "$log")
if [ "$(printf '%s\n' "$report" | wc -l | tr -d ' ')" -ne 1 ]; then
  printf '%s\n' "Scientific-document E2E did not produce exactly one report" >&2
  exit 1
fi
case "$report" in
  *"links=5 "*"link_preview=1 "*"reference_preview=0 "*"reference_group=3 "*"scholarly=failed "*"scientific=1/1 "*"references=8 "*"dois=2 "*"bracket_citations=1 "*"superscript_citations=4 "*"pending=0 "*"debouncing=0 "*"status=Ready") ;;
  *)
    printf 'Scientific-document E2E did not infer its references: %s\n' "$report" >&2
    exit 1
    ;;
esac

printf 'E2E scientific references: %s\n' "$report"
