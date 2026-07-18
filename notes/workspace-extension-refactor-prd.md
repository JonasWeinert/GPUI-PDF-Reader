# Workspace and extension refactor PRD

## Status

Draft for discussion. This document authorizes no implementation work.

The refactor starts only after the owner and implementer have reviewed this
order, resolved the decisions marked as gates, and explicitly approved the
first phase. Later high-risk phases require their own approval even if earlier
work has begun.

## Related decisions

- `notes/workspace-reorganisation-plan.md` defines the proposed package map,
  ownership rules, and long-term repository strategy.
- `notes/installable-extension-architecture.md` defines the proposed extension
  package, capability, permission, UI, runtime, and security model.

This PRD converts those architectural intentions into a reversible delivery
order. If the documents disagree, resolve the disagreement before coding; this
PRD does not silently override either design.

## Problem

GPUI PDF Reader is a capable standalone application, but most functionality is
still delivered by one binary crate. Domain logic, PDFium ownership, GPUI
components, product policy, storage, networking, and feature coordination are
therefore easy to couple accidentally.

The future product direction has two simultaneous requirements:

1. Keep shipping the PDF reader as an independent, focused application.
2. Reuse its editor and PDF functionality in a larger Key application, while
   making selected features installable, permissioned extensions.

A direct rewrite into many repositories or a fully dynamic plugin system would
combine too many unknowns. The safer path is a staged Cargo workspace in the
current repository, with measured package boundaries first and installable
execution added only after the semantic contracts are proven.

## Product outcome

At completion:

- GPUI PDF Reader remains independently buildable, testable, and distributable.
- A future Key shell can embed the editor and PDF reader without inheriting the
  standalone reader's menus, file picker, sidecar layout, or bundle assumptions.
- Core PDF and editor behavior is testable without GPUI, PDFium, or macOS.
- Engine, storage, reference, and safe-network implementations are replaceable
  behind narrow interfaces.
- Official built-ins and third-party WebAssembly extensions use the same typed
  events, effects, capabilities, and host-rendered UI semantics.
- The app can install, validate, permission, enable, disable, upgrade, and
  remove declarative and sandboxed extensions without loading arbitrary native
  code.
- A minimal reader bundle does not include optional scholarly networking or a
  WebAssembly runtime; the standard bundle retains today's features.

## Non-goals

This refactor does not initially:

- Build the full Key second-brain application.
- Split packages into separate repositories.
- Use Git submodules for ordinary Rust dependencies.
- Make every existing reader feature dynamically installable.
- Let third-party code construct arbitrary GPUI elements.
- Expose raw PDFium, GPUI, filesystem, socket, or process handles.
- Invent a general-purpose programming language.
- Grant general WASI access.
- Support document mutation or claim secure redaction before a separate threat
  model and engine-level verification exist.
- Change user-visible behavior merely to make extraction easier.
- Promise a stable public extension API before pilots and compatibility tests
  demonstrate that it is sufficient.

## Current baseline

The repository currently has:

- One `gpui-pdf-reader` binary package.
- Domain-oriented source modules for annotations, search, links, scientific
  detection, scholarly lookup, jumps, navigation focus, themes, and editing.
- A PDF backend that is being internally separated from its PDFium engine.
- A reader controller and GPUI view that are being internally decomposed.
- Adjacent JSON annotation persistence.
- Blocking scholarly HTTP behind application code.
- Unit tests in the binary crate and macOS native E2E scenarios.
- Vendored PDFium and patched GPUI/PDFium-facing dependencies.
- A strict MIT/Apache/permissive dependency requirement.

Uncommitted modularity work already present on the branch must be reviewed and
either committed as a behavior-preserving baseline or deliberately discarded
before Phase 1. Workspace moves must not obscure that diff.

## Architectural invariants

Every phase must preserve these rules:

1. Core crates have no GPUI, PDFium, app-shell, filesystem-convention, or
   networking dependencies.
2. Extension contracts have no GPUI, PDFium, Wasmtime, storage-implementation,
   or unrestricted operating-system types.
3. PDFium remains owned according to its proven threading and lifetime rules.
4. UI components accept configuration and services and emit typed events; they
   do not reach into an application singleton.
