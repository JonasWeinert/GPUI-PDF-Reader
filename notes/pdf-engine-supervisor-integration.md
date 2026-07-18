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

Integration is complete:

- `ApplicationHost` constructs and owns exactly one supervisor. The engine
  factory runs on its owner thread; no PDFium engine is created in a view.
- Each `PdfReader` attaches a routed `DocumentClient`. Its document adapter
  translates the established `WorkerCommand`/`WorkerEvent` UI seam while
  search and scientific state machines remain outside the engine owner.
- Visible tiles, prefetch, preview, visible text, copy, search, link text, and
  scientific analysis retain independent replacement/cancellation domains.
- The 150 ms zoom debounce and 200 ms automatic-text quiet period remain in
  the view/document orchestration layers.
- Commands and tagged supervisor events share one bounded document mailbox.
  Raster publication is one-item bounded. A full route is retained and retried
  by the supervisor without cloning a multi-megabyte bitmap.
- `PdfWorker` keeps a client clone so caller-side replacement cancels engine
  work immediately, before a queued adapter command is consumed.
- The last `PdfWorker` handle sets an atomic shutdown flag and wakes its
  adapter. This breaks the routed-sender/client ownership cycle even when the
  mailbox is full, then detaches the document from the supervisor.
- The previous per-reader PDFium engine path is no longer compiled. Running it
  beside the supervisor would violate the serialization invariant.
