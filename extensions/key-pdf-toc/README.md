# Key PDF TOC pilot

This bundled native extension proves the pre-stable PDF capability contract.
It receives a bounded title/page selection from a trusted presentation, then
uses only host-arbitrated capabilities to obtain the active document, read its
outline, and request a centered navigation with text refinement and focus.

The adapter keeps at most one capability request in flight and coalesces rapid
selections to the newest target. Document invalidation clears its pending state
and makes all earlier effect completions unusable.

The GPUI rail and hover callout remain application-owned. Schema v1 can render
bounded static lists but cannot repeat a node tree over a dynamic outline. The
controller, lifecycle, permissions, command, resource access, and navigation
are genuine host-managed extension behavior; moving the dynamic presentation
into a package requires a separately reviewed data-driven repeater contract.