5. Applications compose policy: windows, menus, file selection, packaging,
   permissions, default bundles, and implementation choice.
6. Extensions request semantic effects; the host validates and performs them.
7. Native and WebAssembly extensions share one semantic contract.
8. Untrusted extension UI is a bounded data tree rendered by trusted GPUI code.
9. No phase weakens license verification or distribution notices.
10. Each structural move is behavior-preserving unless a separately approved
    product change is called out and tested.
11. The repository remains buildable at every merged phase.
12. Removal or rollback of a new adapter must not corrupt document or extension
    state.

## Delivery principles

- Prefer extraction over rewriting.
- Separate moves from behavior changes.
- Create a crate only when it establishes a dependency rule or independently
  optional responsibility.
- Introduce interfaces at actual variation points, not around every function.
- Keep one repository and one lockfile while contracts are moving.
- Use external contract tests to prove a public crate is consumable without
  internal access.
- Measure hot paths before moving performance-critical behavior across a
  dynamic boundary.
- Start extension capabilities read-only and add authority deliberately.
- Pilot the smallest feature that can disprove each abstraction.
- Preserve a native path for trusted high-frequency rendering and interaction.

## Workstreams

The refactor contains four connected workstreams:

### A. Product and library separation

Move from a single binary crate to thin application composition over reusable
editor, PDF, UI, engine, storage, and network packages.

### B. Extension semantics

Define manifests, capabilities, events, effects, snapshots, lifecycle,
contributions, permissions, and compatibility independent of any execution
runtime.

### C. Installable execution

Add declarative packages and an optional sandboxed WebAssembly Component Model
adapter with strict validation and budgets.

### D. Feature migration

Prove the abstractions with theme, statistics, and TOC pilots, then decide which
existing features benefit from becoming first-party extensions.

These workstreams are ordered below to prevent the runtime from defining the
domain API and to prevent broad feature migration from freezing an unproven
contract.

## Phase 0: confer, baseline, and threat model

### Outcome

An agreed scope, a clean behavioral baseline, and explicit security assumptions
before package boundaries or extension APIs are frozen.

### Work

- Review all three architecture documents together.
- Resolve the Phase 0 decisions listed under `Approval gates`.
- Review the current uncommitted modularity changes independently.
- Record the current unit, license, release-build, and macOS E2E results.
- Catalogue public behaviors that must survive extraction: zoom and display
  changes, selection, annotations, comments, search, TOC, links, reference
  cards, themes, panel offsets, and persistence.
- Write the extension threat model: trusted app, trusted bundled native code,
  signed or local untrusted packages, malicious documents, hostile network
  responses, and compromised extension publishers.
- Define the first read-only capability boundary and data sensitivity labels.
- Decide what is allowed in official distributions under the license policy.

### Exit criteria

- The modularity baseline is a reviewed commit with a clean working tree aside
  from intentionally retained fixtures.
- All existing automated checks have recorded results.
- Threat actors, protected assets, trust boundaries, and out-of-scope risks are
  documented.
- The first capability set contains no write or mutation authority.
- The owner explicitly approves Phase 1.

### Rollback

Documentation only. No runtime behavior changes.

## Phase 1: introduce the virtual Cargo workspace

### Outcome

The existing reader builds from `apps/gpui-pdf-reader` inside one workspace,
with behavior and dependency versions unchanged.

### Work

- Add a virtual root workspace manifest.
- Move the current package to `apps/gpui-pdf-reader` mechanically.
- Centralize shared dependency versions, profiles, patches, and lints without
  changing resolved versions.
- Make scripts locate the workspace root rather than assume the old package
  directory.
- Add `xtask` only for repeated orchestration that cannot remain declarative.
- Preserve asset, PDFium, and application bundle discovery.

### Exit criteria

- Debug and release builds succeed from the workspace root.
- Existing unit and E2E suites pass without changed expected behavior.
- The packaged app finds PDFium, themes, and assets.
- The lockfile and permissive license audit cover the complete workspace.
- A revert of the phase restores the previous layout without data migration.

## Phase 2: extract the Markdown editor boundary

### Outcome

Markdown state and commands are UI-independent, while the GPUI editor is a
reusable configured component.

