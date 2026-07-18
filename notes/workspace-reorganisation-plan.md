# Workspace reorganisation plan

## Goal

Ship GPUI PDF Reader as a small standalone application while making the PDF,
Markdown editor, shared GPUI UI, and extension host usable by a future Key
application with notes, multiple files, and other document types.

Installable extensions are an architectural requirement, not a later bolt-on.
The workspace must support declarative packages, sandboxed WebAssembly
components, and trusted native built-ins through one typed semantic contract.
The detailed security and runtime design is in
`notes/installable-extension-architecture.md`.

The first step is a Cargo workspace in this repository. Separate repositories
are deferred until an extracted crate has a stable API, independent consumers,
and its own release cadence.

## Dependency rules

- Core crates must not depend on GPUI, PDFium, networking, app actions, menus,
  file dialogs, or repository paths.
- Extension API crates must not depend on GPUI, PDFium, Wasmtime, application
  crates, storage implementations, or unrestricted OS types.
- GPUI crates may depend on their matching core crate and shared GPUI UI.
- Engine, storage, and network implementations depend inward on core traits.
- The WebAssembly runtime is an optional host adapter. Domain and extension
  contracts do not expose its types.
- Native and WebAssembly extensions use the same commands, events, snapshots,
  effects, capabilities, and UI contribution schema.
- Extensions never receive raw GPUI, PDFium, filesystem, or socket access.
- Application crates assemble implementations and own windows, menus, file
  dialogs, packaging, global shortcuts, trusted built-in selection, permissions,
  and product-specific policy.
- Key Notes must not depend on PDF crates. The future Key app may compose both.
- No library crate may import an application crate.
- Avoid a general-purpose `key-core` dependency magnet. Add a tiny foundation
  crate only after two domains share stable, genuinely generic types.

## Target layout

```text
Cargo.toml
crates/
  key-extension-api/
  key-extension-host/
  key-extension-wasm/
  key-extension-gpui/
  key-ui-gpui/
  key-editor-core/
  key-editor-gpui/
  key-pdf-core/
  key-pdf-runtime/
  key-pdfium/
  key-pdf-gpui/
  key-pdf-extension-api/
  key-safe-http/
  key-sidecar-store/
extensions/
  key-reference/
  reference-theme-pack/
  reference-document-statistics/
apps/
  gpui-pdf-reader/
  key/                    # later
wit/
  key-extension/
  key-pdf/
tests/
  fixtures/
  e2e/
xtask/                    # shared build, QA, and packaging commands
```

This is a target ownership map, not a requirement to create every crate at
once. A crate is created only when its dependency rule can be tested or it has
an independently optional, reusable, or installable responsibility.

## Package responsibilities

### key-extension-api

Platform-neutral extension identity, manifests, compatibility, lifecycle,
commands, events, snapshots, effects, capabilities, permissions, structured
errors, settings schemas, and declarative UI data types. It mirrors the public
WIT contract where Rust-native code needs the same semantics.

### key-extension-host

Package validation, dependency resolution, lifecycle, capability arbitration,
permission decisions, event scheduling, effect validation, quotas, namespaced
storage routing, diagnostics, suspension, and safe mode. It knows neither GPUI
nor PDFium and runs native and WebAssembly adapters through a common interface.

### key-extension-wasm

Optional WebAssembly Component Model runtime adapter. It validates and
instantiates components, links only granted WIT capabilities, enforces fuel,
deadlines, memory and host-call quotas, and converts traps into extension
diagnostics. No other crate depends on a concrete Wasm runtime.

### key-extension-gpui

Trusted GPUI renderer for the constrained declarative UI tree, contribution
slot manager, focus and keyboard arbitration, panel ownership, overlay ordering,
and bounded UI patch application. WebAssembly extensions never construct GPUI
elements directly.

### key-ui-gpui

Theme tokens, icons, buttons, panel shells, headers, empty states, animation
values, and common text-input plumbing. It must not know about PDFs, comments,
search results, or scholarly references.

### key-editor-core

Rich-text state, Markdown parsing and serialization, selections, editing
commands, validation, and configurable persistence limits. No GPUI types.

### key-editor-gpui

The reusable Markdown editor, slash menu, formatting controls, IME adapter,
and editor events. Placeholder, limits, available commands, and styling are
configuration rather than PDF-comment constants.

### key-pdf-core

