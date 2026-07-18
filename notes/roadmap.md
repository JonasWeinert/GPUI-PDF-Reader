# Roadmap

This file records work deliberately deferred from the multi-file and
multi-view refactor. Items stay here until they are implemented or moved into
an accepted product plan.

## Workspace and views

- Add tabs and split panes by allowing a window content layout to contain more
  than one view placement.
- Add an explicit **New View** command for multiple simultaneous views of one
  item. Reuse the same document session while keeping view-local navigation,
  selection, zoom, and panels independent.
- Publish an additive multi-document extension API only after the internal
  item, view, window, document-session, and generation contracts have proven
  stable in the standalone reader.
- Add window and workspace restoration, including bounds, dock visibility,
  active items, and content-layout restoration with missing-file handling.
- Add a real workspace/file-tree sidebar. The current left window dock is the
  integration point; it must remain outside document content and view-local
  panels.
- Add dock stacking, resizing, reordering, and multiple contributions per dock
  after real window-level tools need them.
- Replace the Settings mock with persisted application, workspace, resource,
  appearance, and extension settings.
- Add installable-extension contributions for complete workspace views and
  window-level panels after their bounded declarative contracts and lifecycle
  rules are designed.

## Resource management

- Add resource participants and demand estimators for Markdown editing, video
  playback, indexing, and future note/database views. The coordinator is
  domain-neutral now; each domain still needs measured cost models.
- Add platform-specific occlusion, minimization, thermal, and power-state
  signals. GPUI 0.2.2 does not expose every signal required for accurate
  automatic activity classification.
- Calibrate Auto, Saver, Balanced, and Performance budgets on old and current
  Macs using measured CPU time, resident memory, GPU texture residency, input
  latency, and power use.
- Measure the capped per-document orchestration adapter overhead with dozens of
  open PDFs. If its bounded 512 KiB stack and sleeping mailbox are material,
  replace adapters with a fair fixed-size actor pool without moving search or
  scientific state onto the PDFium owner thread.
- Consider an opt-in PDFium helper-process performance mode for machines with
  enough memory and cores. The default remains one process-wide PDFium owner
  because PDFium APIs are not thread-safe.
- Add a global disk-cache budget and eviction policy for link previews and
  other temporary document data.

## Products and packaging

- Add the future Key application only after the standalone reader continues to
  build, test, and package from the shared crates without taking note-specific
  dependencies.
- Add independent repositories or published crates only when an extracted
  package has a stable API, a second real consumer, and a justified independent
  release cadence. Until then the Cargo workspace is the maintenance boundary.
- Add Markdown, video, notes, and database-backed item/session implementations
  through the same workspace host rather than adding type checks to the PDF
  reader shell.