### Work

- Create `key-editor-core` from the pure editor model, parsing,
  serialization, selection, formatting, and validation logic.
- Create external API tests that use it as a consumer would.
- Create `key-editor-gpui` for rendering, IME, slash commands, focus, and
  formatting controls.
- Replace PDF-comment constants with `MarkdownEditorConfig`.
- Replace direct reader mutation with `MarkdownEditorEvent`.
- Keep comment-specific persistence and panel workflow outside the editor.

### Exit criteria

- `key-editor-core` compiles and tests without GPUI or macOS.
- A component test can construct the editor with custom commands and limits.
- Existing comment creation, newlines, lists, slash commands, autosave, and
  selection behavior pass.
- The standalone app remains the owner of comment workflow and storage.

## Phase 3: extract PDF domain and storage boundaries

### Outcome

PDF concepts and annotation behavior are independent of the engine, UI, and
sidecar representation.

### Work

- Create `key-pdf-core` for geometry, layouts, text layers, spatial indexes,
  selection, search, annotations, links, outline models, jump targets, and
  scientific-document detection.
- Define `AnnotationStore` and document identity semantics.
- Create `key-sidecar-store` for the current adjacent JSON implementation.
- Remove path construction and JSON I/O from domain types.
- Provide deterministic in-memory test stores.

### Exit criteria

- `key-pdf-core` has no GPUI, PDFium, reqwest, `open`, or sidecar-path imports.
- Search, link resolution, reference range detection, annotation migration,
  and navigation target tests pass in pure Rust.
- Sidecar compatibility tests load and save current persisted documents.
- Failed or interrupted saves are atomic and do not lose the prior valid state.

## Phase 4: extract PDF runtime and PDFium adapter

### Outcome

Document sessions, cancellation, cache policy, and demand coordination are
engine-independent; PDFium is a replaceable implementation with explicit
lifetime rules.

### Work

- Define the minimum `PdfEngine` interface from current call sites.
- Create `key-pdf-runtime` for session generation, work scheduling,
  cancellation, render and text demand, typed results, and cache policy.
- Create `key-pdfium` for bindings, document/page lifetimes, extraction,
  destinations, outlines, rendering, and supported mutations.
- Inject PDFium library location and configuration from the app.
- Add a mock engine and contract suite shared by implementations.
- Preserve the latest-wins zoom strategy and screen-change safeguards.

### Exit criteria

- Runtime tests pass against the mock engine without PDFium.
- The PDFium adapter passes the shared engine contract tests.
- No raw PDFium object crosses the adapter boundary.
- Rapid zoom, multi-page visibility changes, document close during work, and
  display transitions pass existing regression coverage.
- Render throughput and interaction latency do not regress beyond an agreed
  measurement tolerance.

## Phase 5: extract shared GPUI primitives and the PDF component

### Outcome

The app shell composes reusable, themed GPUI components instead of owning a
monolithic reader view.

### Work

- Create `key-ui-gpui` for theme tokens, icons, close buttons, panel shells,
  headers, empty states, focus helpers, animation values, and generic inputs.
- Create `key-pdf-gpui` for the viewport, text layer, overlays, and reader
  components.
- Keep menus, file dialogs, global actions, product navigation, and extension
  policy in the application.
- Give the reader a configuration/service input and typed event output.
- Move search, comments, TOC, references, and toolbars into independently
  testable feature components without yet making them installable.

### Exit criteria

- A harness can embed the PDF component without the app's main window type.
- Classic and Fluid views retain feature parity.
- Shared controls no longer have feature-specific dependencies.
- Panel layout, z-order, focus, text selection, animations, and accessibility
  behavior pass component and macOS E2E checks.

## Phase 6: freeze extension semantics without execution

### Outcome

A runtime-neutral, versioned extension contract exists before Wasm or package
execution is introduced.

### Work

- Create `key-extension-api`.
- Define the manifest, IDs, versions, dependency ranges, permissions,
  contribution declarations, settings schemas, limits, and structured errors.
- Define lifecycle, immutable snapshots, subscribed events, requested effects,
  cause IDs, tasks, and cancellation.
