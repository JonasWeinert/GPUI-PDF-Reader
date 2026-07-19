#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-fluid.XXXXXX")
working_pdf="$tmp_dir/fluid-interaction.pdf"
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
  printf '%s\n' "macos_fluid.sh requires macOS" >&2
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
    sed -n '1,280p' "$tmp_dir/$fail_name.log" >&2
  fi
  exit 1
}

report_value() {
  report_line=$1
  report_key=$2
  printf '%s\n' "$report_line" | sed -E "s/.* ${report_key}=([^ ]+).*/\\1/"
}

run_case() {
  case_name=$1
  case_mode=$2
  case_log="$tmp_dir/$case_name.log"
  case_timeout="$tmp_dir/$case_name.timeout"

  if [ "$case_mode" = "scenario" ]; then
    GPUI_PDF_READER_QA_FLUID_SCENARIO=1 \
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
  case "$case_report" in
    *"view=Fluid "*"pending=0 "*"debouncing=0 "*"comment_editor=0 "*"comment_dirty=0 "*"autosave_pending=0 "*"annotations=1 "*"highlights=1 "*"highlight_colors=1 "*"comments=1 "*"annotation_revision=2/2/2 "*"annotation_loading=0 "*"annotation_blocked=0 "*"status=Ready") ;;
    *) fail_case "$case_name" "Fluid state was not quiet, persisted, and internally consistent" ;;
  esac

  exact=$(report_value "$case_report" visible_exact)
  have=${exact%%/*}
  need=${exact##*/}
  if [ "$have" -le 0 ] || [ "$have" -ne "$need" ]; then
    fail_case "$case_name" "visible tiles were not all exact: $exact"
  fi

  if [ "$case_mode" = "scenario" ]; then
    case "$case_report" in
      *"sidebar=1.000/1 "*"comment_pane=0.000/0 "*"search_complete=1 "*) ;;
      *) fail_case "$case_name" "Fluid panel or comment slide did not reach its final state" ;;
    esac
    results=$(report_value "$case_report" search_results)
    if [ "$results" -lt 2 ]; then
      fail_case "$case_name" "Fluid search returned only $results result(s)"
    fi
  else
    case "$case_report" in
      *"sidebar=0.000/0 "*"search_results=0 "*) ;;
      *) fail_case "$case_name" "Fluid sidecar reload had unexpected transient UI state" ;;
    esac
  fi

  printf 'E2E %s: %s\n' "$case_name" "$case_report"
}

cd "$root"
cargo build --locked
run_case fluid_interactions scenario

if [ ! -f "$sidecar" ]; then
  fail_case fluid_interactions "annotation sidecar was not created"
fi
if [ "$(grep -c '"id":' "$sidecar")" -ne 1 ]; then
  fail_case fluid_interactions "sidecar does not contain exactly one annotation"
fi
if ! grep -q '"highlight": "yellow"' "$sidecar"; then
  fail_case fluid_interactions "sidecar is missing the clicked yellow highlight"
fi
if ! grep -q 'fluid note' "$sidecar"; then
  fail_case fluid_interactions "sidecar is missing the auto-saved native-input comment"
fi

run_case fluid_reload reload
