# Build and investigation history

- The first scaffold established a macOS-only Rust/GPUI app, a dedicated PDFium worker, AppKit pinch monitoring, and a permissive-only dependency policy.
- A generated three-page fixture then exposed rotation, CropBox, channel order, Unicode extraction, selection, and GPUI runtime-only failures.
- Early review removed per-frame O(page-count) layout work, bounded bitmap/text caches, made document opening atomic, and made viewport demand latest-wins.
- Rapid zoom initially looked like a debounce or PDFium race. Sampling showed the main thread blocked in GPUI Blade's GPU wait while PDFium was idle.
- macOS logs reported `InvalidResource`. The real bug was immediate texture destruction while an unretained Metal command buffer could still sample it.
- Retiring images across two frame callbacks fixed the freeze. The 150ms debounce remains useful for work reduction but is not the safety fix.
- Whole-page rendering was replaced with bounded 1024px tile cores, 32px bleed, a one-bitmap result channel, and protected visible-cache entries.
- Rotation, CropBox, annotation, and form investigations established the shared negative-origin tile mapping and the need to pass matching geometry to `FPDF_FFLDraw`.
- Scheduler review added immediate burst cancellation, stable complete viewport signatures, visible-first priority, and stale success and failure rejection.
- Text review added fixed-precision bounds, cancellable bounded walks, a 16×16 spatial index, resumable copy, O(1) Select All, and cache/clipboard limits.
- Final work added exact multi-page zoom stress, log inspection, a focused tiled-versus-full regression, target-active license auditing, and concise documentation.

# Subagent findings reviewed

- `gpui_research` found the missing published pinch API and the AppKit coordinate/lifetime constraints.
- `pdfium_research` checked native rendering, text, rotation, and library-loading behavior.
- `license_arch`, `license_audit`, `license_reaudit`, and `final_license_audit` drove target-specific graph, native binary, notice, and fixture audits.
- `code_review` found correctness, scheduling, cache, and large-document performance gaps in the first implementation.
- `zoom_crash_audit` separated PDFium work from the GPU wait and helped identify the multi-page trigger.
- `metal_header` explored an alternate renderer build path; it was stopped after texture lifetime, not the renderer choice, proved to be the cause.
- `tile_feasibility` and `tile_ui_design` shaped the bounded PDFium tile API, viewport planning, bleed, caching, and fallback behavior.
- `zoom_regression_tests` defined the rapid min/max and debounce-crossing stress sequences.
- `rotation_mapping` verified the same tile-origin rule for intrinsic 0/90/180/270 rotation and the need for larger bleed.
- `text_perf_design` designed shared O(1) layout rescaling and bounded spatial text queries.
- `tile_code_review` repeatedly audited the finished worker state machine, cancellation, bounds, cache rules, and error handling.
- `docs_audit` reconciled the README and current limits with the implementation.
- `test_audit`, `e2e_design`, and `notes_history` identified the final saved-test gap and distilled these durable notes.
