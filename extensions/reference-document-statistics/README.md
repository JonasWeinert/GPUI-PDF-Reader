# Reference Document Statistics

This package is the installable-extension sandbox pilot. It negotiates only
PDF metadata and text capabilities, previews its three permissions before
activation, contributes a semantic side panel, and runs through the same host
lifecycle as native extensions.

`source/component.wat` is the auditable Component Model source. It imports no
WASI, filesystem, clock, random, or network interface. The current pilot
observes bounded lifecycle and document events, then publishes a typed
`runtime-ready` state patch with no effects. The trusted host validates and
atomically stores that state before its renderer resolves the panel. PDF access
and rendering stay host-owned, so the guest never receives direct document
authority.

Rebuild `package/component.wasm` and the typed package files from the repository
root with:

```sh
cargo run -p key-extension-wasm --example build_reference_extensions
```

The generated package is unsigned and intended for local development and
tests. Distribution builds should apply the product's signature policy.
