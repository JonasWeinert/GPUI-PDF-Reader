# Resource tracing

The debug app has opt-in resource tracing for native QA. Release builds do not include it.

Run the two-window stress case:

```sh
tests/e2e/macos_resources.sh
```

When the larger real-world reference fixtures are present, the script uses them automatically. Override either input with:

```sh
GPUI_PDF_READER_RESOURCE_FIXTURE_A=/path/to/first.pdf \
GPUI_PDF_READER_RESOURCE_FIXTURE_B=/path/to/second.pdf \
tests/e2e/macos_resources.sh
```

Keep the complete timeline for comparison:

```sh
GPUI_PDF_READER_RESOURCE_LOG=/tmp/gpui-resources.log \
tests/e2e/macos_resources.sh
```

The underlying flags are `GPUI_PDF_READER_QA_RESOURCE_TRACE=1` and optional `GPUI_PDF_READER_QA_RESOURCE_SAMPLE_MS=100`. `GPUI_PDF_READER_QA_RESOURCE_STRESS=1` drives zoom work in every open PDF window.

Each `GPUI_PDF_READER_QA_RESOURCE` line contains a monotonic timestamp, operation marker, process resident/peak/virtual memory, Rust allocator calls and byte totals, system capacity, activity states, allocations, tile working sets, pending tiles, and text pages.

`tile_resident_estimate` charges raw BGRA plus one GPU copy. It does not measure Metal atlas fragmentation or PDFium-internal allocations. Process resident memory is the authoritative whole-process measurement; allocator counters cover Rust's global allocator only.
