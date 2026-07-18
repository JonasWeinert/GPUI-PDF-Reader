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

## Workspace host

- `ApplicationHost` is the process owner for shared extension, PDF engine,
  annotation, reference, theme, resource, item, view, and window services.
- A `WorkspaceWindow` is window chrome around a typed view. PDF and Settings
  are the first two view kinds; the window layout already reserves left,
  right, bottom, overlay, and modal slots outside document-local panels.
- Items, views, and windows have separate typed IDs. Opening the same canonical
  file focuses its existing window; opening a different file creates another
  item, view, and window. A host-owned Settings view needs no fake document.
- The current layout is one view per window. Tabs, splits, and multiple views
  of one item are additive layout changes, not changes to PDF domain state.
- Theme selection and extension runtime ownership are application-scoped.
  Selection, zoom, scrolling, panels, and navigation remain view-scoped.

The resource registry is domain-neutral. It assigns budgets from system RAM,
CPU count, Low Power Mode, the selected Auto/Saver/Balanced/Performance mode,
and a view MRU activity set. PDF views translate allocations into tile bytes,
tile count, cached text pages, and prefetch permission. Older views become
cold and then suspended; suspension cancels demand, drops CPU text state, and
retires Metal images through the safe two-frame path. Future Markdown, video,
indexing, and database views implement the same participant contract.

## PDF execution

- GPUI owns the main thread, window, input, layout, and Metal painting.
- All PDFium calls for every open document stay on one process-wide
  `pdfium-engine-owner` thread. Do not assume PDFium documents, pages, or form
  handles are thread-safe.
- Each open PDF has a small event-driven orchestration adapter for search,
  scientific analysis, text caching, and demand translation. It never owns or
  calls PDFium. Its mailbox and raster output are bounded, its stack is capped,
  and dropping the last view handle explicitly breaks its supervisor route.
- The supervisor rotates runnable documents fairly. A full background
  window's event route pauses only that document and cannot block foreground
  engine work.
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
  One process service fairly routes every document through one bounded writer
  thread, coalesces adjacent revisions, never drops a full UI event lane, and
  flushes accepted saves without blocking window close. The standalone app
  injects adjacent JSON sidecars; a future app can inject a database without
  changing the editor or PDF domain.
- Extension settings and document state are separately namespaced, atomically
  persisted, and quota-enforced. Ephemeral extension cache and tasks are
  invalidated before a new document generation becomes visible.
- Keep website preview and scholarly networking outside the PDFium owner.
  `key-safe-http` bounds domains, redirects, resolved addresses, time, bytes,
  content types, image dimensions, concurrency, and cancellation.
- Link previews and scholarly metadata share one fixed process executor with
  fair per-document scopes, bounded queues, generation cancellation, and a
  global cache ledger. Dropping a document scope cancels its work and purges
  its temporary files.
- Website assets live in a per-document temporary cache whose drop purges the
  files. Scholarly metadata is bounded in memory and discarded with the
  document session.

## Reuse rule

An embedding app should construct the editor, viewport controller, PDF
runtime, stores, capability providers, and optional extension adapters through
their public crates. It must not import `apps/gpui-pdf-reader`, copy its reader
state, or inherit its menus, file picker, sidecar path, PDFium bundle lookup,
or default extension policy.
