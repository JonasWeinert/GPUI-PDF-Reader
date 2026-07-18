# Reference Theme Pack

This is a declarative extension package: it contributes a nested View menu,
one settings view, and a bounded preset value. It does not execute native or
WebAssembly code and cannot inject GPUI elements, CSS, or arbitrary colors.
The trusted host maps its semantic theme choice to host-owned theme tokens.

The strict loadable directory is `package/`. Rebuild its typed manifest and UI
contract from the repository root with:

```sh
cargo run -p key-extension-wasm --example build_reference_extensions
```

The generated package is unsigned and intended for local development and
tests. Distribution builds should apply the product's signature policy.
