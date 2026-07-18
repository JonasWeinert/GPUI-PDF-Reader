# key-pdf-extension-api

Pre-stable semantic contracts between Key PDF hosts and extensions. This crate
contains only transport values, validation, capability names, and narrow
service traits. It depends on `key-extension-api` for generic identities and on
`key-pdf-core` for shared navigation behavior.

The boundary intentionally contains no PDFium, GPUI, filesystem path, HTTP,
socket, Wasmtime, or executor type. Native built-ins and WebAssembly adapters
must implement the same service semantics and structured errors.

## Contract rules

- A document open or replacement creates a new `GenerationId`.
- Document, page, and overlay-set handles are opaque tokens. A numeric ID is
  never authority without a matching live generation and provider lookup.
- Capabilities are requested independently. Text access does not follow from
  metadata access, and navigation does not imply overlay access.
- All snapshots and extension requests are validated with
  `PdfValidationContext` before retention or dispatch.
- Navigation uses `key-pdf-core::DocumentJump`, including centering and the
  shared post-scroll sweep/pulse focus semantics. Providers may refine rough
  page destinations with the bounded `TextTargetHint` before making the jump.
- Overlays use normalized page geometry and semantic theme tones. They cannot
  supply arbitrary UI trees, colors, animation code, or z-index values.
- This 0.1 contract is explicitly pre-stable. Additive and corrective changes
  are expected until native and WebAssembly pilots prove the boundary.

The versioned Component Model declaration is in
`wit/v0.1.0/pdf-extension.wit`. The `full-read-only-host` world is a conformance
surface; a production runtime links only the interfaces granted to an
extension.

