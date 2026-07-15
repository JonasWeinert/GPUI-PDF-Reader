# Text layer

- `FPDFText_LoadPage` is synchronous. Automatic extraction waits until visible rendering is done and the viewport has been quiet for 200ms.
- The later character walk checks for replacement commands every 64 characters.
- Explicit copy extraction retains its completed prefix and resumes after viewport work instead of restarting the page.
- `pdfium-render`'s normal `chars()` path eagerly builds an index vector. The bounded walk validates the count once and uses `char_at_unchecked` only inside that range.
- Cap a page at 100,000 characters and normal worker/UI text caches at 16 pages.
- Extract coordinates at a fixed high-precision raster independent of zoom so hit boxes do not move.
- Convert all four bounds corners, normalize to the page, clamp to 0–1, and discard non-finite or empty geometry.
- Preserve characters without usable bounds because their offsets and values still matter for copying.
- Keep PDFium character order as canonical. The spatial index stores original offsets only.
- The bounded 16×16 grid speeds hit testing and visible selection painting without copying glyphs into many cells.
- Cache a malformed text layer as empty and report a warning. It must not discard a valid bitmap or stall cross-page copy.
- Select All stores endpoints only. Copy streams one page at a time and stops at the 64MiB clipboard limit.