Document geometry, layout, text layers, spatial indexes, selection, search,
annotations, links, navigation jumps, and scientific-document detection. It
defines engine and annotation-storage interfaces but contains no PDFium, GPUI,
filesystem convention, extension runtime, or HTTP client.

### key-pdf-runtime

Engine-independent document session coordination, generation and cancellation,
render/text demand, search scheduling, cache policies, annotations, navigation,
and typed service handles. It exposes validated semantic operations rather than
engine internals.

### key-pdfium

PDFium binding and lifetime management, document loading, TOC/link/text
extraction, rasterization, and engine-specific mutation implementations. It
implements `key-pdf-core` engine traits and preserves single-thread PDFium
ownership. Library discovery is injected by the application rather than
assuming a repo or bundle layout.

### key-pdf-gpui

Embeddable PDF viewport and reader components. It owns trusted PDF interaction,
presentation, and integration with extension UI slots, but not the application
window, global menus, file picker, product navigation, or extension package
policy. It accepts services/configuration and emits typed events.

### key-pdf-extension-api

Versioned PDF-specific capabilities and WIT worlds layered on the generic
extension API: document metadata, text, selection, navigation, overlays,
annotations, and separately reviewed mutations. It exposes opaque resources
and semantic commands, never PDFium or GPUI objects.

### key-safe-http

Bounded public-network HTTP, domain permissions, redirect and DNS validation,
response limits, cancellation, and cache policy. Applications may omit it.
Extensions receive only capability-scoped handles.

### key-sidecar-store

Standalone reader implementation of document-scoped extension and annotation
storage using adjacent, versioned sidecars. The future Key app can provide a
database implementation of the same semantic stores.

### extensions

First-party and reference extension packages. `key-reference` contains website
preview and scholarly metadata functionality once it is ready to leave the
reader. Reference packages prove declarative-only and Wasm backend-plus-UI
contracts. Independently installable features may contain backend logic, UI,
or both without requiring one crate per small component.

### gpui-pdf-reader app

Standalone product shell: window, menus, shortcuts, open dialog, adjacent JSON
store selection, PDFium bundle lookup, extension permission policy, official
default extension bundle, release packaging, and app-specific QA.

### Key app

Future composition shell. It can use the PDF reader with a database-backed
store and add note/file/navigation packages without changing the standalone
reader. It may expose a broader capability set while using the same extension
package format.

## Public integration contracts

- `PdfEngine`: open/close, page metadata, text, render demand, preview demand,
  cancellation, and typed events.
- `PdfSession`: engine-independent document services and generation-scoped
  handles.
- `AnnotationStore`: load and compare-and-save annotations for a document ID.
- `ReferenceProvider`: resolve a reference with cancellation and bounded data.
- `PdfReaderConfig`: appearance, enabled optional features, limits, and stores.
- `PdfReaderEvent`: document status, open URL, copy, navigation, and errors.
- `MarkdownEditorConfig` and `MarkdownEditorEvent`: editor policy without PDF
  or application action dependencies.
- `ExtensionManifest`: identity, compatibility, permissions, dependencies,
  contributions, settings, limits, and package integrity.
- `ExtensionHost`: validation, activation, subscriptions, effect arbitration,
  suspension, unloading, and diagnostics.
- `ExtensionRuntime`: replaceable native or WebAssembly execution adapter.
- `ExtensionEvent` and `ExtensionEffect`: bounded, typed state flow with cause
  IDs and no direct host mutation.
- `UiContribution`: constrained, host-rendered UI trees and typed slots.
- `CapabilityProvider`: resolves versioned semantic capabilities without
  exposing implementation handles.

Prefer small structs and enums over broad callback bags. Add external contract
tests for every public crate before treating its API as stable.

## Migration phases

1. Freeze current behavior with saved unit, contract, and native E2E baselines.
   Agree the extension threat model and the read-only v1 capability boundary.
2. Add a virtual workspace and move the unchanged binary to
   `apps/gpui-pdf-reader`. Keep one lockfile, shared dependency versions,
   patches, profiles, and lints at the workspace root.
3. Extract `key-editor-core` from the existing pure Markdown model module.
   Preserve all model tests before moving the GPUI editor.
4. Extract `key-pdf-core` from model, search, annotations, link resolution,
   scientific detection, document jump, and navigation-focus logic. Split
   annotation filesystem persistence from its domain types.
5. Extract engine-independent `key-pdf-runtime`, then `key-pdfium` from the
   internal engine module. Inject library locations and preserve the current
   one-thread, latest-wins, bounded-cache invariants.