- Define capability discovery and required/optional negotiation.
- Define the bounded declarative UI schema and typed contribution slots.
- Write the initial WIT packages mirroring the Rust semantics.
- Implement static package and manifest validation only; do not execute code.
- Document versioning and deprecation rules.

### Exit criteria

- Rust and WIT contracts describe the same semantics with mapping tests.
- Invalid IDs, versions, dependencies, capabilities, UI trees, and package
  paths fail with stable structured diagnostics.
- Dependency and declarative composition cycles are rejected.
- The contract contains no GPUI, PDFium, Wasmtime, raw path, or socket types.
- Security review approves the read-only v1 surface before activation exists.

## Phase 7: implement the host and native pilot

### Outcome

The application can run one trusted native feature through extension lifecycle,
event, effect, capability, and contribution arbitration.

### Work

- Create `key-extension-host`.
- Implement activation, subscriptions, event scheduling, effect validation,
  capability resolution, namespaced storage routing, diagnostics, suspension,
  unload, and safe mode.
- Add loop controls: cause chains, delayed self-events, dispatch depth, batch
  limits, coalescing, retry bounds, and cancellation on generation change.
- Add a native adapter that uses the public semantic contract.
- Route a low-risk existing feature through it without changing its appearance.

### Pilot selection

Use a read-only or UI-only feature with real state and commands but no hot
rendering path. Do not use tiling, scroll handling, text selection painting, or
document mutation as the first pilot.

### Exit criteria

- Enabling, disabling, reloading, and failing the pilot cannot prevent app
  startup or document close.
- Permission denial and missing optional capabilities are visible and safe.
- Feedback-loop, cancellation, stale-handle, and quota tests pass.
- Safe mode starts with nonessential extensions disabled.
- Native code receives no special semantic authority merely because it is
  native.

## Phase 8: declarative UI and theme-pack pilot

### Outcome

An installable package with no executable code can contribute bounded,
host-rendered UI.

### Work

- Create `key-extension-gpui`.
- Render the approved UI nodes through `key-ui-gpui` theme tokens and icons.
- Implement typed slots, focus arbitration, panel ownership, overlay priority,
  clipping, accessibility, and bounded patches.
- Implement a declarative reference theme pack or view preset.
- Validate package assets, decoded dimensions, nesting, node counts, strings,
  and bindings.

### Exit criteria

- The package installs, previews permissions, enables, disables, and removes.
- It cannot request arbitrary layout callbacks, CSS, native views, shaders, or
  global z-order.
- Oversized, deeply nested, cyclic, or invalid UI is rejected without affecting
  the app.
- Keyboard navigation, screen-reader labels, themes, and panel layout remain
  host-controlled.

## Phase 9: sandboxed WebAssembly pilot

### Outcome

An optional Component Model runtime executes a read-only third-party extension
under explicit permissions and budgets.

### Work

- Create `key-extension-wasm` behind an application feature.
- Prototype and select the runtime; do not encode it in public APIs.
- Link only granted WIT imports and grant no general WASI capabilities.
- Enforce component size, memory, table, resource, stack, fuel, epoch deadline,
  host-call, output, queue, and UI budgets.
- Move work off the GPUI paint path and coalesce high-frequency events.
- Build a document-statistics extension using document metadata/text and a side
  panel.
- Add package signing verification in development form if the final scheme has
  been selected; otherwise keep distribution explicitly local/development-only.

### Exit criteria

- Infinite loops, traps, memory growth, oversized values, host-call floods, and
  unload during work are contained to the extension.
- The app starts and opens documents after a malicious or broken extension.
- The minimal reader builds without the Wasm runtime dependency.
- The native and Wasm pilots pass the same semantic compatibility suite.
- A security review approves the runtime configuration before general package
  installation is enabled.

## Phase 10: PDF extension API and TOC pilot

### Outcome

PDF-specific read-only capabilities are proven with a real navigation and UI
feature before broader migration.

### Work

- Create `key-pdf-extension-api` and its WIT world.
- Expose document metadata, text, selection snapshots, navigation, overlays,
  and read-only outline access as independently grantable capabilities.
- Use opaque generation-scoped resources.
- Convert an alternate TOC/navigation experience into a pilot extension.
- Reuse the shared jump abstraction for target resolution, centering, and
  transient focus animation.

