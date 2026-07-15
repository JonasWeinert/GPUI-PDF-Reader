# Architecture

- GPUI owns the main thread, window, input, layout, and Metal painting.
- All PDFium calls stay on one dedicated worker thread. Do not assume PDFium documents or form handles are thread-safe.
- Worker results use a one-item bounded channel so bitmap data cannot pile up while the UI is busy.
- Page rasters are conceptual. Allocate viewport tiles, never a whole high-zoom page.
- Tile identity includes page, raster size, column, and row. Document generation rejects results from an older file.
- Layout stores page sizes and an f64 height prefix once. Zoom rescaling shares that geometry and is O(1); visible-page queries are O(log n).
- Keep rendering, text extraction, and UI failures separate. A failed tile or text layer is a warning, not a fatal document error.
