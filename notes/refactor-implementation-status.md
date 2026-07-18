# Workspace refactor implementation status

## Completed foundation

- One Cargo workspace and lockfile now contain the standalone app, reusable
  editor, PDF domain/runtime/PDFium, GPUI components, storage, safe HTTP,
  extension contracts, host, declarative renderer, package loader, and optional
  Wasm runtime.
- `key-editor-core` has an external-consumer contract suite. `key-editor-gpui`
  is configured and reused by comments.
- PDF engine ownership is split between runtime-neutral contracts and the
  single-threaded `key-pdfium` adapter. The app uses a reusable `key-pdf-gpui`
  canvas with bounded clipping, interaction events, and navigation focus.
- App-owned annotation sidecars implement the reusable store contract.
  Extension settings and document data use atomic, namespaced, quota-enforced
  storage; ephemeral data is purged at document boundaries.
- Standard and minimal bundles build from the same app. Minimal omits Wasmtime
  and scholarly networking.

## Extension platform

- Rust and versioned WIT contracts are exhaustively mapped by tests. They
  contain semantic data only: no GPUI, PDFium, Wasmtime, raw path, socket, or
  process handles.
- The host validates manifests, dependencies, licenses, permissions,
  capabilities, contributions, events, effects, quotas, cause chains, settings,
  lifecycle, safe mode, and document generations.
- Menubar contributions support typed host-owned slots and nested submenus.
- Declarative packages use bounded host-rendered GPUI nodes and validated
  assets. The reference theme pack proves no-code installation.
- Wasm components run on bounded workers outside GPUI callbacks. Queue memory,
  fuel, deadlines, host calls, output, traps, panics, unload, cancellation, and
  ordering-safe coalescing are covered by focused tests. No-default-features
  builds do not include Wasmtime.
- Installed package source hashes, enablement, permission decisions, settings,
  assets, and restoration failures are durable. The manager exposes individual
  permission grants and revocation. Asynchronous Wasm upgrades stay provisional
  until their worker settles; failure restores the prior package, adapter,
  authority snapshot, durable registry entry, and assets.
- Host semantic services execute namespaced storage and registered cancellable
  tasks without exposing threads or files to extensions.

## Proven pilots

- The bundled theme feature uses the native extension lifecycle and contributes
  the View > Appearance submenu.
- Reference theme and document-statistics packages prove declarative and Wasm
  package loading, validation, permissions, UI, and failure containment.
- `key-pdf-toc` is a real host-managed native PDF extension. The existing TOC
  rail remains trusted presentation, while command dispatch, permissions,
  active-document and outline capabilities, text-hint refinement, navigation,
  centering, focus, rapid-selection coalescing, and invalidation cross the
  extension boundary. The direct navigation path remains only as startup-fault
  fallback.
- `key-reference` and `key-safe-http` isolate optional website/scholarly work
  from PDF core and minimal builds. Their fixed-domain, redirect, public-address,
  response-limit, cancellation, cache-purge, provider-fallback, and partial
  reference behavior is tested.

## Packaging and policy

- The explicit MIT, Apache-2.0, and more-permissive dependency policy covers
  host and cross-target normal/build/dev graphs plus vendored PDFium notices.
- The macOS assembler creates and verifies separate unsigned standard and
  minimal app bundles, including PDFium, themes, exact feature-selected Cargo
  inventories, retained package notices, and native architecture/dependency
  checks.
- The deterministic quality path passes the full workspace all-target suite,
  strict Clippy, the minimal feature build, dependency-boundary audit, 788-record
  license audit, and the isolated PDFium tiled-render parity test.
- Installable third-party packages remain local/development distribution. A
  public store, signing trust service, notarization, and automatic update system
  are not implied by the development signature fixture.

## Deliberately deferred

- Schema v1 does not let an extension construct arbitrary GPUI or dynamically
  repeat an unbounded data collection. The TOC rail therefore remains trusted;
  a future bounded data-list node needs its own compatibility design.
- Search, comments, and reference UI remain first-party feature modules rather
  than installable packages. Their domain/component boundaries are reusable;
  bulk migration was a stated non-goal and should happen only when a second
  product or omission requirement proves the capability shape.
- Secure mutation/redaction remains unimplemented pending the separate threat
  model, adversarial PDF verification, and explicit approval required by Phase
  13.
- The future Key second-brain app is not created. The workspace boundaries and
  minimal bundle prove that it can compose editor functionality without taking
  PDFium, scholarly networking, or the standalone app shell.
- Public API stability is pre-1.0. Keep packages in this repository until a
  second independent consumer and release cadence justify a separate repo.

Build-output growth and safe cleanup are recorded in `notes/build-artifacts.md`.
