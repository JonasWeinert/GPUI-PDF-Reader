# Viewport tile patch

This directory is an exact source copy of `pdfium-render` 0.9.2 plus a focused viewport-tile
patch. The upstream package and this patch remain available under `MIT OR Apache-2.0`; see
`LICENSE.md`.

The patch adds this public method to `PdfPage`:

```rust
pub fn render_tile_with_config(
    &self,
    config: &PdfRenderConfig,
    tile_left: Pixels,
    tile_top: Pixels,
    tile_width: Pixels,
    tile_height: Pixels,
) -> Result<PdfBitmap<'_>, PdfiumError>
```

It also adds `PdfPageText::char_at()` and the explicitly unsafe
`char_at_unchecked()`. GPUI PDF Reader validates PDFium's character count once, caps it
at 100,000, and then uses the unchecked accessor for its bounded, cancellable
walk; that avoids both a full index-vector allocation and one redundant
`FPDFText_CountChars()` call per character. The upstream `chars()` convenience
method remains unchanged.

`config` defines the full rendered page size. The four tile values are device pixels in that
full-page coordinate space. The method validates that the tile is non-empty, non-negative,
overflow-free, and fully inside the effective configured page size before allocating anything.
It allocates only `tile_width * tile_height` pixels.

Internally the patch calls `FPDF_RenderPageBitmap()` with `start_x = -tile_left`,
`start_y = -tile_top`, and the configured full page width and height. It then calls
`FPDF_FFLDraw()` with the same geometry. This is the same path used by the upstream full-page
renderer, so PDFium remains responsible for intrinsic page rotation, CropBox handling,
annotations, widget appearance, and AcroForm data.

Pdfium cannot combine `FPDF_FFLDraw()` with matrix or clip rendering. A configuration that has
disabled form rendering—including one modified by `PdfRenderConfig::clip()` or a transformation
method—is rejected with `PdfiumError::PageRenderTileUnsupportedConfiguration` rather than
silently omitting forms. Invalid tile geometry returns `PdfiumError::PageRenderTileOutOfBounds`.

## Scheduler constraints

`FPDF_RenderPageBitmap()` and `FPDF_FFLDraw()` are synchronous. An individual call cannot be
cancelled safely. The application should:

- use bounded tiles (1024 device pixels plus bleed is a reasonable starting point);
- check generation and latest viewport demand before every tile;
- discard a result if its generation or scale key became stale while PDFium was rendering;
- keep all PDFium calls on the existing dedicated renderer thread;
- use a latest-wins pending-tile map so stale queued work is replaced;
- request a 32-pixel bleed around adjacent tile cores and paint only the core, because
  antialiasing at a hard bitmap edge can otherwise create a visible seam.

PDFium exposes progressive rendering entry points, but adding their pause callback lifecycle and
separate form-overlay cancellation would be a substantially larger change. Bounded synchronous
tiles provide predictable cancellation latency without broadening the unsafe API surface.

## Verification

The focused unit test `test_tiled_rendering_matches_full_rendering` uses:

- `test/tile-render.pdf`: portrait, inherited 90-degree rotation, and a non-default CropBox;
- a live square annotation added through `pdfium-render`;
- `test/tile-form.pdf`: an AcroForm text field and a square annotation;
- form-field highlighting to prove that `FPDF_FFLDraw()` contributes pixels beyond the widget's
  static appearance;
- out-of-bounds and unsupported-configuration checks.

The fixture generator is `test/create-tile-form-pdf.rs`.