### Exit criteria

- The pilot supports real outlines, page-only destinations, text-position
  refinement, hover details, and navigation without engine handles.
- Closing or replacing a document invalidates all extension resources safely.
- Overlay and event rates remain bounded during scroll and zoom.
- Navigation remains smooth and does not block rendering.
- The capability surface is revised from pilot findings before being marked
  stable.

## Phase 11: migrate features one at a time

### Outcome

Selected features become independent first-party packages only where this
improves reuse, optionality, or third-party extensibility.

### Candidate order

1. Search UI and result navigation.
2. Comments workflow over the shared editor and annotation capabilities.
3. Link preview and reference-detail UI.
4. Scholarly detection and metadata resolution.
5. Optional toolbars, view presets, and alternative navigation.

Core viewport rendering, page tiling, scroll/zoom input, text-layer geometry,
and base selection should remain trusted native functionality unless profiling
and a concrete extension use case justify moving them.

### Per-feature process

- Record current behavior and performance.
- Identify the smallest missing capability rather than exporting internals.
- Add permission, cancellation, quota, and compatibility tests.
- Implement the package alongside the native path temporarily.
- Run both against shared behavioral fixtures.
- Switch the standard bundle only after parity.
- Remove the old path only when rollback is available through the prior release
  and persisted data remains compatible.

### Exit criteria

- Each migrated package can be omitted from a minimal build.
- Disabling one feature does not disable unrelated reader behavior.
- Standard-bundle E2E behavior remains equivalent.
- No feature-specific API has leaked into generic extension contracts without
  evidence of a second consumer.

## Phase 12: safe HTTP and scholarly reference extension

### Outcome

Scholarly metadata and website previews are optional, permissioned network
functionality rather than a networking dependency of the PDF core.

### Work

- Create `key-safe-http` with fixed-domain permissions, redirect and DNS
  validation, private-address denial, timeouts, response and content-type
  limits, cancellation, concurrency limits, and bounded caching.
- Move scientific detection primitives that are broadly useful into PDF core;
  keep provider orchestration in a `key-reference` extension.
- Implement OpenAlex and Semantic Scholar providers through the safe HTTP
  capability.
- Preserve per-document cache purge and cancellation semantics.
- Make network permission and source attribution explicit in UI.

### Exit criteria

- Notes-only and minimal-reader builds do not compile or ship scholarly HTTP.
- Redirect, SSRF, oversized response, malformed payload, timeout, provider
  fallback, partial multi-reference, and document-close tests pass.
- Permission revocation cancels work and prevents new requests.
- Existing reference cards, selectable details, OA links, DOI copying, and
  result grouping retain behavior.

## Phase 13: mutations and redaction design

### Outcome

A separately approved design determines whether any document-write capability
is safe enough to expose. This phase may conclude that it should remain native
or be deferred.

### Work

- Threat-model mutation authority, irreversible loss, signature invalidation,
  metadata leakage, undo, and save semantics.
- Verify PDFium's actual content-removal behavior using adversarial fixtures.
- Define capability granularity, user confirmation, transaction boundaries,
  Save As policy, cache invalidation, and audit records.
- Prototype redaction only after the design is approved.

### Exit criteria

- A black visual overlay is never represented as secure redaction.
- Tests confirm removed text and graphics cannot be extracted or recovered from
  the produced file within the documented threat model.
- Failure is atomic and leaves the original document intact.
- Mutation permission is visibly distinct from read and annotation permission.
- The owner separately approves release of mutation capabilities.

## Phase 14: compose the future Key application

### Outcome

A second thin application proves the packages are reusable without weakening
the standalone reader.

### Work

- Add the Key app only when its own PRD defines notes, files, navigation, and
  persistence.
- Inject database-backed annotation and extension storage.
- Compose editor, PDF, and application navigation packages.
- Select a Key-specific default extension bundle and permission policy.
- Keep PDFium and scholarly networking out of notes-only configurations.

### Exit criteria

- Both apps build and test independently from the same workspace.
- Key imports no standalone reader shell code.
- The reader imports no Key product code.
- Each app can evolve its storage and bundled features through public contracts.

## Approval gates

### Gate A: before Phase 1

