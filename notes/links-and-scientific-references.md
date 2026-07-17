# Links and scientific references

- PDF link destinations are often page-level hints. Match normalized source text near the hint, then jump to and focus the full meaningful target range.
- Superscript citations may have punctuation inside the annotation bounds, such as `.9,10`. Strip leading punctuation and treat ranges/lists as starting at their first reference number.
- Infer missing citation links only after strong document-level evidence: a reference section or sequential bibliography plus repeated citation, DOI, or concentrated-link signals.
- Keep all PDFium work on the single worker. Scientific detection runs one page at a time after interactive work and preserves partial extraction when new rendering or search demand arrives.
- PDFium superscript geometry is font-dependent. A useful signal combines substantially smaller glyph height with a raised top or bottom relative to nearby body text.
- OpenAlex works can be fetched by DOI and return abstracts as an inverted index that must be reconstructed by token position.
- Semantic Scholar's title-match endpoint works better with a probable title extracted from the citation than with the entire formatted reference. Compute and display our own title-token certainty.
- Scholarly requests are bounded, public-network-only, initiated on hover, cached only in the document session, and ignored after the document generation changes.
- Keep hover ownership split between source and card with a short delayed close. That lets the pointer cross the gap and interact with buttons or a scrolling card without dismounting it.
- Retain each bibliography entry's exact multi-line text bounds. Use them for target hover, jump focus, and as the hard boundary for scholarly lookup; internal targets outside those bounds are figures/tables/sections and get a local thumbnail instead.
- Render destination thumbnails as bounded, latest-wins crop requests on the existing PDFium worker. A second PDFium thread would reintroduce unsafe concurrent engine access.
- Keep hover cards compact and move long abstracts into a reusable floating detail panel with independent scrolling. Deliberate ellipses are safer than clipping live text against rounded corners.
- Treat a floating details panel as viewport geometry, not decoration: preserve the visible-center anchor during reveal and add the occluded width to horizontal scroll range so no PDF content becomes unreachable.
- Keep the actual sidebar reveal width separate from total panel occlusion. Otherwise a closed Search/Comments panel can remain visible or intercept input behind another floating panel.
- Do not infer a PDF URL from a scholarly landing page. Model publisher/journal, full-text PDF, DOI, and metadata-record destinations separately and label each action honestly.
- Size hover previews from bounded text measurements and interpolate width during metadata reveal; a single fixed tooltip width makes short failures feel empty and long citations cramped.
- A stable title-derived theme color gives each reference visual identity without storing artwork. Use it consistently for the preview rail, detail hero, section labels, and link affordances.
- Prefer editorial dividers and key/value rows over nested gray cards for dense reference metadata. Let long journal names increase the hero height rather than clipping them.
