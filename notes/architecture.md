# Architecture

The repository is one Cargo workspace with thin application composition over
reusable libraries. Keep it in one repository while the public contracts are
pre-stable; split a crate only after it has an independent consumer, owner,
release cadence, and compatibility policy.

## Package direction

- `key-editor-core` and `key-pdf-core` contain deterministic domain logic.
- `key-pdf-runtime` owns engine-independent sessions, generations, demand,
  cancellation, and cache policy.
- `key-pdfium` is the only PDFium adapter. It implements the runtime engine
  contract without exposing a PDFium handle.
- `key-ui-gpui`, `key-editor-gpui`, and `key-pdf-gpui` contain reusable GPUI
  presentation and controller adapters.
- `key-sidecar-store`, `key-safe-http`, and `key-reference` are replaceable
  storage, bounded-network, and scholarly-provider implementations.
- `key-extension-api` is the runtime-neutral semantic contract.
  `key-extension-host` owns lifecycle and policy, `key-extension-gpui` renders
  bounded data trees, and optional `key-extension-wasm` executes Component
  Model guests without WASI.
- `key-pdf-toc` is the first host-managed PDF feature pilot. Its trusted rail
  dispatches through extension lifecycle, permissions, outline, and navigation
  capabilities rather than receiving engine handles.
- `gpui-pdf-reader` owns product policy: windows, menus, file dialogs, PDFium
  discovery, default features, permission prompts, and release packaging.

Dependencies point inward. Core and extension-contract crates never depend on
GPUI, PDFium, Wasmtime, networking, app actions, filesystem conventions, or
another app. Run `scripts/audit-boundaries.sh` after changing manifests.

## PDF execution

- GPUI owns the main thread, window, input, layout, and Metal painting.
- All PDFium calls stay on one dedicated worker thread. Do not assume PDFium
  documents, pages, or form handles are thread-safe.
- The app talks to the engine through `key-pdf-runtime`. A session generation
  makes results, handles, overlays, and extension snapshots from an older
  document unobservable.
- Demand is latest-wins. A new viewport or zoom generation cancels superseded
  queued work before it can consume the render budget.
- Worker results use a one-item bounded channel so bitmap data cannot pile up
  while the UI is busy.
- Page rasters are conceptual. Allocate viewport tiles, never a whole
  high-zoom page.
- Tile identity includes page, raster size, column, and row. Layout stores page
  sizes and an f64 height prefix once; zoom rescaling is O(1) and visible-page
  queries are O(log n).
- Retire removed GPU images across two frame callbacks because an already
  submitted Metal frame may still reference them.
- Keep rendering, text extraction, annotation I/O, networking, and UI failures
  separate. A failed tile or text layer is a warning, not a fatal document
  error.

## Extension execution

Extensions exchange immutable events, bounded state, and requested semantic
effects. They never receive GPUI, PDFium, filesystem, socket, process, or raw
pointer authority.

- Native built-ins, declarative packages, and Wasm components use the same
  IDs, manifests, commands, snapshots, effects, capabilities, and UI slots.
- The host validates dependencies, license, compatibility, permissions,
  contribution bounds, effect authority, cause depth, queue size, and state
  size before exposing a result to the app.
- Declarative UI and extension menus are data rendered by trusted host code.
  Nested menus are supported only in product-owned slots; packages cannot
  invent global z-order or arbitrary native views.
- Wasm packages get isolated stores, fuel, epoch deadlines, memory/table/stack
  limits, bounded input/result mailboxes, cancellable off-GPUI execution, and
  no default WASI imports.
- The PDF capability bridge publishes generation-scoped metadata, text,
  selection, viewport, outline, and link snapshots. Navigation and overlays
  return through the shared jump and paint abstractions.
- Safe mode disables non-bundled packages. One failed package is suspended or
  rolled back without preventing startup, document replacement, or close.

Installable execution is a standard-bundle feature. The minimal reader omits
both Wasmtime and scholarly networking; it still opens, renders, searches, and
annotates PDFs through the same core/runtime boundaries.

## Storage and networking

- Annotation stores use compare-and-save semantics and atomic replacement.
  The standalone app injects adjacent JSON sidecars; a future app can inject a
  database without changing the editor or PDF domain.
- Extension settings and document state are separately namespaced, atomically
  persisted, and quota-enforced. Ephemeral extension cache and tasks are
  invalidated before a new document generation becomes visible.
- Keep website preview and scholarly networking outside the PDFium worker.
  `key-safe-http` bounds domains, redirects, resolved addresses, time, bytes,
  content types, image dimensions, concurrency, and cancellation.
- Website assets live in a per-document temporary cache whose drop purges the
  files. Scholarly metadata is bounded in memory and discarded with the
  document session.

## Reuse rule

An embedding app should construct the editor, viewport controller, PDF
runtime, stores, capability providers, and optional extension adapters through
their public crates. It must not import `apps/gpui-pdf-reader`, copy its reader
state, or inherit its menus, file picker, sidecar path, PDFium bundle lookup,
or default extension policy.
