# Build artifacts

Rust build output is intentionally not repository data. All `target`
directories, Python bytecode, app bundles, and local annotation sidecars are
ignored by Git.

Debug builds are large because GPUI, Wasmtime, PDFium bindings, and their
transitive dependencies produce debug symbols, object files, build-script
outputs, and separate dependency hashes for standard, minimal, test, and
feature combinations. Incremental compilation can retain several generations
of the same large crate. A 20 GB or larger `target` directory is therefore
possible even though `.git` remains small.

The workspace quality script disables incremental compilation. Its isolated
vendored PDFium parity test writes to `target/vendor-pdfium-render`, not inside
`vendor`, so all generated output has one cleanup boundary.

Safe cleanup options:

- `cargo clean` removes all Cargo output.
- Delete `target/` for the same complete local reset.
- Remove only an obsolete profile or isolated test directory when preserving
  the current cache matters.

Cleanup never removes source, fixtures, downloaded PDFium libraries under
`lib/`, or Git history. The next build recreates anything it needs.
