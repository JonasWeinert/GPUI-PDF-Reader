#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-extensions.XXXXXX")
reference_data="$tmp_dir/reference-data"
adversarial_data="$tmp_dir/adversarial-data"
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_extensions.sh requires macOS" >&2
  exit 1
fi
if [ ! -f "$fixture" ]; then
  printf 'missing fixture: %s\n' "$fixture" >&2
  exit 1
fi

fail_case() {
  fail_name=$1
  fail_message=$2
  printf 'E2E %s failed: %s\n' "$fail_name" "$fail_message" >&2
  if [ -f "$tmp_dir/$fail_name.log" ]; then
    sed -n '1,300p' "$tmp_dir/$fail_name.log" >&2
  fi
  if [ -f "$tmp_dir/$fail_name.system.log" ]; then
    sed -n '1,160p' "$tmp_dir/$fail_name.system.log" >&2
  fi
  exit 1
}

assert_common_report() {
  assert_name=$1
  assert_report=$2
  case "$assert_report" in
    *"pending=0 "*"debouncing=0 "*"extension_failed=0 "*"status=Ready") ;;
    *) fail_case "$assert_name" "reader or extension host did not reach a quiet Ready state" ;;
  esac
  exact=$(printf '%s\n' "$assert_report" | sed -E 's/.* visible_exact=([^ ]+).*/\1/')
  have=${exact%%/*}
  need=${exact##*/}
  if [ "$have" -le 0 ] || [ "$have" -ne "$need" ]; then
    fail_case "$assert_name" "visible tiles were not all exact: $exact"
  fi
}

assert_scenario_report() {
  assert_name=$1
  assert_scenario=$2
  assert_report=$3
  assert_common_report "$assert_name" "$assert_report"
  case "$assert_scenario" in
    reference)
      case "$assert_report" in
        *"extension_packages=2 "*"extension_active=2 "*"extension_suspended=0 "*"extension_panel=org.key.reference.document-statistics "*"extension_checks=11 "*"extension_native_rejected=0 "*) ;;
        *) fail_case "$assert_name" "reference packages did not exercise the expected live app surfaces" ;;
      esac
      ;;
    manager)
      case "$assert_report" in
        *"extension_packages=1 "*"extension_active=1 "*"extension_suspended=0 "*"extension_panel=none "*"extension_checks=6 "*"extension_native_rejected=0 "*) ;;
        *) fail_case "$assert_name" "extension manager review and settings flow did not complete" ;;
      esac
      ;;
    restore)
      case "$assert_report" in
        *"extension_packages=2 "*"extension_active=2 "*"extension_suspended=0 "*"extension_panel=org.key.reference.document-statistics "*"extension_checks=4 "*) ;;
        *) fail_case "$assert_name" "reviewed packages were not restored through the durable registry" ;;
      esac
      ;;
    adversarial)
      case "$assert_report" in
        *"extension_packages=1 "*"extension_active=0 "*"extension_suspended=1 "*"extension_panel=none "*"extension_checks=8 "*"extension_native_rejected=1 "*) ;;
        *) fail_case "$assert_name" "hostile packages escaped rejection or containment" ;;
      esac
      ;;
  esac
}

run_case() {
  case_name=$1
  scenario=$2
  data_dir=$3
  log="$tmp_dir/$case_name.log"
  timeout_file="$tmp_dir/$case_name.timeout"
  mkdir -p "$data_dir"

  GPUI_PDF_READER_DATA_DIR="$data_dir" \
  GPUI_PDF_READER_QA_EXTENSION_SCENARIO="$scenario" \
  GPUI_PDF_READER_QA_TIMEOUT_MS=45000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
    "$binary" "$fixture" >"$log" 2>&1 &
  app_pid=$!
  case_app_pid=$app_pid

  (
    sleep 75
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

  if [ -f "$timeout_file" ]; then
    fail_case "$case_name" "native app exceeded the 75 second watchdog"
  fi
  if [ "$status" -ne 0 ]; then
    fail_case "$case_name" "native app exited with status $status"
  fi
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$log"; then
    fail_case "$case_name" "app log contains a QA error, panic, or GPU/Metal failure"
  fi

  count=$(grep -c '^GPUI_PDF_READER_QA ' "$log" || true)
  if [ "$count" -ne 1 ]; then
    fail_case "$case_name" "expected exactly one QA report, got $count"
  fi
  report=$(grep '^GPUI_PDF_READER_QA ' "$log")
  assert_scenario_report "$case_name" "$scenario" "$report"

  system_log="$tmp_dir/$case_name.system.log"
  if /usr/bin/log show --last 2m --style compact \
    --predicate "processIdentifier == $case_app_pid" >"$system_log" 2>/dev/null; then
    if grep -Eiq 'InvalidResource|GPU Address Fault|Metal[^:]*invalid|page fault.*GPU' "$system_log"; then
      fail_case "$case_name" "macOS log contains a GPU/Metal failure"
    fi
  fi
  printf 'E2E %s: %s\n' "$case_name" "$report"
}

cd "$root"
cargo build --locked

run_case install_and_use reference "$reference_data"
run_case review_and_configure manager "$tmp_dir/manager-data"
run_case restore_and_use restore "$reference_data"
run_case contain_hostile adversarial "$adversarial_data"
