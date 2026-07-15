# PDFium

- `FPDF_RenderPageBitmap`, `FPDF_FFLDraw`, and `FPDFText_LoadPage` are synchronous and cannot be safely interrupted once entered.
- Bound render latency with small tiles. Progressive rendering would require a larger pause-callback and form-overlay design.
- Render a tile in full-page device coordinates with negative tile offsets and the full configured page size. PDFium handles intrinsic rotation and CropBox geometry.
- Pass the same offsets and dimensions to `FPDF_FFLDraw`, or form appearances will not align with page content.
- Intrinsic 0, 90, 180, and 270 degree rotations use the same tile-origin rule. Do not manually rotate tile offsets.
- Hard bitmap edges change PDFium culling and antialiasing. An 8px bleed failed on a rotated page; the fixture needed about 18px, so the app uses 32px and paints only the core.
- Fixed bleed is a practical guard, not a guarantee for every hostile PDF. Keep the full-versus-tiled regression.
- Matrix/clip rendering cannot be combined with the form overlay in this path. Reject unsupported configurations instead of silently omitting forms.
- Enable annotations, form data, and PDFium's decoded-image cache limit explicitly.
- Validate page sizes, raster dimensions, tile containment, integer overflow, returned dimensions, and byte length before trusting native results.
- The vendored `pdfium-render` changes are deliberately narrow: viewport tile rendering and constant-memory character access.
