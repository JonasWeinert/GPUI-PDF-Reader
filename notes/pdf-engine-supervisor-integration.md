# PDF engine supervisor integration

`key-pdf-runtime::start_engine_supervisor` is the migration seam for one
process-wide PDFium owner. It is engine independent and tested with an engine
and documents that are deliberately not `Send`.

Guarantees:

- The factory, engine, and every engine document stay on one owner thread.
- Each attached document has an independent ID, session generation, work
  queue, cancellation domains, and bounded event route.
- Work rotates fairly across runnable documents. Within a document, higher
  priority and newer equal-priority demands run first.
- Replacing a viewport, preview, or text domain cancels in-flight work before
  queue cleanup reaches the owner thread.
- A full event channel pauses only its document. It cannot block another open
  window or accumulate an unbounded number of raster buffers.
- Close, reopen, and detach cancel sessions and make old demands stale.

Application migration order:

1. Create one supervisor in `ApplicationHost`, using a factory closure that
   constructs `PdfiumEngine` on the supervisor thread. Do not construct a
   PDFium engine before moving the factory.
2. Replace each `PdfWorker::start` with `supervisor.attach`. Keep the returned
   `DocumentClient` and event receiver in that document's controller.
3. On `SupervisorEvent::Opened`, store the returned `DocumentSession` in the
   controller. Build render, text, and preview demands from that session.
4. Translate a settled viewport into one `replace_render_viewport` call. The
   order-to-priority conversion and the existing 150 ms zoom debounce remain
   in the per-view controller; latest-wins cancellation is then enforced by
   the supervisor.
5. Schedule visible text, copy, search, link resolution, and document analysis
   through their separate `WorkClass` domains. Preserve the current short
   quiet period before automatic visible-text extraction in the controller.
6. Adapt tagged supervisor events back to the existing `WorkerEvent` model
   while the UI is migrated. Route by `SupervisorDocumentId`; never infer the
   destination from whichever window is active.
7. Keep search and scientific-analysis state machines outside the engine
   owner. They request one page of background text at a time and yield between
   pages, allowing visible work from any document to run.
8. Drop or explicitly close the document client when its document session is
   evicted. Keep the supervisor itself alive until application shutdown.

The old worker must not run alongside the supervisor after migration: two
independent PDFium owner threads would violate the serialization invariant.
