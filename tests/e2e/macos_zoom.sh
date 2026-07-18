#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
binary="$target_dir/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-e2e.XXXXXX")
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_zoom.sh requires macOS" >&2
  exit 1
fi
if [ ! -f "$fixture" ]; then
  printf '%s\n' "missing fixture: $fixture" >&2
  exit 1
fi

repeat_token() {
  repeat_count=$1
  repeat_value=$2
  repeat_result=""
  repeat_index=0
  while [ "$repeat_index" -lt "$repeat_count" ]; do
    repeat_result="${repeat_result}${repeat_result:+ }${repeat_value}"
    repeat_index=$((repeat_index + 1))
  done
  printf '%s' "$repeat_result"
}

fail_case() {
  fail_name=$1
  fail_message=$2
  printf 'E2E %s failed: %s\n' "$fail_name" "$fail_message" >&2
  if [ -f "$tmp_dir/$fail_name.log" ]; then
    sed -n '1,240p' "$tmp_dir/$fail_name.log" >&2
  fi
  if [ -f "$tmp_dir/$fail_name.system.log" ]; then
    sed -n '1,120p' "$tmp_dir/$fail_name.system.log" >&2
  fi
  exit 1
}

assert_report() {
  assert_name=$1
  assert_report_line=$2
  assert_zoom=$3
  assert_min_pages=$4

  case "$assert_report_line" in
    *"zoom=$assert_zoom "*) ;;
    *) fail_case "$assert_name" "expected zoom=$assert_zoom" ;;
  esac
  case "$assert_report_line" in
    *"pending=0 "*"debouncing=0 "*"status=Ready") ;;
    *) fail_case "$assert_name" "reader did not reach a quiet Ready state" ;;
  esac

  assert_exact=$(printf '%s\n' "$assert_report_line" | sed -E 's/.* visible_exact=([0-9]+)\/([0-9]+) .*/\1 \2/')
  assert_have=${assert_exact%% *}
  assert_need=${assert_exact##* }
  if [ "$assert_have" -le 0 ] || [ "$assert_have" -ne "$assert_need" ]; then
    fail_case "$assert_name" "visible tiles were not all exact: $assert_have/$assert_need"
  fi

  assert_pages=$(printf '%s\n' "$assert_report_line" | sed -E 's/.* visible_pages=([0-9]+) .*/\1/')
  if [ "$assert_pages" -lt "$assert_min_pages" ]; then
    fail_case "$assert_name" "expected at least $assert_min_pages visible pages, got $assert_pages"
  fi

  assert_tile_bytes=$(printf '%s\n' "$assert_report_line" | sed -E 's/.* max_tile_bytes=([0-9]+) .*/\1/')
  if [ "$assert_tile_bytes" -gt 4734976 ]; then
    fail_case "$assert_name" "tile exceeded the 1088x1088 BGRA bound: $assert_tile_bytes bytes"
  fi
}

run_case() {
  case_name=$1
  case_kind=$2
  case_sequence=$3
  case_interval=$4
  case_zoom=$5
  case_min_pages=$6
  case_log="$tmp_dir/$case_name.log"
  case_timeout="$tmp_dir/$case_name.timeout"
  case_keys=""
  case_wheels=""
  if [ "$case_kind" = "keys" ]; then
    case_keys=$case_sequence
  else
    case_wheels=$case_sequence
  fi

  GPUI_PDF_READER_QA_KEYS="$case_keys" \
  GPUI_PDF_READER_QA_WHEEL_DELTAS="$case_wheels" \
  GPUI_PDF_READER_QA_INTERVAL_MS="$case_interval" \
  GPUI_PDF_READER_QA_TIMEOUT_MS=20000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
    "$binary" "$fixture" >"$case_log" 2>&1 &
  app_pid=$!
  case_app_pid=$app_pid

  (
    sleep 45
    : >"$case_timeout"
    kill "$app_pid" 2>/dev/null || true
    sleep 2
    kill -9 "$app_pid" 2>/dev/null || true
  ) &
  watchdog_pid=$!

  set +e
  wait "$app_pid"
  case_status=$?
  set -e
  app_pid=""
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true

  if [ -f "$case_timeout" ]; then
    fail_case "$case_name" "native app exceeded the 45 second watchdog"
  fi
  if [ "$case_status" -ne 0 ]; then
    fail_case "$case_name" "native app exited with status $case_status"
  fi
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$case_log"; then
    fail_case "$case_name" "app log contains a panic or GPU/Metal failure"
  fi

  case_qa_count=$(grep -c '^GPUI_PDF_READER_QA ' "$case_log" || true)
  if [ "$case_qa_count" -ne 1 ]; then
    fail_case "$case_name" "expected exactly one QA report, got $case_qa_count"
  fi
  case_report=$(grep '^GPUI_PDF_READER_QA ' "$case_log")
  assert_report "$case_name" "$case_report" "$case_zoom" "$case_min_pages"

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

wheel_min_to_max="$(repeat_token 12 -80) $(repeat_token 24 80)"
wheel_max_to_min="$(repeat_token 24 80) $(repeat_token 24 -80)"
key_churn=$(repeat_token 20 cmd--)
churn_round=0
while [ "$churn_round" -lt 3 ]; do
  key_churn="$key_churn $(repeat_token 3 cmd-=) $(repeat_token 3 cmd--)"
  churn_round=$((churn_round + 1))
done

run_case wheel_rapid_min_to_max wheel "$wheel_min_to_max" 20 5.000 1
run_case wheel_rapid_max_to_min wheel "$wheel_max_to_min" 20 0.200 2
run_case keys_debounce_churn keys "$key_churn" 180 0.200 2
