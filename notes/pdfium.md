# PDFium

- `FPDF_RenderPageBitmap`, `FPDF_FFLDraw`, and `FPDFText_LoadPage` are synchronous and cannot be safely interrupted once entered.
- Bound render latency with small tiles. The forced-color API is progressive: loop `Start`/`Continue` to a terminal status, always call `FPDF_RenderPage_Close`, then draw the form overlay with the same geometry.
- Render a tile in full-page device coordinates with negative tile offsets and the full configured page size. PDFium handles intrinsic rotation and CropBox geometry.
- Pass the same offsets and dimensions to `FPDF_FFLDraw`, or form appearances will not align with page content.
- Intrinsic 0, 90, 180, and 270 degree rotations use the same tile-origin rule. Do not manually rotate tile offsets.
- Hard bitmap edges change PDFium culling and antialiasing. An 8px bleed failed on a rotated page; the fixture needed about 18px, so the app uses 32px and paints only the core.
- Fixed bleed is a practical guard, not a guarantee for every hostile PDF. Keep the full-versus-tiled regression.
- Matrix/clip rendering cannot be combined with the form overlay in this path. Reject unsupported configurations instead of silently omitting forms.
- Enable annotations, form data, and PDFium's decoded-image cache limit explicitly.
- `FPDF_RenderPageBitmapWithColorScheme_Start` recolors text and vector paths but preserves raster image objects. Clear the bitmap with the dark paper color first and use `FPDF_CONVERT_FILL_TO_STROKE` so adjacent forced-color fills retain boundaries.
- Forced colors do not cover widget appearances drawn later by `FPDF_FFLDraw`; forms remain an explicit limitation to inspect on form-heavy documents.
- Carry the concrete render appearance with every tile demand and completion. Comparing only page/raster keys can publish a stale light tile after a dark-theme replacement.
- When users disable forced PDF colors inside a dark UI, return to the normal PDFium path and a light GPUI paper backing together. Changing either appearance must discard image tiles and re-request the viewport.
- PDFium exposes the document outline as a bookmark graph. Traverse it iteratively with a visited set, cap nodes/depth/title bytes, accept only same-document destinations, and validate every destination page against the opened page count. A bookmark action may contain the local destination when `FPDFBookmark_GetDest` does not.
- Preserve explicit destination y coordinates through PDFium's view settings and page-to-device conversion. For page-only destinations, lazily extract just that page, search for the normalized outline title, prefer the largest exact occurrence as the likely heading, and fall back to page top only when no match has usable bounds.
- Pdfium's process-global bindings can be reused through a fresh `Pdfium::default()` facade after initialization, but native render tests must not drive the same Pdfium instance concurrently.
- Validate page sizes, raster dimensions, tile containment, integer overflow, returned dimensions, and byte length before trusting native results.
- The vendored `pdfium-render` changes are deliberately narrow: viewport tile rendering and constant-memory character access.