Agree:

- Monorepo workspace first; no submodules or repo split.
- Whether the current uncommitted modularity changes form the baseline.
- Supported macOS baseline and what cross-platform means during refactoring.
- Behavioral and performance regression tolerances.
- Threat model and read-only first capability boundary.

### Gate B: before freezing Phase 6

Agree:

- `.keyext` archive and canonicalization format.
- Manifest serialization and extension ID rules.
- Declarative UI serialization and patch model.
- Stable v1 lifecycle, event, effect, and error semantics.
- Official-bundle versus local-install license policy.
- Capability and permission vocabulary.

### Gate C: before Phase 9

Agree:

- Wasmtime or another Component Model runtime based on a focused prototype.
- Supported Component Model/WASI versions.
- Exact default budgets and suspension policy.
- Signature, publisher trust, revocation, and local-development policy.
- Whether installation requires restart in the first release.

### Gate D: before network extensions

Agree:

- Domain approval UX and per-document versus global grants.
- Cache location, privacy, expiry, and purge guarantees.
- Attribution, telemetry, and provider terms.

### Gate E: before any mutation capability

Agree:

- Whether mutations belong in the first public extension API at all.
- Save, undo, confirmation, audit, and recovery model.
- Security evidence required to call a feature redaction.

## Test strategy

### Per-crate tests

- Pure unit tests beside domain code.
- External API tests that depend only on public exports.
- Compile-fail or dependency checks for forbidden imports.
- Property tests for geometry, positions, ranges, parsers, manifests, and state
  transitions where useful.

### Contract tests

- Every `PdfEngine` implementation runs the same suite.
- Every `AnnotationStore` implementation runs the same suite.
- Native and Wasm extension adapters run the same lifecycle/event/effect suite.
- Old manifest, WIT, and persisted-state fixtures remain compatibility tests.

### Security and robustness tests

- Malformed archives, traversal, decompression bombs, invalid signatures, and
  oversized assets.
- Dependency cycles, missing capabilities, permission denial, stale resources,
  and invalid effects.
- Infinite compute, traps, memory growth, queue floods, excessive host calls,
  oversized WIT values, and event feedback loops.
- UI depth, node, patch, string, list, image, geometry, and animation limits.
- HTTP redirect, DNS rebinding/private address, body, timeout, rate, and cache
  limits.
- Upgrade and migration rollback.

### Product tests

- Minimal standalone reader.
- Standard standalone reader.
- Standard reader with a reference third-party package.
- Safe mode with all third-party extensions disabled.
- Future Key app and notes-only configuration.
- Native macOS E2E for Classic and Fluid views, zoom, display changes, themes,
  search, comments, TOC, links, and scholarly references.

### Quality gates for every phase

- Formatting and Clippy with warnings denied.
- All workspace tests.
- Strict license audit across normal, build, dev, native, and tool dependencies.
- Vendor tile regression.
- Relevant native E2E scenarios.
- Release build and package smoke test when packaging paths change.
- `git diff --check` and an intentional review of moved versus changed lines.

## Performance budgets

Exact numbers must be recorded in Phase 0, but the following categories are
release gates:

- Input-to-scroll and input-to-zoom responsiveness.
- Frame pacing during continuous scroll and rapid zoom.
- Time to first visible page and first selectable text.
- Render cancellation latency after a new zoom generation.
- Peak resident memory for representative large documents.
- Extension event dispatch and UI-patch cost.
- App startup with the standard extension bundle.

No dynamic extension callback should run synchronously in a GPUI paint or raw
pointer/scroll hot path. Coalesced state events and host-owned rendering remain
the default.

## Data and migration policy

- Preserve the current adjacent annotation JSON format until the store boundary
  has compatibility tests.
- Add schema versions before extensions can own persisted document state.
- Namespace extension settings, document data, and cache separately.
- Use atomic writes or transactions.
- Snapshot or transactionally migrate extension data on upgrade.
- Failed upgrades restore the last valid package and state.
- Document close invalidates handles, cancels tasks, and purges declared
  ephemeral caches.
- Uninstall must state whether user data is retained or removed and require an
  explicit choice for destructive deletion.

## Packaging and licensing