6. Extract `key-ui-gpui` and `key-editor-gpui`. Move app edit actions and key
   bindings to the app shell; components expose commands and events.
7. Define `key-extension-api`, the manifest, capability taxonomy, state/effect
   model, WIT packages, and package validation. Do not execute extensions yet.
8. Extract `key-extension-host` and route one trusted native feature through
   the new semantic API while preserving UI and behavior.
9. Implement the constrained UI tree in `key-extension-gpui` and prove a
   declarative theme-pack extension.
10. Implement `key-extension-wasm` with no default WASI capabilities and strict
    runtime limits. Prove it with a read-only document-statistics extension.
11. Define `key-pdf-extension-api` from the capabilities proven by those pilots.
    Convert a TOC alternative to validate dynamic UI, overlays, and navigation.
12. Decompose the reader into viewport, search, comments, TOC, link preview,
    reference details, and toolbar components. Move a feature behind the
    extension contract only when the contract handles its real needs.
13. Make safe HTTP optional and extract citations/reference metadata as a
    first-party extension after permission and cancellation contracts pass.
14. Design and threat-model document mutations separately before exposing
    redaction or other write capabilities.
15. Add the future Key app and database storage only after standalone reader
   packaging and tests still work from its thin app crate.

Each phase must compile and pass tests before the next begins. Avoid mixing a
file move with behavior changes unless a failing contract test requires it.

## Testing and CI

- Keep pure unit tests beside their owning crate.
- Add external API tests that consume each library as another crate would.
- Add mock engine, store, and reference providers for deterministic component
  and controller tests.
- Add reference native, declarative, and Wasm extensions as compatibility
  fixtures across supported API versions.
- Test package validation, permission denial, missing capabilities, dependency
  cycles, traps, timeouts, memory growth, event feedback loops, UI limits,
  unload, safe mode, and state migration rollback.
- Fuzz package extraction, manifests, declarative UI, WIT boundary values,
  effects, and extension migrations.
- Move the repeated macOS process/watchdog/log logic into `xtask`; retain small
  declarative E2E scenarios per application.
- Run formatting, Clippy with warnings denied, all workspace tests, the vendor
  tile regression, and strict license checks in CI.
- Run native macOS E2E separately on a logged-in GUI runner. Keep real display
  transition checks as a documented manual or hardware-runner suite.

## Dependencies, patches, and licensing

- Use workspace dependency declarations and exact versions where runtime
  compatibility requires them.
- Keep the WebAssembly runtime optional so minimal and notes-only products do
  not compile or ship it unless installable executable extensions are enabled.
- Audit the runtime, Component Model toolchain, extension packages, and decoded
  assets under the same permissive-license policy as application dependencies.
- Cargo patches are not inherited by downstream users. Before publishing the
  GPUI or PDFium-facing crates, upstream the required fixes or publish clearly
  named forks pinned by version or revision. Requiring every consumer to copy
  patch entries is a last resort.
- Replace the license deny-list with an explicit approved SPDX policy covering
  normal, build, dev, tool, native, and target-specific dependencies.
- Generate the actual distributable notices bundle for each application.

## Repository strategy

Keep the workspace in one repository while boundaries are moving. This allows
atomic API changes, one patch and lock policy, shared fixtures, and one CI
pipeline. Standalone shipping is provided by the app package and release job,
not by a separate Git repository.

Do not use Git submodules for normal Rust package composition. If a crate later
needs an independent repository, consume a semver release or pinned Git tag.
Split only when ownership, consumers, API stability, and release cadence justify
the extra coordination.

## Completion criteria

- GPUI PDF Reader builds and packages without the future Key app.
- GPUI PDF Reader can start in safe mode with all third-party extensions
  disabled.
- The standard reader bundle and a minimal reader bundle are independently
  buildable and tested.
- The Key app can embed the PDF and editor components without importing reader
  menus, file dialogs, sidecar paths, or PDFium bundle assumptions.
- Notes-only builds do not compile or ship PDFium or scholarly networking.
- Core crates compile without platform UI dependencies.
- Public APIs have external contract tests and documented versioning policy.
- Both applications can choose their own storage and optional features.
- A declarative extension and a sandboxed Wasm extension can be installed,
  validated, permissioned, enabled, disabled, and removed without restarting
  into an unsafe state.
- Native and Wasm extensions use the same typed commands, effects, snapshots,
  capabilities, and UI contribution semantics.
