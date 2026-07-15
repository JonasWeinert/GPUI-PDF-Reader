#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-features.XXXXXX")
working_pdf="$tmp_dir/feature-interaction.pdf"
sidecar="$working_pdf.gpui-pdf-reader.json"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_features.sh requires macOS" >&2
  exit 1
fi
if [ ! -f "$fixture" ]; then
  printf 'missing fixture: %s\n' "$fixture" >&2
  exit 1
fi

cp "$fixture" "$working_pdf"

fail_case() {
  fail_name=$1
  fail_message=$2
  printf 'E2E %s failed: %s\n' "$fail_name" "$fail_message" >&2
  if [ -f "$tmp_dir/$fail_name.log" ]; then
    sed -n '1,260p' "$tmp_dir/$fail_name.log" >&2
  fi
  if [ -f "$tmp_dir/$fail_name.system.log" ]; then
    sed -n '1,140p' "$tmp_dir/$fail_name.system.log" >&2
  fi
  exit 1
}

report_value() {
  report_line=$1
  report_key=$2
  printf '%s\n' "$report_line" | sed -E "s/.* ${report_key}=([^ ]+).*/\\1/"
}

assert_common_report() {
  assert_name=$1
  assert_report=$2
  case "$assert_report" in
    *"pending=0 "*"debouncing=0 "*"annotation_loading=0 "*"annotation_blocked=0 "*"status=Ready") ;;
    *) fail_case "$assert_name" "reader did not reach a quiet, writable Ready state" ;;
  esac

  assert_exact=$(report_value "$assert_report" visible_exact)
  assert_have=${assert_exact%%/*}
  assert_need=${assert_exact##*/}
  if [ "$assert_have" -le 0 ] || [ "$assert_have" -ne "$assert_need" ]; then
    fail_case "$assert_name" "visible tiles were not all exact: $assert_exact"
  fi
}

assert_feature_report() {
  assert_name=$1
  assert_report=$2
  assert_common_report "$assert_name" "$assert_report"
  case "$assert_report" in
    *"sidebar=1.000/1 "*"annotations=6 "*"highlights=5 "*"highlight_colors=5 "*"comments=1 "*"annotation_revision=6/6/6 "*"search_pages=3 "*"active_search=2 "*"search_complete=1 "*) ;;
    *) fail_case "$assert_name" "feature counts or final navigation state were unexpected" ;;
  esac

  assert_results=$(report_value "$assert_report" search_results)
  assert_runs=$(report_value "$assert_report" search_highlight_runs)
  assert_transitions=$(report_value "$assert_report" sidebar_transitions)
  assert_anchor_error=$(report_value "$assert_report" sidebar_anchor_error)
  if [ "$assert_results" -lt 3 ] || [ "$assert_runs" -le 0 ]; then
    fail_case "$assert_name" "search did not return bounded highlight geometry"
  fi
  if [ "$assert_transitions" -lt 4 ]; then
    fail_case "$assert_name" "expected anchor-checked Comments and Search transitions"
  fi
  if ! awk -v error="$assert_anchor_error" 'BEGIN { exit !(error <= 0.002) }'; then
    fail_case "$assert_name" "sidebar changed the document anchor by $assert_anchor_error"
  fi
}

assert_reload_report() {
  assert_name=$1
  assert_report=$2
  assert_common_report "$assert_name" "$assert_report"
  case "$assert_report" in
    *"sidebar=0.000/0 "*"annotations=6 "*"highlights=5 "*"highlight_colors=5 "*"comments=1 "*"annotation_revision=6/6/6 "*"search_results=0 "*) ;;
    *) fail_case "$assert_name" "persisted annotations did not reload exactly" ;;
  esac
}

run_case() {
  case_name=$1
  case_mode=$2
  case_log="$tmp_dir/$case_name.log"
  case_timeout="$tmp_dir/$case_name.timeout"

  if [ "$case_mode" = "features" ]; then
    GPUI_PDF_READER_QA_FEATURE_SCENARIO=1 \
    GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
    GPUI_PDF_READER_QA_REPORT=1 \
    GPUI_PDF_READER_QA_EXIT=1 \
      "$binary" "$working_pdf" >"$case_log" 2>&1 &
  else
    GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
    GPUI_PDF_READER_QA_REPORT=1 \
    GPUI_PDF_READER_QA_EXIT=1 \
      "$binary" "$working_pdf" >"$case_log" 2>&1 &
  fi
  app_pid=$!
  case_app_pid=$app_pid

  (
    sleep 60
    : >"$case_timeout"
    kill "$case_app_pid" 2>/dev/null || true
    sleep 2
    kill -9 "$case_app_pid" 2>/dev/null || true
  ) &
  watchdog_pid=$!

  set +e
  wait "$case_app_pid"
  case_status=$?
  set -e
  app_pid=""
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true

  if [ -f "$case_timeout" ]; then
    fail_case "$case_name" "native app exceeded the 60 second watchdog"
  fi
  if [ "$case_status" -ne 0 ]; then
    fail_case "$case_name" "native app exited with status $case_status"
  fi
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$case_log"; then
    fail_case "$case_name" "app log contains a QA error, panic, or GPU/Metal failure"
  fi

  case_qa_count=$(grep -c '^GPUI_PDF_READER_QA ' "$case_log" || true)
  if [ "$case_qa_count" -ne 1 ]; then
    fail_case "$case_name" "expected exactly one QA report, got $case_qa_count"
  fi
  case_report=$(grep '^GPUI_PDF_READER_QA ' "$case_log")
  if [ "$case_mode" = "features" ]; then
    assert_feature_report "$case_name" "$case_report"
  else
    assert_reload_report "$case_name" "$case_report"
  fi

  case_system_log="$tmp_dir/$case_name.system.log"
  if /usr/bin/log show --last 2m --style compact \
    --predicate "processIdentifier == $case_app_pid" >"$case_system_log" 2>/dev/null; then
    if grep -Eiq 'InvalidResource|GPU Address Fault|Metal[^:]*invalid|page fault.*GPU' "$case_system_log"; then
      fail_case "$case_name" "macOS log contains a GPU/Metal failure"
    fi
  fi
  printf 'E2E %s: %s\n' "$case_name" "$case_report"
}

cd "$root"
cargo build --locked

run_case create_and_search features

if [ ! -f "$sidecar" ]; then
  fail_case create_and_search "annotation sidecar was not created"
fi
for color in yellow green blue pink purple; do
  if ! grep -q "\"highlight\": \"$color\"" "$sidecar"; then
    fail_case create_and_search "sidecar is missing the $color highlight"
  fi
done
if [ "$(grep -c '"id":' "$sidecar")" -ne 6 ]; then
  fail_case create_and_search "sidecar does not contain exactly six annotations"
fi
if ! grep -q '\*\*important copy check\*\*' "$sidecar"; then
  fail_case create_and_search "sidecar is missing the native-input formatted Markdown comment"
fi

run_case reload_sidecar reload