- The workspace retains one lockfile and centralized patches initially.
- Wasm runtime, scholarly networking, and optional packages are feature- or
  bundle-selectable.
- Each distributable generates notices from the dependencies it actually ships.
- Official packages must satisfy the project's MIT, Apache, or more-permissive
  policy unless the owner explicitly revises that policy.
- The extension installer displays package license and requested permissions.
- The policy for locally installed third-party licenses remains a Gate B
  decision and must not accidentally change official distribution contents.

## Repository and release policy

Keep all crates, WIT, reference extensions, fixtures, and apps in this repository
while APIs change. Split a package into its own repository only when it has:

- A stable public API.
- At least one independent consumer.
- Clear ownership.
- An independent release cadence.
- Contract tests runnable outside this repository.
- A versioning and security-support policy.

If split later, consume semantic versions or pinned release tags. Do not use Git
submodules as the default package manager.

## Risks and mitigations

### Too many crates too early

Mitigation: create crates only when a dependency rule or optional shipping
boundary is testable; otherwise keep an internal module.

### Public API frozen around current implementation

Mitigation: define contracts from multiple pilots, keep them pre-stable, and
avoid engine/UI types.

### Extension architecture delays product work

Mitigation: preserve native built-ins, deliver pilots incrementally, and do not
require all features to migrate.

### Wasm overhead harms interaction

Mitigation: keep hot rendering/input native, use coalesced snapshots and bounded
patches, measure before moving a boundary.

### Sandbox creates false confidence

Mitigation: capability minimization, explicit permissions, bounded host APIs,
publisher trust, safe mode, and security tests in addition to Wasm isolation.

### App and extension UI diverge

Mitigation: both use host theme tokens and trusted `key-ui-gpui` components;
extensions contribute data, not arbitrary rendering code.

### Storage becomes tied to one product

Mitigation: semantic store interfaces with sidecar and future database adapters,
plus shared compatibility tests.

### Dependency and license growth

Mitigation: optional runtime/network crates, bundle-specific notices, explicit
SPDX allow policy, and audits before every merge.

### Large refactors hide regressions

Mitigation: mechanical moves separate from behavior changes, phase-sized PRs,
recorded baselines, and diff review that distinguishes moved and edited code.

## Definition of complete

The architectural refactor is complete only when:

- The standalone reader and future consumer can compose public crates without
  importing each other's app shells.
- Core packages compile without platform UI and implementation dependencies.
- Minimal and standard reader bundles build and pass their test matrices.
- The current reader experience retains its behavior and measured performance.
- Declarative and Wasm reference extensions can be safely installed, validated,
  permissioned, enabled, disabled, upgraded, and removed.
- Safe mode recovers from broken third-party packages.
- Native and Wasm adapters pass the same semantic contract tests.
- Public APIs have compatibility, deprecation, and security-support policies.
- Storage migrations and package upgrades can roll back without data loss.
- Licenses and notices are correct for every shipped bundle.

## Questions for our architecture conference

Before implementation, we should discuss these in order:

1. Do we agree that one monorepo workspace is the product boundary for now,
   with independent repositories only after proven external reuse?
2. Which current features are essential native reader capabilities, and which
   are candidates for first-party extensions?
3. Should comments be part of the base reader bundle but implemented through
   extension semantics, or remain a direct PDF component feature initially?
4. Is the first extension API strictly read-only plus navigation/overlays, or
   should annotation creation be included?
5. Which native pilot best tests the host without touching a hot path?
6. Are theme packs sufficient for the first declarative pilot, or should the
   pilot include a small settings/panel contribution too?
7. What local-install license policy should differ, if at all, from the official
   permissive-only distribution policy?
8. Should third-party installation require an app restart in v1, while enable,
   disable, and failure recovery remain live?
9. What permission granularity should users see: per extension, per document,
   per domain, or a combination?
10. What concrete performance regressions are acceptable during extraction?
11. Which runtime prototype criteria decide Wasmtime versus an alternative?
12. Do we explicitly defer all PDF mutation capabilities until after the
    read-only extension system ships?

No Phase 1 edits should begin until at least questions 1, 2, 4, 5, 7, 10, and
12 have agreed answers and the owner explicitly says to proceed.
