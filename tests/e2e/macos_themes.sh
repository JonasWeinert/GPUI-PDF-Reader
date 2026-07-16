#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture="$root/tests/fixtures/interaction.pdf"
binary="$root/target/debug/gpui-pdf-reader"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/gpui-pdf-reader-themes.XXXXXX")
app_pid=""

cleanup() {
  if [ -n "$app_pid" ]; then
    kill "$app_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' "macos_themes.sh requires macOS" >&2
  exit 1
fi

run_case() {
  case_name=$1
  case_theme=$2
  case_pdf_render=${3:-}
  case_pdf_dark=${4:-on}
  case_pdf_dark_report=1
  if [ "$case_pdf_dark" = "off" ]; then
    case_pdf_dark_report=0
  fi
  case_log="$tmp_dir/$case_name.log"
  case_timeout="$tmp_dir/$case_name.timeout"

  GPUI_PDF_READER_QA_THEME="$case_theme" \
  GPUI_PDF_READER_QA_PDF_DARK="$case_pdf_dark" \
  GPUI_PDF_READER_QA_TIMEOUT_MS=30000 \
  GPUI_PDF_READER_QA_REPORT=1 \
  GPUI_PDF_READER_QA_EXIT=1 \
    "$binary" "$fixture" >"$case_log" 2>&1 &
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

  if [ -f "$case_timeout" ] || [ "$case_status" -ne 0 ]; then
    sed -n '1,260p' "$case_log" >&2
    printf 'E2E %s failed to exit cleanly\n' "$case_name" >&2
    exit 1
  fi
  if grep -Eiq 'GPUI_PDF_READER_QA_ERROR|thread .* panicked|panicked at|InvalidResource|GPU Address Fault|Metal.*invalid' "$case_log"; then
    sed -n '1,260p' "$case_log" >&2
    printf 'E2E %s logged a panic or rendering failure\n' "$case_name" >&2
    exit 1
  fi
  report=$(grep '^GPUI_PDF_READER_QA ' "$case_log")
  if [ "$(printf '%s\n' "$report" | wc -l | tr -d ' ')" -ne 1 ]; then
    printf 'E2E %s did not produce exactly one report\n' "$case_name" >&2
    exit 1
  fi
  case "$report" in
    *"theme=$case_theme "*"pending=0 "*"debouncing=0 "*"status=Ready") ;;
    *)
      printf 'E2E %s theme was not applied in a settled reader: %s\n' "$case_name" "$report" >&2
      exit 1
      ;;
  esac
  if [ -n "$case_pdf_render" ]; then
    case "$report" in
      *"pdf_render=$case_pdf_render "*) ;;
      *)
        printf 'E2E %s used the wrong PDF render appearance: %s\n' "$case_name" "$report" >&2
        exit 1
        ;;
    esac
  fi
  case "$report" in
    *"pdf_dark_enabled=$case_pdf_dark_report "*) ;;
    *)
      printf 'E2E %s reported the wrong PDF dark-mode preference: %s\n' "$case_name" "$report" >&2
      exit 1
      ;;
  esac
  printf 'E2E %s: %s\n' "$case_name" "$report"
}

cd "$root"
cargo build --locked
run_case theme_system system
run_case theme_light "Catppuccin Latte" normal
run_case theme_dark "Tokyo Night" forced
run_case theme_dark_pdf_light "Tokyo Night" normal off
